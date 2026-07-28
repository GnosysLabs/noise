use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, anyhow, bail};
use futures_util::{StreamExt, stream};
use noise_migration_verifier::{
    LegacyPayloadEncoding, SourceInput, normalize_legacy_media_payload, verify,
};
use object_store::{
    Error as ObjectStoreError, ObjectStore, ObjectStoreExt, PutMode, PutOptions,
    aws::AmazonS3Builder, local::LocalFileSystem, path::Path as ObjectPath,
};
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use tokio_postgres::{NoTls, Transaction};
use url::Url;

const MAX_UPLOAD_CONCURRENCY: usize = 16;

#[derive(Clone)]
pub struct ImportSource {
    pub input: SourceInput,
    pub compatibility_origin: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ImportSummary {
    pub status: &'static str,
    pub providers: u64,
    pub canonical_objects: u64,
    pub canonical_storage_bytes: u64,
    pub live_legacy_aliases: u64,
    pub deleted_legacy_aliases: u64,
    pub legacy_json_aliases: u64,
}

pub struct ImportPlan {
    pub summary: ImportSummary,
    objects: Vec<ObjectPlan>,
    aliases: Vec<AliasPlan>,
    providers: BTreeMap<String, String>,
}

#[derive(Clone)]
struct ObjectPlan {
    object_id: String,
    storage_key: String,
    storage_hash: String,
    storage_length: u64,
    source_path: PathBuf,
}

struct AliasPlan {
    provider: String,
    shard_id: Vec<u8>,
    state: &'static str,
    payload_hash: Option<Vec<u8>>,
    ciphertext_length: Option<i64>,
    canonical_object_id: Option<Vec<u8>>,
    payload_encoding: Option<&'static str>,
    deletion_capability_hash: Option<Vec<u8>>,
}

pub fn build_plan(sources: Vec<ImportSource>, primary_source: &str) -> anyhow::Result<ImportPlan> {
    let verifier_inputs = sources
        .iter()
        .map(|source| source.input.clone())
        .collect::<Vec<_>>();
    let verification = verify(verifier_inputs, primary_source)?;
    if verification.status != "pass" {
        bail!("migration verifier did not pass");
    }

    let mut providers = BTreeMap::new();
    let mut objects = BTreeMap::<String, ObjectPlan>::new();
    let mut aliases = Vec::new();
    let mut legacy_json_aliases = 0_u64;
    let mut deleted_aliases = 0_u64;

    for source in &sources {
        let origin = canonical_compatibility_origin(&source.compatibility_origin)?;
        if providers
            .insert(source.input.label.clone(), origin)
            .is_some()
        {
            bail!("import source labels must be unique");
        }
        let database = open_snapshot(&source.input.database_path)?;
        let tombstones = read_id_set(&database, "relay_shard_tombstones")?;
        let mut statement = database.prepare(
            "SELECT shard_id, payload_hash, delete_token_hash, byte_length
             FROM relay_shards
             ORDER BY shard_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (shard_id, payload_hash, delete_token_hash, byte_length) = row?;
            let decoded_shard_id = decode_hex_32(&shard_id)?;
            if tombstones.contains(&shard_id) {
                bail!("live shard is also tombstoned");
            }
            let source_path = source
                .input
                .media_root
                .join("shards")
                .join(&shard_id[..2])
                .join(format!("{shard_id}.bin"));
            let payload = fs::read(&source_path).with_context(|| {
                format!(
                    "verified media file for source {} is unavailable",
                    source.input.label
                )
            })?;
            if payload.len() as i64 != byte_length
                || blake3::hash(&payload).to_hex().as_str() != payload_hash
            {
                bail!("media changed after verification");
            }
            let normalized = normalize_legacy_media_payload(&payload)?;
            let storage_hash = blake3::hash(&normalized.storage_bytes).to_hex().to_string();
            let storage_length = normalized.storage_bytes.len() as u64;
            let storage_key = canonical_storage_key(&normalized.object_id)?;
            let candidate = ObjectPlan {
                object_id: normalized.object_id.clone(),
                storage_key,
                storage_hash: storage_hash.clone(),
                storage_length,
                source_path: source_path.clone(),
            };
            if let Some(existing) = objects.get_mut(&normalized.object_id) {
                if existing.storage_hash != storage_hash
                    || existing.storage_length != storage_length
                {
                    bail!("canonical media object has conflicting normalized bytes");
                }
                if normalized.source_encoding == LegacyPayloadEncoding::Nsb2 {
                    existing.source_path = source_path;
                }
            } else {
                objects.insert(normalized.object_id.clone(), candidate);
            }
            let payload_encoding = match normalized.source_encoding {
                LegacyPayloadEncoding::Nsb2 => "nsb2",
                LegacyPayloadEncoding::LegacyJson => {
                    legacy_json_aliases += 1;
                    "legacy_json"
                }
            };
            aliases.push(AliasPlan {
                provider: source.input.label.clone(),
                shard_id: decoded_shard_id,
                state: "live",
                payload_hash: Some(decode_hex_32(&payload_hash)?),
                ciphertext_length: Some(byte_length),
                canonical_object_id: Some(decode_hex_32(&normalized.object_id)?),
                payload_encoding: Some(payload_encoding),
                deletion_capability_hash: Some(decode_hex_32(&delete_token_hash)?),
            });
        }
        drop(statement);
        for shard_id in tombstones {
            deleted_aliases += 1;
            aliases.push(AliasPlan {
                provider: source.input.label.clone(),
                shard_id: decode_hex_32(&shard_id)?,
                state: "deleted",
                payload_hash: None,
                ciphertext_length: None,
                canonical_object_id: None,
                payload_encoding: None,
                deletion_capability_hash: None,
            });
        }
    }

    let canonical_storage_bytes = objects.values().map(|object| object.storage_length).sum();
    let summary = ImportSummary {
        status: "ready",
        providers: providers.len() as u64,
        canonical_objects: objects.len() as u64,
        canonical_storage_bytes,
        live_legacy_aliases: aliases.iter().filter(|alias| alias.state == "live").count() as u64,
        deleted_legacy_aliases: deleted_aliases,
        legacy_json_aliases,
    };
    if summary.canonical_objects != verification.media.canonical_encrypted_objects
        || summary.canonical_storage_bytes != verification.media.canonical_storage_bytes
        || summary.live_legacy_aliases != verification.media.provider_scoped_live_shards
        || summary.deleted_legacy_aliases != verification.media.provider_scoped_tombstones
    {
        bail!("import plan does not reproduce the passing verifier report");
    }

    Ok(ImportPlan {
        summary,
        objects: objects.into_values().collect(),
        aliases,
        providers,
    })
}

pub async fn execute_plan(
    plan: &ImportPlan,
    store: Arc<dyn ObjectStore>,
    database_url: &str,
    upload_concurrency: usize,
) -> anyhow::Result<ImportSummary> {
    if upload_concurrency == 0 || upload_concurrency > MAX_UPLOAD_CONCURRENCY {
        bail!("upload concurrency must be between 1 and {MAX_UPLOAD_CONCURRENCY}");
    }
    upload_plan_objects(plan, store, upload_concurrency).await?;
    write_database(plan, database_url).await?;
    let mut summary = plan.summary.clone();
    summary.status = "imported";
    Ok(summary)
}

pub fn local_object_store(root: &Path) -> anyhow::Result<Arc<dyn ObjectStore>> {
    fs::create_dir_all(root).context("could not create local migration object store")?;
    Ok(Arc::new(LocalFileSystem::new_with_prefix(root).context(
        "could not initialize local migration object store",
    )?))
}

pub fn r2_object_store_from_env() -> anyhow::Result<Arc<dyn ObjectStore>> {
    let endpoint = required_secret_env("NOISE_R2_ENDPOINT")?;
    validate_r2_endpoint(&endpoint)?;
    let bucket = required_secret_env("NOISE_R2_BUCKET")?;
    if !(3..=63).contains(&bucket.len())
        || !bucket
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !bucket
            .as_bytes()
            .last()
            .is_some_and(u8::is_ascii_alphanumeric)
        || !bucket
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        bail!("NOISE_R2_BUCKET is invalid");
    }
    let access_key = required_secret_env("NOISE_R2_ACCESS_KEY_ID")?;
    let secret_key = required_secret_env("NOISE_R2_SECRET_ACCESS_KEY")?;
    let store = AmazonS3Builder::new()
        .with_endpoint(endpoint)
        .with_region("auto")
        .with_bucket_name(bucket)
        .with_access_key_id(access_key)
        .with_secret_access_key(secret_key)
        .with_virtual_hosted_style_request(false)
        .with_disable_bulk_delete(true)
        .build()
        .context("could not initialize private R2 object store")?;
    Ok(Arc::new(store))
}

