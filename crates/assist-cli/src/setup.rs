//! Startup shared by `assist shell` and `assist serve`: config, audit log, job store,
//! scheduler and dialog engine.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use assist_core::audit::Audit;
use assist_core::config::Config;
use assist_core::dialog::{Engine, LiveEngine};
use assist_core::phrases::PhraseSet;
use assist_core::scheduler::{Persistence, Scheduler};
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
    /// Loads config and opens the data directory. Prints what it cleaned up from the last run.
    pub fn start(paths: &Paths) -> Result<Self> {
        let config = Arc::new(load_config(paths)?);
        let audit_dir = paths.data.join("audit");
        let audit = Audit::open(&audit_dir)
            .with_context(|| format!("audit log {}", audit_dir.display()))?;
        let (store, report) = Store::open(&paths.data, &config.jobs)
            .with_context(|| format!("job store {}", paths.data.display()))?;
        if report.interrupted > 0 {
            println!(
                "{} job(s) from the last run were interrupted.",
                report.interrupted
            );
        }
        // Without a keystore the assistant still runs; secrets and redaction are off.
        let secrets = OsKeystore::open()
            .and_then(|keystore| Secrets::open(Box::new(keystore), &paths.data))
            .map_err(|err| println!("Secrets are off: {err}"))
            .ok()
            .map(Arc::new);
        let scheduler = Scheduler::new(
            assist_actions::builtin(),
            Arc::clone(&config),
            Persistence {
                audit: Some(audit.clone()),
                store: Some(store.clone()),
                redactor: secrets.as_ref().map(|secrets| secrets.redactor()),
            },
        );
        let mut phrases = PhraseSet::default_en()?;
        phrases.extend(PhraseSet::load_dir(&paths.phrases_dir())?);
        let engine = Arc::new(LiveEngine::new(Arc::new(Engine::new(
            Arc::clone(&config),
            &phrases,
            scheduler.commands(),
            true,
        )?)));
        Ok(Self {
            config,
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
