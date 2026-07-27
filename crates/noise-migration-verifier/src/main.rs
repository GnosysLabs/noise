use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, bail};
use clap::Parser;
use noise_migration_verifier::{SourceInput, verify};

#[derive(Parser)]
#[command(
    name = "noise-migration-verifier",
    about = "Read-only reconciliation of clean noise relay snapshots"
)]
struct Args {
    /// Snapshot in the form LABEL=/absolute/path/to/relay.db. Repeat per relay.
    #[arg(long = "source", required = true)]
    sources: Vec<String>,

    /// Relay data root in the form LABEL=/absolute/path. Repeat per relay.
    #[arg(long = "media-root", required = true)]
    media_roots: Vec<String>,

    /// Source whose push registrations become the migration source.
    #[arg(long)]
    primary_source: String,

    /// Write the sanitized JSON report here instead of stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let databases = parse_labeled_paths(&args.sources, "source")?;
    let media_roots = parse_labeled_paths(&args.media_roots, "media root")?;
    if databases.len() < 2 {
        bail!("at least two relay snapshots are required");
    }
    if !databases.contains_key(&args.primary_source) {
        bail!("primary source does not match a configured source label");
    }
    if databases.keys().ne(media_roots.keys()) {
        bail!("every source must have exactly one matching media root");
    }
    let inputs = databases
        .into_iter()
        .map(|(label, database_path)| SourceInput {
            media_root: media_roots
                .get(&label)
                .expect("matching media roots were checked")
                .clone(),
            label,
            database_path,
        })
        .collect();
    let report = verify(inputs, &args.primary_source)?;
    let encoded = serde_json::to_vec_pretty(&report)?;
    if let Some(output) = args.output {
        std::fs::write(&output, &encoded)
            .with_context(|| format!("could not write report {}", output.display()))?;
    } else {
        println!("{}", String::from_utf8(encoded).expect("JSON is UTF-8"));
    }
    if report.status != "pass" {
        bail!(
            "migration verification is blocked by {} invariant violation(s)",
            report
                .blockers
                .iter()
                .map(|blocker| blocker.count)
                .sum::<u64>()
        );
    }
    Ok(())
}

fn parse_labeled_paths(
    values: &[String],
    description: &str,
) -> anyhow::Result<BTreeMap<String, PathBuf>> {
    let mut parsed = BTreeMap::new();
    for value in values {
        let (label, path) = value
            .split_once('=')
            .with_context(|| format!("{description} must use LABEL=/absolute/path"))?;
        if label.is_empty()
            || label.len() > 64
            || !label.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
        {
            bail!("{description} has an invalid label");
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            bail!("{description} path must be absolute");
        }
        if parsed.insert(label.to_owned(), path).is_some() {
            bail!("{description} label was provided more than once");
        }
    }
    Ok(parsed)
}
