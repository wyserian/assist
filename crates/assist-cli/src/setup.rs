//! Startup shared by `assist shell` and `assist serve`: config, audit log, job store,
//! scheduler and dialog engine.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use assist_core::audit::Audit;
use assist_core::config::Config;
use assist_core::dialog::Engine;
use assist_core::phrases::PhraseSet;
use assist_core::scheduler::{Persistence, Scheduler};
use assist_core::store::Store;
use directories::ProjectDirs;

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
}

/// Everything a running assistant needs.
pub struct Assistant {
    /// Audit log.
    pub audit: Audit,
    /// Job history.
    pub store: Store,
    /// Runs jobs.
    pub scheduler: Scheduler,
    /// Shared dialog state: matcher, policy, available commands.
    pub engine: Arc<Engine>,
}

impl Assistant {
    /// Loads config and opens the data directory. Prints what it cleaned up from the last run.
    pub fn start(paths: &Paths) -> Result<Self> {
        let config = if paths.config.exists() {
            Config::load(&paths.config)
                .with_context(|| format!("config {}", paths.config.display()))?
        } else {
            println!(
                "No config at {}; starting empty (see config/config.example.toml).",
                paths.config.display()
            );
            Config::from_toml("")?
        };
        let config = Arc::new(config);
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
        let scheduler = Scheduler::new(
            assist_actions::builtin(),
            Arc::clone(&config),
            Persistence {
                audit: Some(audit.clone()),
                store: Some(store.clone()),
            },
        );
        let phrases = PhraseSet::default_en()?;
        let engine = Arc::new(Engine::new(
            Arc::clone(&config),
            &phrases,
            scheduler.commands(),
            true,
        )?);
        Ok(Self {
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
