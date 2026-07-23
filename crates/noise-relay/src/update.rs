use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use noise_transport::RELAY_PROTOCOL_VERSION;
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::time::sleep;

pub const DEFAULT_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/GnosysLabs/noise/main/deploy/relay-channels/stable.json";
const RELEASE_PUBLIC_KEY_BASE64: &str = "H/ZLZBbbg0pmV4sY7nkzlf2GB9yo4Mc3EM1+jQBDKhA=";
const MANIFEST_SCHEMA: u32 = 1;
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
const MAX_PACKAGE_BYTES: usize = 128 * 1024 * 1024;
const UPDATE_DIRECTORY: &str = "/var/lib/noise-relay/updates";
const INSTALLED_BINARY: &str = "/usr/bin/noise-relay";
const INSTALLED_UNITS: &[&str] = &[
    "/lib/systemd/system/noise-relay.service",
    "/lib/systemd/system/noise-relay-update.service",
    "/lib/systemd/system/noise-relay-update.timer",
];

#[derive(Clone, Debug)]
pub struct UpdateOptions {
    pub manifest_url: String,
    pub signature_url: Option<String>,
    pub apply: bool,
    pub health_url: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseManifest {
    pub schema: u32,
    pub channel: String,
    pub version: String,
    pub protocol_min: u16,
    pub protocol_max: u16,
    pub published_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
    pub assets: Vec<ReleaseAsset>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseAsset {
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub byte_length: u64,
}

#[derive(Debug, Serialize)]
pub struct UpdateStatus {
    pub current_version: String,
    pub latest_version: String,
    pub protocol_version: u16,
    pub channel: String,
    pub update_available: bool,
    pub target: String,
}

pub async fn run(options: UpdateOptions) -> anyhow::Result<UpdateStatus> {
    let (manifest, asset) = fetch_release(&options).await?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("the running relay has an invalid package version")?;
    let latest = Version::parse(&manifest.version).context("release version is invalid")?;
    if latest < current {
        bail!(
            "refusing relay downgrade from {} to {}",
            current,
            manifest.version
        )
    }
    let status = UpdateStatus {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        protocol_version: RELAY_PROTOCOL_VERSION,
        channel: manifest.channel.clone(),
        update_available: latest > current,
        target: asset.target.clone(),
    };
    if options.apply && status.update_available {
        apply_release(&manifest, &asset, &current.to_string(), &options.health_url).await?;
    }
    Ok(status)
}

async fn fetch_release(options: &UpdateOptions) -> anyhow::Result<(ReleaseManifest, ReleaseAsset)> {
    require_https(&options.manifest_url, "release manifest")?;
    let signature_url = options
        .signature_url
        .clone()
        .unwrap_or_else(|| format!("{}.sig", options.manifest_url));
    require_https(&signature_url, "release signature")?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(30))
        .build()
        .context("could not initialize relay updater")?;
    let manifest_bytes = fetch_limited(&client, &options.manifest_url, MAX_MANIFEST_BYTES).await?;
    let signature_bytes = fetch_limited(&client, &signature_url, MAX_SIGNATURE_BYTES).await?;
    verify_manifest_signature(&manifest_bytes, &signature_bytes)?;
    let manifest = serde_json::from_slice::<ReleaseManifest>(&manifest_bytes)
        .context("signed relay release manifest is invalid JSON")?;
    validate_manifest(&manifest)?;
    let target = platform_target()?;
    let asset = manifest
        .assets
        .iter()
        .find(|asset| asset.target == target)
        .cloned()
        .with_context(|| format!("release has no package for {target}"))?;
    Ok((manifest, asset))
}

async fn fetch_limited(
    client: &reqwest::Client,
    url: &str,
    maximum_bytes: usize,
) -> anyhow::Result<Vec<u8>> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("could not download {url}"))?
        .error_for_status()
        .with_context(|| format!("release server rejected {url}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes as u64)
    {
        bail!("download from {url} is larger than allowed")
    }
    let bytes = response
        .bytes()
        .await
        .with_context(|| format!("could not read {url}"))?;
    if bytes.len() > maximum_bytes {
        bail!("download from {url} is larger than allowed")
    }
    Ok(bytes.to_vec())
}

fn verify_manifest_signature(manifest: &[u8], encoded_signature: &[u8]) -> anyhow::Result<()> {
    let public_key = STANDARD
        .decode(RELEASE_PUBLIC_KEY_BASE64)
        .context("relay release public key is invalid")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("relay release public key has an invalid length"))?;
    let encoded_signature = std::str::from_utf8(encoded_signature)
        .context("relay release signature is not text")?
        .trim();
    let signature = STANDARD
        .decode(encoded_signature)
        .context("relay release signature is invalid")?;
    let signature = Signature::from_slice(&signature)
        .context("relay release signature has an invalid length")?;
    VerifyingKey::from_bytes(&public_key)
        .context("relay release public key is malformed")?
        .verify(manifest, &signature)
        .context("relay release manifest signature is invalid")
}

