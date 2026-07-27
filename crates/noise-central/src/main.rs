use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use noise_central::{CentralConfig, build_app};
use tokio::{net::TcpListener, signal};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("noise-central failed: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> anyhow::Result<()> {
    let config = CentralConfig::parse();
    let listen = config.listen;
    let app = build_app(&config).await?;
    let listener = TcpListener::bind(listen)
        .await
        .with_context(|| format!("could not bind noise-central to {listen}"))?;
    eprintln!("noise-central listening on {listen}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("noise-central server failed")
}

async fn shutdown_signal() {
    let interrupt = async {
        signal::ctrl_c()
            .await
            .expect("could not install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("could not install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {},
        () = terminate => {},
    }
}