pub async fn upload_plan_objects(
    plan: &ImportPlan,
    store: Arc<dyn ObjectStore>,
    concurrency: usize,
) -> anyhow::Result<()> {
    let completed = Arc::new(AtomicU64::new(0));
    let total = plan.objects.len() as u64;
    let mut uploads = stream::iter(plan.objects.iter().cloned())
        .map(|object| {
            let store = store.clone();
            let completed = completed.clone();
            async move {
                upload_object(store.as_ref(), &object).await?;
                let current = completed.fetch_add(1, Ordering::Relaxed) + 1;
                if current == total || current % 100 == 0 {
                    eprintln!("verified {current} of {total} encrypted R2 objects");
                }
                Ok::<(), anyhow::Error>(())
            }
        })
        .buffer_unordered(concurrency);
    while let Some(result) = uploads.next().await {
        result?;
    }
    Ok(())
}

async fn upload_object(store: &dyn ObjectStore, object: &ObjectPlan) -> anyhow::Result<()> {
    let source = fs::read(&object.source_path).context("migration media source disappeared")?;
    let normalized = normalize_legacy_media_payload(&source)?;
    if normalized.object_id != object.object_id
        || normalized.storage_bytes.len() as u64 != object.storage_length
        || blake3::hash(&normalized.storage_bytes).to_hex().as_str() != object.storage_hash
    {
        bail!("migration media source changed after planning");
    }
    let location =
        ObjectPath::parse(&object.storage_key).context("canonical R2 object key is invalid")?;
    let result = store
        .put_opts(
            &location,
            normalized.storage_bytes.clone().into(),
            PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            },
        )
        .await;
    match result {
        Ok(_) | Err(ObjectStoreError::AlreadyExists { .. }) => {}
        Err(error) => return Err(error).context("could not upload canonical encrypted object"),
    }
    let stored = store
        .get(&location)
        .await
        .context("could not read back canonical encrypted object")?
        .bytes()
        .await
        .context("could not verify canonical encrypted object")?;
    if stored.len() as u64 != object.storage_length
        || blake3::hash(&stored).to_hex().as_str() != object.storage_hash
    {
        bail!("stored canonical encrypted object failed read-back verification");
    }
    Ok(())
}