fn validate_manifest(manifest: &ReleaseManifest) -> anyhow::Result<()> {
    if manifest.schema != MANIFEST_SCHEMA {
        bail!("unsupported relay release manifest schema")
    }
    if !matches!(manifest.channel.as_str(), "stable" | "canary") {
        bail!("relay release channel is invalid")
    }
    Version::parse(&manifest.version).context("relay release version is invalid")?;
    if manifest.protocol_min == 0
        || manifest.protocol_max < manifest.protocol_min
        || !(manifest.protocol_min..=manifest.protocol_max).contains(&RELAY_PROTOCOL_VERSION)
    {
        bail!("relay release is incompatible with this protocol")
    }
    let now = unix_seconds()?;
    if manifest.published_at_unix_seconds > now.saturating_add(5 * 60) {
        bail!("relay release manifest was published in the future")
    }
    if manifest.expires_at_unix_seconds <= now
        || manifest.expires_at_unix_seconds <= manifest.published_at_unix_seconds
        || manifest.expires_at_unix_seconds - manifest.published_at_unix_seconds
            > 180 * 24 * 60 * 60
    {
        bail!("relay release manifest is expired or has an invalid lifetime")
    }
    if manifest.assets.is_empty() || manifest.assets.len() > 8 {
        bail!("relay release manifest has an invalid asset list")
    }
    let mut targets = HashSet::new();
    for asset in &manifest.assets {
        if !targets.insert(asset.target.as_str())
            || !matches!(asset.target.as_str(), "linux-x86_64" | "linux-aarch64")
            || asset.sha256.len() != 64
            || !asset
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || asset.byte_length == 0
            || asset.byte_length > MAX_PACKAGE_BYTES as u64
        {
            bail!("relay release manifest contains an invalid asset")
        }
        require_https(&asset.url, "relay package")?;
    }
    Ok(())
}

async fn apply_release(
    manifest: &ReleaseManifest,
    asset: &ReleaseAsset,
    previous_version: &str,
    health_url: &str,
) -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        bail!("automatic relay installation is only supported on Linux")
    }
    let current_executable =
        fs::canonicalize(std::env::current_exe()?).context("could not locate running relay")?;
    if current_executable != Path::new(INSTALLED_BINARY) {
        bail!("automatic updates require the packaged relay at {INSTALLED_BINARY}")
    }
    let update_directory = PathBuf::from(UPDATE_DIRECTORY);
    fs::create_dir_all(&update_directory)
        .with_context(|| format!("could not create {}", update_directory.display()))?;
    clean_update_directory(&update_directory)?;
    let package_path = update_directory.join(format!("noise-relay-{}.deb", manifest.version));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(120))
        .build()
        .context("could not initialize package download")?;
    let package = fetch_limited(&client, &asset.url, MAX_PACKAGE_BYTES).await?;
    verify_package(&package, asset)?;
    fs::write(&package_path, package)
        .with_context(|| format!("could not stage {}", package_path.display()))?;
    let rollback_files = stage_rollback_files(&update_directory, &current_executable)?;

    let result = install_and_restart(&package_path, &manifest.version, health_url).await;
    if let Err(error) = result {
        let rollback_result = restore_files(&rollback_files, previous_version, health_url).await;
        let _ = fs::remove_file(&package_path);
        remove_rollback_files(&rollback_files);
        rollback_result.context("relay update failed and rollback also failed")?;
        return Err(error)
            .context("relay update failed; the previous binary and service units were restored");
    }
    fs::remove_file(&package_path).context("could not remove staged relay package")?;
    remove_rollback_files(&rollback_files);
    Ok(())
}

