use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, bail};
use clap::{Parser, ValueEnum};
use noise_migration_importer::{
    ImportSource, build_plan, execute_database_plan, execute_plan, local_object_store,
    r2_object_store_from_env,
};
use noise_migration_verifier::SourceInput;

#[derive(Clone, Copy, ValueEnum)]
enum ObjectStoreKind {
    R2,
    Local,
}

#[derive(Parser)]
#[command(
    name = "noise-migration-importer",
    about = "Verified, resumable import of legacy encrypted noise media"
)]
struct Args {
    /// Snapshot in the form LABEL=/absolute/path/to/relay.db. Repeat per relay.
    #[arg(long = "source", required = true)]
    sources: Vec<String>,

    /// Relay data root in the form LABEL=/absolute/path. Repeat per relay.
    #[arg(long = "media-root", required = true)]
    media_roots: Vec<String>,

    /// Legacy HTTPS origin in the form LABEL=https://relay.example. Repeat per relay.
    #[arg(long = "compatibility-origin", required = true)]
    compatibility_origins: Vec<String>,

    /// Source whose push registrations become the later migration source.
    #[arg(long)]
    primary_source: String,

    /// Perform R2/local object writes and one PostgreSQL transaction.
    #[arg(long)]
    execute: bool,

    /// Object-store implementation used only with --execute.
    #[arg(long, value_enum, default_value = "r2")]
    object_store: ObjectStoreKind,

    /// Local object-store root for disposable execution tests.
    #[arg(long)]
    local_object_root: Option<PathBuf>,

    /// Canonical PostgreSQL URL. Prefer the NOISE_DATABASE_URL environment variable.
    #[arg(long, env = "NOISE_DATABASE_URL", hide_env_values = true)]
    database_url: Option<String>,

    /// Parallel object uploads and read-back checks.
    #[arg(long, default_value_t = 4)]
    upload_concurrency: usize,

    /// Reuse the already verified R2 import and write only PostgreSQL data.
    #[arg(long)]
    reuse_verified_objects: bool,

    /// Write the sanitized JSON result here instead of stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let databases = parse_labeled_paths(&args.sources, "source")?;
    let media_roots = parse_labeled_paths(&args.media_roots, "media root")?;
    let origins = parse_labeled_values(&args.compatibility_origins, "compatibility origin")?;
    if databases.len() < 2 {
        bail!("at least two relay snapshots are required");
    }
    if !databases.contains_key(&args.primary_source) {
        bail!("primary source does not match a configured source label");
    }
    if databases.keys().ne(media_roots.keys()) || databases.keys().ne(origins.keys()) {
        bail!("every source must have exactly one media root and compatibility origin");
    }
    let sources = databases
        .into_iter()
        .map(|(label, database_path)| ImportSource {
            compatibility_origin: origins
                .get(&label)
                .expect("matching origins were checked")
                .clone(),
            input: SourceInput {
                media_root: media_roots
                    .get(&label)
                    .expect("matching media roots were checked")
                    .clone(),
                label,
                database_path,
            },
        })
        .collect();
    let plan = build_plan(sources, &args.primary_source)?;
    let summary = if args.execute {
        let database_url = args
            .database_url
            .as_deref()
            .context("--execute requires NOISE_DATABASE_URL or --database-url")?;
        if args.reuse_verified_objects {
            if args.local_object_root.is_some() {
                bail!("--reuse-verified-objects cannot use --local-object-root");
            }
            execute_database_plan(&plan, database_url).await?
        } else {
            let store = match args.object_store {
                ObjectStoreKind::R2 => {
                    if args.local_object_root.is_some() {
                        bail!("--local-object-root cannot be used with the R2 object store");
                    }
                    r2_object_store_from_env()?
                }
                ObjectStoreKind::Local => {
                    let root = args
                        .local_object_root
                        .as_deref()
                        .context("local execution requires --local-object-root")?;
                    local_object_store(root)?
                }
            };
            execute_plan(&plan, store, database_url, args.upload_concurrency).await?
        }
    } else {
        if args.database_url.is_some()
            || args.local_object_root.is_some()
            || args.reuse_verified_objects
        {
            bail!("database and object-store destinations require --execute");
        }
        plan.summary.clone()
    };
    let encoded = serde_json::to_vec_pretty(&summary)?;
    if let Some(output) = args.output {
        std::fs::write(&output, &encoded)
            .with_context(|| format!("could not write report {}", output.display()))?;
    } else {
        println!("{}", String::from_utf8(encoded).expect("JSON is UTF-8"));
    }
    Ok(())
}

fn parse_labeled_paths(
    values: &[String],
    description: &str,
) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let parsed = parse_labeled_values(values, description)?;
    parsed
        .into_iter()
        .map(|(label, value)| {
            let path = PathBuf::from(value);
            if !path.is_absolute() {
                bail!("{description} path must be absolute");
            }
            Ok((label, path))
        })
        .collect()
}

fn parse_labeled_values(
    values: &[String],
    description: &str,
) -> anyhow::Result<BTreeMap<String, String>> {
    let mut parsed = BTreeMap::new();
    for value in values {
        let (label, value) = value
            .split_once('=')
            .with_context(|| format!("{description} must use LABEL=VALUE"))?;
        if label.is_empty()
            || label.len() > 64
            || !label.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
        {
            bail!("{description} has an invalid label");
        }
        if value.is_empty() {
            bail!("{description} value cannot be empty");
        }
        if parsed.insert(label.to_owned(), value.to_owned()).is_some() {
            bail!("{description} label was provided more than once");
        }
    }
    Ok(parsed)
}
