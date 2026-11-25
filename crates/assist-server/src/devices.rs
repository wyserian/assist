//! Running commands on paired devices (PLAN.md section 3.13): the hub offers a phone's
//! commands it lacks itself (call, text, ...) as actions that hand the confirmed request to
//! a connected device able to do it and report its result as the job's own.
//!
//! shortcut: a device that is not connected fails the job at once; queue it and wake the
//! phone (store-and-forward, step 3).

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use assist_core::action::{Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Manifest, Output};
use assist_proto::{Envelope, Kind, Ran, Run};
use tokio::sync::{mpsc, oneshot};

use crate::conn::HUB;

/// How long a device has to report a command's result.
const RUN_WAIT: Duration = Duration::from_secs(120);

/// The paired devices connected now, and the requests waiting on them.
#[derive(Default)]
pub struct Devices {
    connected: Mutex<HashMap<String, Device>>,
    next: AtomicU64,
}

struct Device {
    name: String,
    capabilities: Vec<String>,
    outbox: mpsc::Sender<Envelope>,
    waiting: HashMap<u64, oneshot::Sender<Ran>>,
}

impl Devices {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Device>> {
        self.connected
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// A device connected; messages for it go to `outbox`.
    pub(crate) fn connect(
        &self,
        id: &str,
        name: &str,
        capabilities: Vec<String>,
        outbox: mpsc::Sender<Envelope>,
    ) {
        self.lock().insert(
            id.to_owned(),
            Device {
                name: name.to_owned(),
                capabilities,
                outbox,
                waiting: HashMap::new(),
            },
        );
    }

    /// A device went away; requests waiting on it fail.
    pub(crate) fn disconnect(&self, id: &str) {
        self.lock().remove(id);
    }

    /// A device reported a result.
    pub(crate) fn finished(&self, id: &str, ran: Ran) {
        let waiting = self
            .lock()
            .get_mut(id)
            .and_then(|device| device.waiting.remove(&ran.job));
        if let Some(waiting) = waiting {
            let _ = waiting.send(ran);
        }
    }

    /// Runs `command` on a connected device that can; returns its result and the device's
    /// name.
    async fn run(
        &self,
        command: &str,
        slots: BTreeMap<String, String>,
    ) -> Result<(Ran, String), String> {
        let job = self.next.fetch_add(1, Ordering::Relaxed);
        let (done, result) = oneshot::channel();
        let (outbox, name) = {
            let mut connected = self.lock();
            let device = connected
                .values_mut()
                .find(|device| device.capabilities.iter().any(|c| c == command))
                .ok_or_else(|| {
                    "no paired phone is connected; open Assist on the phone".to_owned()
                })?;
            device.waiting.insert(job, done);
            (device.outbox.clone(), device.name.clone())
        };
        let request = Run {
            job,
            command: command.to_owned(),
            slots,
        };
        outbox
            .send(Envelope::new(Kind::Run, HUB, &name, request))
            .await
            .map_err(|_| format!("{name} disconnected"))?;
        let ran = tokio::time::timeout(RUN_WAIT, result)
            .await
            .map_err(|_| format!("{name} did not answer"))?
            .map_err(|_| format!("{name} disconnected"))?;
        Ok((ran, name))
    }
}

/// Actions for the phone commands this hub has no action for (`local` names its own), each
/// run on a paired device.
pub fn proxies(devices: &Arc<Devices>, local: &[&str]) -> Vec<Arc<dyn Action>> {
    assist_net::commands::PHONE
        .into_iter()
        .filter(|manifest| !local.contains(&manifest.name))
        .map(|manifest| {
            Arc::new(Remote {
                manifest,
                devices: Arc::clone(devices),
            }) as Arc<dyn Action>
        })
        .collect()
}

/// A phone command run on a paired device.
struct Remote {
    manifest: &'static Manifest,
    devices: Arc<Devices>,
}

impl Action for Remote {
    fn manifest(&self) -> &Manifest {
        self.manifest
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        let devices = Arc::clone(&self.devices);
        let command = self.manifest.name;
        Box::pin(async move {
            let (ran, device) = devices
                .run(command, ctx.plan.slots.clone())
                .await
                .map_err(ActionError::new)?;
            let _ = out.send(Chunk::Info(format!("On {device}."))).await;
            for line in ran.lines {
                if out.send(Chunk::Line(line)).await.is_err() {
                    break;
                }
            }
            match ran.error {
                Some(error) => Err(ActionError::new(error)),
                None => Ok(()),
            }
        })
    }
}