fn clean_update_directory(directory: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)
        .with_context(|| format!("could not inspect {}", directory.display()))?
    {
        let entry = entry.context("could not inspect staged relay update")?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if (name.starts_with("noise-relay-") && name.ends_with(".deb"))
            || name.ends_with(".rollback")
        {
            fs::remove_file(entry.path()).with_context(|| {
                format!(
                    "could not remove stale relay update {}",
                    entry.path().display()
                )
            })?;
        }
    }
    Ok(())
}

fn stage_rollback_files(
    update_directory: &Path,
    current_executable: &Path,
) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    let mut installed_files = vec![current_executable.to_path_buf()];
    installed_files.extend(INSTALLED_UNITS.iter().map(PathBuf::from));
    let mut rollback_files = Vec::with_capacity(installed_files.len());
    for (index, installed) in installed_files.into_iter().enumerate() {
        if !installed.exists() {
            if installed == current_executable {
                bail!("installed relay binary disappeared before the update")
            }
            continue;
        }
        let rollback = update_directory.join(format!("{index}.rollback"));
        fs::copy(&installed, &rollback)
            .with_context(|| format!("could not back up {}", installed.display()))?;
        rollback_files.push((installed, rollback));
    }
    Ok(rollback_files)
}

fn remove_rollback_files(files: &[(PathBuf, PathBuf)]) {
    for (_, rollback) in files {
        let _ = fs::remove_file(rollback);
    }
}

fn verify_package(package: &[u8], asset: &ReleaseAsset) -> anyhow::Result<()> {
    if package.len() as u64 != asset.byte_length {
        bail!("relay package size does not match its signed manifest")
    }
    let actual = Sha256::digest(package)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != asset.sha256 {
        bail!("relay package hash does not match its signed manifest")
    }
    Ok(())
}

async fn install_and_restart(
    package: &Path,
    expected_version: &str,
    health_url: &str,
) -> anyhow::Result<()> {
    command_success(
        Command::new("/usr/bin/dpkg").arg("--install").arg(package),
        "could not install relay package",
    )?;
    let version_output = Command::new(INSTALLED_BINARY)
        .arg("--version")
        .output()
        .context("could not verify installed relay binary")?;
    if !version_output.status.success()
        || !String::from_utf8_lossy(&version_output.stdout).contains(expected_version)
    {
        bail!("installed relay binary did not report version {expected_version}")
    }
    command_success(
        Command::new("/usr/bin/systemctl").arg("daemon-reload"),
        "could not reload systemd",
    )?;
    command_success(
        Command::new("/usr/bin/systemctl")
            .arg("restart")
            .arg("noise-relay.service"),
        "could not restart relay service",
    )?;
    wait_for_service(expected_version, health_url).await
}

async fn restore_files(
    files: &[(PathBuf, PathBuf)],
    expected_version: &str,
    health_url: &str,
) -> anyhow::Result<()> {
    for (installed, rollback) in files {
        fs::copy(rollback, installed)
            .with_context(|| format!("could not restore {}", installed.display()))?;
    }
    let _ = Command::new("/usr/bin/systemctl")
        .arg("daemon-reload")
        .status();
    command_success(
        Command::new("/usr/bin/systemctl")
            .arg("restart")
            .arg("noise-relay.service"),
        "could not restart restored relay",
    )?;
    wait_for_service(expected_version, health_url).await
}

