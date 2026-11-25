//! Startup shared by `assist shell` and `assist serve`: config, audit log, job store,
//! scheduler and dialog engine.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use assist_core::assistant::{assemble, Host, Parts};
use assist_core::audit::Audit;
use assist_core::config::Config;
use assist_core::dialog::LiveEngine;
use assist_core::facts::Facts;
use assist_core::phrases::PhraseSet;
use assist_core::scheduler::Scheduler;
use assist_core::secrets::Secrets;
use assist_core::store::Store;
use directories::ProjectDirs;

use crate::keystore::OsKeystore;

/// Where config and data live.
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
}

impl Paths {
    /// Explicit paths, or the user's config and data directories.
    pub fn resolve(config: Option<PathBuf>, data: Option<PathBuf>) -> Result<Self> {
        let dirs = ProjectDirs::from("", "", "assist");
        let config = match config {
            Some(path) => path,
            None => dirs
                .as_ref()
                .map(|d| d.config_dir().join("config.toml"))
                .context("no user config directory; pass --config")?,
        };
        let data = match data {
            Some(path) => path,
            None => dirs
                .as_ref()
                .map(|d| d.data_dir().to_owned())
                .context("no user data directory; pass --data-dir")?,
        };
        Ok(Self { config, data })
    }

    /// The data directory: audit log, job store, token, daemon address.
    pub fn data(&self) -> &PathBuf {
        &self.data
    }

    /// The config file.
    pub fn config_file(&self) -> &PathBuf {
        &self.config
    }

    /// User phrase files, next to the config file; they extend or override the built-ins.
    pub fn phrases_dir(&self) -> PathBuf {
        self.config
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("phrases")
    }
}

/// How long to wait for the OS to list installed apps before starting without them.
const FIND_APPS: Duration = Duration::from_secs(10);

/// How often device facts are read.
const FACT_INTERVAL: Duration = Duration::from_secs(30);

/// Keeps `facts` current from this device's sources; a fact a source stops reporting is
/// removed. Runs until the runtime stops.
async fn sample_facts(facts: Facts) {
    let mut reported: Vec<&'static str> = Vec::new();
    let mut tick = tokio::time::interval(FACT_INTERVAL);
    loop {
        tick.tick().await;
        let Ok(sampled) = tokio::task::spawn_blocking(assist_actions::facts::sample).await else {
            continue;
        };
        for name in reported
            .iter()
            .filter(|n| !sampled.iter().any(|(s, _)| s == *n))
        {
            facts.remove(name);
        }
        reported = sampled.iter().map(|(name, _)| *name).collect();
        for (name, value) in sampled {
            facts.set(name, value);
        }
    }
}

/// The config file, or an empty config when there is none.
pub fn load_config(paths: &Paths) -> Result<Config> {
    if paths.config.exists() {
        return Config::load(&paths.config)
            .with_context(|| format!("config {}", paths.config.display()));
    }
    println!(
        "No config at {}; starting empty (see config/config.example.toml).",
        paths.config.display()
    );
    Ok(Config::from_toml("")?)
}

/// Everything a running assistant needs.
pub struct Assistant {
    /// The loaded config.
    pub config: Arc<Config>,
    /// Current device state, sampled by the daemon.
    pub facts: Facts,
    /// Secrets, if a keystore is available.
    pub secrets: Option<Arc<Secrets>>,
    /// Audit log.
    pub audit: Audit,
    /// Job history.
    pub store: Store,
    /// Runs jobs.
    pub scheduler: Scheduler,
    /// Shared dialog state: matcher, policy, available commands. Replaced when phrases are
    /// added.
    pub engine: Arc<LiveEngine>,
}

impl Assistant {
    /// Loads config, adds the apps installed here and opens the data directory. Prints what
    /// it cleaned up from the last run.
    pub async fn start(paths: &Paths) -> Result<Self> {
        let mut config = load_config(paths)?;
        let found = tokio::time::timeout(
            FIND_APPS,
            tokio::task::spawn_blocking(assist_actions::apps::installed),
        )
        .await;
        match found {
            Ok(Ok(apps)) => config.add_found_apps(apps),
            _ => println!("Installed apps not found; only [apps] from the config can be opened."),
        }
        let config = Arc::new(config);
        // Without a keystore the assistant still runs; secrets and redaction are off.
        let secrets = OsKeystore::open()
            .and_then(|keystore| Secrets::open(Box::new(keystore), &paths.data))
            .map_err(|err| println!("Secrets are off: {err}"))
            .ok()
            .map(Arc::new);
        let facts = Facts::default();
        tokio::spawn(sample_facts(facts.clone()));
        let host = Host {
            actions: assist_actions::builtin(),
            phrases: PhraseSet::load_dir(&paths.phrases_dir())?,
            offer_shell: true,
            facts: facts.clone(),
            redactor: secrets.as_ref().map(|secrets| secrets.redactor()),
            processes: Some(Arc::new(assist_actions::processes::OsProcesses)),
        };
        let parts = assemble(Arc::clone(&config), &paths.data, host)?;
        if parts.report.interrupted > 0 {
            println!(
                "{} job(s) from the last run were interrupted.",
                parts.report.interrupted
            );
        }
        let Parts {
            audit,
            store,
            scheduler,
            engine,
            ..
        } = parts;
        Ok(Self {
            config,
            facts,
            secrets,
            audit,
            store,
            scheduler,
            engine,
        })
    }

    /// Writes pending history to disk and reports write errors. Call once at shutdown.
    pub async fn shutdown(self) {
        let store = self.store.clone();
        let _ = tokio::task::spawn_blocking(move || store.close()).await;
        if let Some(err) = self.store.last_error() {
            eprintln!("job store error: {err}");
        }
        if let Some(err) = self.audit.last_error() {
            eprintln!("audit log error: {err}");
        }
    }
}
