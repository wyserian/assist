//! Running commands on paired devices (PLAN.md section 3.13): the hub offers a phone's
//! commands it lacks itself (call, text, ...) as actions that hand the confirmed request to
//! a paired device able to do it and report its result as the job's own. A device that is
//! away gets the request when it connects, within [`QUEUE_WAIT`]; one with a push address
//! is woken through it.

use std::collections::{BTreeMap, HashMap};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use assist_core::action::{Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Manifest, Output};
use assist_net::Peer;
use assist_proto::{Envelope, Kind, Ran, Run};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::conn::HUB;

/// How long a device has to report a command's result.
const RUN_WAIT: Duration = Duration::from_secs(120);
/// How long a request waits for its device to connect.
const QUEUE_WAIT: Duration = Duration::from_secs(600);
/// How often an away device is woken again while a request waits.
const WAKE_EVERY: Duration = Duration::from_secs(60);
/// Longest push address kept.
const MAX_PUSH: usize = 1024;

/// The paired devices, the ones connected now, and the requests waiting on them.
#[derive(Default)]
pub struct Devices {
    paired: Mutex<HashMap<String, Device>>,
    /// Signalled when a device connects.
    arrived: Notify,
    next: AtomicU64,
}

struct Device {
    name: String,
    capabilities: Vec<String>,
    /// Its UnifiedPush address: a POST there wakes it.
    push: Option<String>,
    woken: Option<Instant>,
    link: Option<Link>,
}

/// An open connection to a device.
struct Link {
    /// Tells this connection from a later one of the same device.
    token: u64,
    outbox: mpsc::Sender<Envelope>,
    waiting: HashMap<u64, oneshot::Sender<Ran>>,
}

/// Where a request can go now.
enum Pick {
    Ready {
        name: String,
        outbox: mpsc::Sender<Envelope>,
        result: oneshot::Receiver<Ran>,
    },
    Away {
        name: String,
        /// The address to wake it through, when it is due a wake.
        wake: Option<String>,
    },
}

impl Devices {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Device>> {
        self.paired.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Remembers a paired device, connected or not.
    pub fn know(&self, peer: &Peer) {
        let mut paired = self.lock();
        let device = paired.entry(peer.id.clone()).or_insert_with(|| Device {
            name: String::new(),
            capabilities: Vec::new(),
            push: None,
            woken: None,
            link: None,
        });
        device.name.clone_from(&peer.name);
        device.capabilities.clone_from(&peer.capabilities);
        device.push = peer.push.clone().filter(|url| valid_push(url));
    }

    /// Device `peer` connected; messages for it go to `outbox`. Returns the connection's
    /// token for [`Devices::disconnect`].
    pub(crate) fn connect(&self, peer: &Peer, outbox: mpsc::Sender<Envelope>) -> u64 {
        self.know(peer);
        let token = self.next.fetch_add(1, Ordering::Relaxed);
        if let Some(device) = self.lock().get_mut(&peer.id) {
            device.link = Some(Link {
                token,
                outbox,
                waiting: HashMap::new(),
            });
        }
        self.arrived.notify_waiters();
        token
    }

    /// Connection `token` of device `id` went away; requests waiting on it fail.
    pub(crate) fn disconnect(&self, id: &str, token: u64) {
        if let Some(device) = self.lock().get_mut(id) {
            if device.link.as_ref().is_some_and(|link| link.token == token) {
                device.link = None;
            }
        }
    }

    /// Device `id` gave its push address; returns whether it is valid.
    pub(crate) fn set_push(&self, id: &str, url: &str) -> bool {
        let valid = valid_push(url);
        if let (true, Some(device)) = (valid, self.lock().get_mut(id)) {
            device.push = Some(url.to_owned());
        }
        valid
    }

    /// A device reported a result.
    pub(crate) fn finished(&self, id: &str, ran: Ran) {
        let waiting = self
            .lock()
            .get_mut(id)
            .and_then(|device| device.link.as_mut())
            .and_then(|link| link.waiting.remove(&ran.job));
        if let Some(waiting) = waiting {
            let _ = waiting.send(ran);
        }
    }