async fn write_database(plan: &ImportPlan, database_url: &str) -> anyhow::Result<()> {
    let config =
        tokio_postgres::Config::from_str(database_url).context("NOISE_DATABASE_URL is invalid")?;
    let (mut client, connection) = config
        .connect(NoTls)
        .await
        .context("could not connect to canonical PostgreSQL")?;
    let connection_task = tokio::spawn(async move { connection.await });
    let transaction = client
        .transaction()
        .await
        .context("could not begin media import transaction")?;
    let schema_version: i32 = transaction
        .query_one(
            "SELECT version FROM noise.schema_migrations ORDER BY version DESC LIMIT 1",
            &[],
        )
        .await
        .context("canonical schema is unavailable")?
        .get(0);
    if schema_version != 2 {
        bail!("canonical schema version {schema_version} is unsupported");
    }

    let mut provider_ids = BTreeMap::new();
    for (name, origin) in &plan.providers {
        let row = transaction
            .query_opt(
                "INSERT INTO noise.legacy_media_providers (
                    provider_name, compatibility_origin
                 ) VALUES ($1, $2)
                 ON CONFLICT (provider_name) DO UPDATE
                 SET provider_name = EXCLUDED.provider_name
                 WHERE noise.legacy_media_providers.compatibility_origin
                     = EXCLUDED.compatibility_origin
                 RETURNING provider_id",
                &[name, origin],
            )
            .await
            .context("could not reconcile legacy media provider")?
            .ok_or_else(|| anyhow!("legacy media provider conflicts with existing data"))?;
        provider_ids.insert(name.as_str(), row.get::<_, i16>(0));
    }
    for object in &plan.objects {
        insert_object(&transaction, object).await?;
    }
    for alias in &plan.aliases {
        let provider_id = provider_ids
            .get(alias.provider.as_str())
            .copied()
            .context("legacy media provider is missing")?;
        insert_alias(&transaction, provider_id, alias).await?;
    }
    transaction
        .commit()
        .await
        .context("could not commit media import")?;
    drop(client);
    connection_task
        .await
        .context("PostgreSQL connection task failed")?
        .context("PostgreSQL connection failed")?;
    Ok(())
}

async fn insert_object(transaction: &Transaction<'_>, object: &ObjectPlan) -> anyhow::Result<()> {
    let object_id = decode_hex_32(&object.object_id)?;
    let payload_hash = decode_hex_32(&object.storage_hash)?;
    let storage_length = i64::try_from(object.storage_length)?;
    let inserted = transaction
        .query_opt(
            "INSERT INTO noise.legacy_media_objects (
                media_object_id, storage_key, storage_byte_length,
                storage_payload_hash, state
             ) VALUES ($1, $2, $3, $4, 'available')
             ON CONFLICT (media_object_id) DO UPDATE
             SET imported_at = noise.legacy_media_objects.imported_at
             WHERE noise.legacy_media_objects.storage_key = EXCLUDED.storage_key
               AND noise.legacy_media_objects.storage_byte_length
                   = EXCLUDED.storage_byte_length
               AND noise.legacy_media_objects.storage_payload_hash
                   = EXCLUDED.storage_payload_hash
               AND noise.legacy_media_objects.state = 'available'
             RETURNING 1",
            &[
                &object_id,
                &object.storage_key,
                &storage_length,
                &payload_hash,
            ],
        )
        .await
        .context("could not import canonical encrypted media object")?;
    if inserted.is_none() {
        bail!("canonical encrypted media object conflicts with existing data");
    }
    Ok(())
}

