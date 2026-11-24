//! `assist serve`: the daemon. Clients find it through `daemon.addr` in the data directory.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use assist_core::private_fs;
use assist_server::{serve, Services};
use tokio::net::TcpListener;

use crate::setup::{Assistant, Paths};

/// File in the data directory with the address the daemon listens on.
const ADDR_FILE: &str = "daemon.addr";

/// How long shutdown waits for stopped jobs to end.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// Serves until Ctrl+C, then stops every job and saves history.
pub async fn run(paths: Paths) -> Result<()> {
    let assistant = Assistant::start(&paths)?;
    let bind = assistant.config.server.bind;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot listen on {bind} (is another daemon running?)"))?;
    let addr = listener.local_addr()?;
    let addr_file = addr_file(paths.data());
    write_addr(&addr_file, &addr.to_string())?;
    println!(
        "assist {} listening on http://{addr}",
        env!("CARGO_PKG_VERSION")
    );

    let services = Services {
        scheduler: assistant.scheduler.clone(),
        engine: assistant.engine.clone(),
        store: assistant.store.clone(),
        audit: Some(assistant.audit.clone()),
        secrets: assistant.secrets.clone(),
        default_cwd: std::env::current_dir()?,
        data_dir: paths.data().clone(),
        user_phrases: Some(paths.phrases_dir().join("user.toml")),
    };
    let result = serve(services, listener, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;

    let stopped = assistant.scheduler.cancel_all();
    if stopped > 0 {
        println!("Stopping {stopped} job(s).");
    }
    let deadline = tokio::time::Instant::now() + STOP_WAIT;
    while assistant.scheduler.active() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let _ = std::fs::remove_file(&addr_file);
    assistant.shutdown().await;
    result.context("server failed")
}

/// Writes the listening address, owner-only.
fn write_addr(path: &Path, addr: &str) -> Result<()> {
    let _ = std::fs::remove_file(path);
    let mut file = private_fs::open_append(path)?;
    file.write_all(addr.as_bytes())?;
    Ok(())
}

/// The data directory's daemon address file.
pub fn addr_file(data: &Path) -> PathBuf {
    data.join(ADDR_FILE)
}