async fn wait_for_service(expected_version: &str, health_url: &str) -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Health {
        status: String,
        software_version: String,
        protocol_version: u16,
    }

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(1))
        .build()
        .context("could not initialize post-update health check")?;
    for _ in 0..20 {
        let service_active = Command::new("/usr/bin/systemctl")
            .arg("is-active")
            .arg("--quiet")
            .arg("noise-relay.service")
            .status()
            .is_ok_and(|status| status.success());
        if service_active
            && let Ok(response) = client.get(health_url).send().await
            && let Ok(response) = response.error_for_status()
            && let Ok(health) = response.json::<Health>().await
            && health.status == "ok"
            && health.software_version == expected_version
            && health.protocol_version == RELAY_PROTOCOL_VERSION
        {
            return Ok(());
        }
        sleep(Duration::from_secs(1)).await;
    }
    bail!("relay service did not become healthy on {health_url} with version {expected_version}")
}

fn command_success(command: &mut Command, context: &str) -> anyhow::Result<()> {
    let status = command.status().with_context(|| context.to_owned())?;
    if !status.success() {
        bail!("{context}: process exited with {status}")
    }
    Ok(())
}

fn require_https(url: &str, description: &str) -> anyhow::Result<()> {
    let parsed =
        reqwest::Url::parse(url).with_context(|| format!("{description} URL is invalid"))?;
    if parsed.scheme() != "https" {
        bail!("{description} URL must use HTTPS")
    }
    Ok(())
}

fn platform_target() -> anyhow::Result<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("linux-x86_64"),
        ("linux", "aarch64") => Ok("linux-aarch64"),
        (os, architecture) => bail!("no relay package is available for {os}-{architecture}"),
    }
}

fn unix_seconds() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> ReleaseManifest {
        let now = unix_seconds().unwrap();
        ReleaseManifest {
            schema: MANIFEST_SCHEMA,
            channel: "stable".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol_min: RELAY_PROTOCOL_VERSION,
            protocol_max: RELAY_PROTOCOL_VERSION,
            published_at_unix_seconds: now,
            expires_at_unix_seconds: now + 24 * 60 * 60,
            assets: vec![
                ReleaseAsset {
                    target: "linux-x86_64".into(),
                    url: "https://github.com/GnosysLabs/noise/releases/download/relay-v0.1.7/noise-relay_0.1.7_amd64.deb".into(),
                    sha256: "0".repeat(64),
                    byte_length: 1024,
                },
                ReleaseAsset {
                    target: "linux-aarch64".into(),
                    url: "https://github.com/GnosysLabs/noise/releases/download/relay-v0.1.7/noise-relay_0.1.7_arm64.deb".into(),
                    sha256: "1".repeat(64),
                    byte_length: 1024,
                },
            ],
        }
    }

    #[test]
    fn accepts_a_bounded_signed_release_shape() {
        validate_manifest(&manifest()).unwrap();
    }

    #[test]
    fn rejects_duplicate_targets_and_incompatible_protocols() {
        let mut duplicate = manifest();
        duplicate.assets.push(duplicate.assets[0].clone());
        assert!(validate_manifest(&duplicate).is_err());

        let mut incompatible = manifest();
        incompatible.protocol_min = RELAY_PROTOCOL_VERSION + 1;
        incompatible.protocol_max = RELAY_PROTOCOL_VERSION + 1;
        assert!(validate_manifest(&incompatible).is_err());
    }

    #[test]
    fn embedded_release_key_accepts_only_the_known_signature() {
        let message = b"noise-relay-release-test-vector-v1\n";
        let signature =
            b"SzwBT1wAOLvwbaOheQnoznS33XSLtOjCHAeGJNDc3CqVokHCQz0wMxKxnjXPErXFMJMDhhSAWQYMxzxAax7cBg==";
        verify_manifest_signature(message, signature).unwrap();

        let mut tampered = message.to_vec();
        tampered[0] ^= 1;
        assert!(verify_manifest_signature(&tampered, signature).is_err());
    }
}