    /// A connected device that can run `command` takes request `job`; otherwise an away one
    /// that can, due a wake at most every [`WAKE_EVERY`].
    fn pick(&self, command: &str, job: u64) -> Result<Pick, String> {
        let mut paired = self.lock();
        let mut able: Vec<&mut Device> = paired
            .values_mut()
            .filter(|device| device.capabilities.iter().any(|c| c == command))
            .collect();
        let at = able
            .iter()
            .position(|device| device.link.is_some())
            .unwrap_or(0);
        let Some(device) = able.get_mut(at) else {
            return Err(format!(
                "no paired device can {command}; pair the phone first"
            ));
        };
        let name = device.name.clone();
        let Some(link) = device.link.as_mut() else {
            let due = device.woken.is_none_or(|at| at.elapsed() >= WAKE_EVERY);
            let wake = device.push.clone().filter(|_| due);
            if wake.is_some() {
                device.woken = Some(Instant::now());
            }
            return Ok(Pick::Away { name, wake });
        };
        let (done, result) = oneshot::channel();
        link.waiting.insert(job, done);
        Ok(Pick::Ready {
            name,
            outbox: link.outbox.clone(),
            result,
        })
    }

    /// Runs `command` on a paired device that can, waiting for it to connect when it is
    /// away; returns its result and the device's name.
    async fn run(
        &self,
        command: &str,
        slots: BTreeMap<String, String>,
        out: &Output,
    ) -> Result<(Ran, String), String> {
        let job = self.next.fetch_add(1, Ordering::Relaxed);
        let deadline = tokio::time::Instant::now() + QUEUE_WAIT;
        let mut told = false;
        loop {
            // Registered before looking, so a device connecting in between is not missed.
            let arrived = self.arrived.notified();
            tokio::pin!(arrived);
            arrived.as_mut().enable();
            let (name, wake) = match self.pick(command, job)? {
                Pick::Ready {
                    name,
                    outbox,
                    result,
                } => return send(job, command, slots, name, outbox, result).await,
                Pick::Away { name, wake } => (name, wake),
            };
            if !told {
                told = true;
                let line = if wake.is_some() {
                    format!("Waking {name}.")
                } else {
                    format!("Waiting for {name}; open Assist on it.")
                };
                let _ = out.send(Chunk::Info(line)).await;
            }
            if let Some(url) = wake {
                tokio::spawn(wake_up(url, name.clone()));
            }
            let slice = (tokio::time::Instant::now() + WAKE_EVERY).min(deadline);
            if tokio::time::timeout_at(slice, arrived).await.is_err() && slice == deadline {
                return Err(format!(
                    "{name} did not connect within {} minutes; open Assist on it",
                    QUEUE_WAIT.as_secs() / 60
                ));
            }
        }
    }
}

/// Sends request `job` to a connected device and waits for its result.
async fn send(
    job: u64,
    command: &str,
    slots: BTreeMap<String, String>,
    name: String,
    outbox: mpsc::Sender<Envelope>,
    result: oneshot::Receiver<Ran>,
) -> Result<(Ran, String), String> {
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

/// A push address the hub will POST to: https, bounded, no spaces or control characters.
fn valid_push(url: &str) -> bool {
    url.len() <= MAX_PUSH
        && url.starts_with("https://")
        && url.len() > "https://".len()
        && url.bytes().all(|b| b.is_ascii_graphic())
}

/// Wakes a device through its push server. The message carries nothing: the device only
/// connects, and the request comes over the authenticated connection.
///
/// shortcut: runs the system curl (on Windows 10+, macOS and most Linux), like the model
/// downloads; move to an HTTP client crate if the hub runs where curl is missing.
async fn wake_up(url: String, name: String) {
    let status = tokio::process::Command::new("curl")
        .args(["-sS", "-m", "15", "--proto", "=https", "-X", "POST"])
        .args(["--data-binary", "wake", "--url"])
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .status()
        .await;
    if !status.is_ok_and(|status| status.success()) {
        println!("Could not wake {name} through its push server.");
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
                .run(command, ctx.plan.slots.clone(), &out)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bounded_https_push_addresses_are_kept() {
        assert!(valid_push("https://ntfy.sh/upAbC123?up=1"));
        assert!(!valid_push("http://ntfy.sh/up"));
        assert!(!valid_push("https://"));
        assert!(!valid_push("https://ntfy.sh/a b"));
        assert!(!valid_push("https://ntfy.sh/a\n-o x"));
        assert!(!valid_push(&format!("https://{}", "a".repeat(MAX_PUSH))));
    }
}