async fn insert_alias(
    transaction: &Transaction<'_>,
    provider_id: i16,
    alias: &AliasPlan,
) -> anyhow::Result<()> {
    let inserted = transaction
        .query_opt(
            "INSERT INTO noise.legacy_media_shards (
                provider_id, shard_id, state, payload_hash, ciphertext_length,
                canonical_object_id, payload_encoding,
                deletion_capability_hash, tombstoned_at
             ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NULL)
             ON CONFLICT (provider_id, shard_id) DO UPDATE
             SET accepted_at = noise.legacy_media_shards.accepted_at
             WHERE noise.legacy_media_shards.state = EXCLUDED.state
               AND noise.legacy_media_shards.payload_hash
                   IS NOT DISTINCT FROM EXCLUDED.payload_hash
               AND noise.legacy_media_shards.ciphertext_length
                   IS NOT DISTINCT FROM EXCLUDED.ciphertext_length
               AND noise.legacy_media_shards.canonical_object_id
                   IS NOT DISTINCT FROM EXCLUDED.canonical_object_id
               AND noise.legacy_media_shards.payload_encoding
                   IS NOT DISTINCT FROM EXCLUDED.payload_encoding
               AND noise.legacy_media_shards.deletion_capability_hash
                   IS NOT DISTINCT FROM EXCLUDED.deletion_capability_hash
             RETURNING 1",
            &[
                &provider_id,
                &alias.shard_id,
                &alias.state,
                &alias.payload_hash,
                &alias.ciphertext_length,
                &alias.canonical_object_id,
                &alias.payload_encoding,
                &alias.deletion_capability_hash,
            ],
        )
        .await
        .context("could not import legacy media alias")?;
    if inserted.is_none() {
        bail!("legacy media alias conflicts with existing data");
    }
    Ok(())
}

fn open_snapshot(path: &Path) -> anyhow::Result<Connection> {
    let uri = immutable_sqlite_uri(path)?;
    let connection = Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    Ok(connection)
}

fn read_id_set(connection: &Connection, table: &str) -> anyhow::Result<BTreeSet<String>> {
    let mut statement =
        connection.prepare(&format!("SELECT shard_id FROM {table} ORDER BY shard_id"))?;
    let values = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(values)
}

fn immutable_sqlite_uri(path: &Path) -> anyhow::Result<String> {
    let path = path
        .to_str()
        .context("SQLite snapshot path must be valid UTF-8")?;
    let mut uri = String::from("file:");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b':' | b'-' | b'_' | b'.' | b'~') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut uri, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    uri.push_str("?immutable=1");
    Ok(uri)
}

fn canonical_compatibility_origin(value: &str) -> anyhow::Result<String> {
    let parsed = Url::parse(value).context("compatibility origin is invalid")?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("compatibility origin must be an HTTPS origin without a path");
    }
    Ok(value.trim_end_matches('/').to_owned())
}

fn validate_r2_endpoint(value: &str) -> anyhow::Result<()> {
    let parsed = Url::parse(value).context("NOISE_R2_ENDPOINT is invalid")?;
    let host = parsed.host_str().unwrap_or_default();
    if parsed.scheme() != "https"
        || !host.ends_with(".r2.cloudflarestorage.com")
        || host == "r2.cloudflarestorage.com"
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("NOISE_R2_ENDPOINT must be an account-specific Cloudflare R2 HTTPS endpoint");
    }
    Ok(())
}

fn required_secret_env(name: &str) -> anyhow::Result<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is required"))
}

fn canonical_storage_key(object_id: &str) -> anyhow::Result<String> {
    decode_hex_32(object_id)?;
    Ok(format!(
        "legacy/objects/{}/{}.nsb2",
        &object_id[..2],
        object_id
    ))
}

fn decode_hex_32(value: &str) -> anyhow::Result<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("expected a canonical 32-byte hexadecimal identifier");
    }
    let mut output = Vec::with_capacity(32);
    for index in (0..64).step_by(2) {
        output.push(
            u8::from_str_radix(&value[index..index + 2], 16)
                .context("hexadecimal identifier is invalid")?,
        );
    }
    Ok(output)
}
