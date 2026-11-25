//! `assist serve`: the daemon. Clients find it through `daemon.addr` in the data directory.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use assist_core::private_fs;
use assist_server::{serve, Nodes, Services};
use tokio::net::TcpListener;

use crate::setup::{Assistant, Paths};
use crate::{proactive, triggers};

/// File in the data directory with the address the daemon listens on.
const ADDR_FILE: &str = "daemon.addr";

/// Proactive messages waiting for slow clients; a client further behind skips some.
const NOTICE_QUEUE: usize = 64;

/// How long shutdown waits for stopped jobs to end.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// Serves until Ctrl+C or "shut down", then stops every job and saves history.
pub async fn run(paths: Paths) -> Result<()> {
    let assistant = Assistant::start(&paths).await?;
    let bind = assistant.config.server.bind;
    let listener = TcpListener::bind(bind)
        .await
        .with_context(|| format!("cannot listen on {bind} (is another daemon running?)"))?;
    let addr = listener.local_addr()?;
    // The address file says the daemon is ready, so the token clients need exists first.
    assist_server::load_or_create_token(paths.data())?;
    let addr_file = addr_file(paths.data());
    write_addr(&addr_file, &addr.to_string())?;
    println!(
        "assist {} listening on http://{addr}",
        env!("CARGO_PKG_VERSION")
    );

    let nodes = start_nodes(paths.data(), Arc::clone(&assistant.devices)).await;
    let endpoint = nodes.as_ref().map(|nodes| nodes.endpoint.clone());
    let notices = tokio::sync::broadcast::channel(NOTICE_QUEUE).0;
    let services = Services {
        scheduler: assistant.scheduler.clone(),
        engine: assistant.engine.clone(),
        facts: assistant.facts.clone(),
        notices: notices.clone(),
        store: assistant.store.clone(),
        audit: Some(assistant.audit.clone()),
        secrets: assistant.secrets.clone(),
        default_cwd: std::env::current_dir()?,
        data_dir: paths.data().clone(),
        user_phrases: Some(paths.phrases_dir().join("user.toml")),
        config_file: Some(paths.config_file().clone()),
        nodes,
    };
    triggers::start(
        &assistant.config,
        triggers::Runner {
            engine: assistant.engine.clone(),
            scheduler: assistant.scheduler.clone(),
            facts: assistant.facts.clone(),
            cwd: std::env::current_dir()?,
            config: assistant.config.clone(),
            data: paths.data().clone(),
            notices: notices.clone(),
        },
    );
    proactive::start(
        &assistant.config,
        assistant.scheduler.clone(),
        assistant.facts.clone(),
        paths.data().clone(),
        notices,
    );
    let stop = assistant.stop.clone();
    let result = serve(services, listener, async move {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            () = stop.cancelled() => println!("Shutting down."),
        }
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
    if let Some(endpoint) = endpoint {
        endpoint.close().await;
    }
    assistant.shutdown().await;
    result.context("server failed")
}

/// The network for paired devices (PLAN.md section 3.13); without it the daemon still serves
/// this machine.
async fn start_nodes(data: &Path, devices: Arc<assist_server::Devices>) -> Option<Nodes> {
    let peers = match assist_net::Peers::load(data) {
        Ok(peers) => peers,
        Err(err) => {
            println!("Paired devices are off: {err}");
            return None;
        }
    };
    match assist_net::endpoint(data).await {
        Ok(endpoint) => Some(Nodes {
            endpoint,
            peers: Mutex::new(peers),
            pairing: assist_net::Pairing::default(),
            name: assist_net::host_name(),
            devices,
        }),
        Err(err) => {
            println!("Paired devices are off: {err}");
            None
        }
    }
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
