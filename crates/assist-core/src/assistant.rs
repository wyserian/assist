//! Building an assistant from its parts: the config, the data directory and what this device
//! brings (its actions and OS hooks). The desktop daemon and shell and the Android app share
//! it, so every device runs the same engine.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::action::Action;
use crate::audit::Audit;
use crate::config::Config;
use crate::dialog::{Device, Engine, EngineError, LiveEngine};
use crate::facts::Facts;
use crate::phrases::{PhraseSet, PhrasesError};
use crate::scheduler::{Persistence, Processes, Scheduler};
use crate::secrets::Redactor;
use crate::store::{OpenReport, Store, StoreError};

/// What a device brings.
#[derive(Default)]
pub struct Host {
    /// The actions this device runs.
    pub actions: Vec<Arc<dyn Action>>,
    /// The owner's phrases, on top of the built-in ones.
    pub phrases: PhraseSet,
    /// Offer unmatched input as a shell command (desktop).
    pub offer_shell: bool,
    /// Current device state, kept up to date by the device.
    pub facts: Facts,
    /// Scrubs secret values from titles and output, when a keystore is open.
    pub redactor: Option<Redactor>,
    /// Pauses and measures job processes.
    pub processes: Option<Arc<dyn Processes>>,
}

/// A running assistant's parts.
pub struct Parts {
    /// Hash-chained audit log.
    pub audit: Audit,
    /// Job history.
    pub store: Store,
    /// What opening the store cleaned up from the last run.
    pub report: OpenReport,
    /// Runs jobs.
    pub scheduler: Scheduler,
    /// Matcher, policy and commands; replaced when phrases change.
    pub engine: Arc<LiveEngine>,
}

/// Why an assistant could not be built.
#[derive(Debug, thiserror::Error)]
pub enum AssembleError {
    /// The audit log cannot be opened.
    #[error("audit log {}: {source}", .path.display())]
    Audit {
        /// Its directory.
        path: PathBuf,
        /// What went wrong.
        source: std::io::Error,
    },
    /// The job store cannot be opened.
    #[error("job store {}: {source}", .path.display())]
    Store {
        /// The data directory.
        path: PathBuf,
        /// What went wrong.
        source: StoreError,
    },
    /// A built-in phrase file is broken.
    #[error(transparent)]
    Phrases(#[from] PhrasesError),
    /// The config names something the engine cannot build.
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Opens the audit log and job store in `data` and builds the scheduler and engine.
pub fn assemble(config: Arc<Config>, data: &Path, host: Host) -> Result<Parts, AssembleError> {
    let audit_dir = data.join("audit");
    let audit = Audit::open(&audit_dir).map_err(|source| AssembleError::Audit {
        path: audit_dir,
        source,
    })?;
    let (store, report) =
        Store::open(data, &config.jobs).map_err(|source| AssembleError::Store {
            path: data.to_owned(),
            source,
        })?;
    let scheduler = Scheduler::new(
        host.actions,
        Arc::clone(&config),
        Persistence {
            audit: Some(audit.clone()),
            store: Some(store.clone()),
            redactor: host.redactor,
            data_dir: Some(data.to_owned()),
            processes: host.processes,
        },
    );
    let mut phrases = PhraseSet::default_en()?;
    phrases.extend(host.phrases);
    let device = Device {
        commands: scheduler.commands(),
        offer_shell: host.offer_shell,
        facts: host.facts,
        preview: Some(Arc::new(scheduler.clone())),
    };
    let engine = Engine::new(config, &phrases, device)?;
    Ok(Parts {
        audit,
        store,
        report,
        scheduler,
        engine: Arc::new(LiveEngine::new(Arc::new(engine))),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialog::Dialog;
    use crate::policy::Principal;
    use std::time::Instant;

    #[test]
    fn builds_a_working_engine_without_actions() {
        let data = std::env::temp_dir().join(format!("assist-assemble-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let config = Arc::new(Config::from_toml("").unwrap());
        let parts = assemble(config, &data, Host::default()).unwrap();
        let owner = Principal::local("test");
        let mut dialog = Dialog::new(parts.engine.get(), owner.clone());
        let reply = dialog.handle(&owner, "search rust", Instant::now());
        assert_eq!(reply.say, ["I can't do that on this device."]);
        drop(parts);
        let _ = std::fs::remove_dir_all(&data);
    }
}
