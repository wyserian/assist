//! The phone's link to its computer (PLAN.md section 3.13): pairing with the code from
//! `assist pair`, then a connection kept open while the app runs. Over it the phone talks to
//! the computer's assistant as its owner, and runs the phone commands the owner confirmed on
//! the computer ("text mom" said to the computer). The computer checks the phone's key on
//! every connection; the phone checks the computer's.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use assist_core::action::BoxFuture;
use assist_net::{Endpoint, Hub, NetError, Ticket};
use assist_proto::{
    Envelope, Hello, JobUpdate, Kind, Pair, Paired, Ran, Reply, RpcResult, Run, Utterance,
};
use iroh::endpoint::Connection;
use tokio::sync::mpsc;

use crate::phone::Phone;

/// How long to wait for the computer to answer.
const ANSWER_WAIT: Duration = Duration::from_secs(30);
/// Messages queued each way.
const QUEUE: usize = 16;

/// Why the link failed, for the user.
pub(crate) type LinkResult<T> = Result<T, String>;

/// Runs a command the computer asks for and reports how it went.
pub(crate) type Runner = Arc<dyn Fn(Run) -> BoxFuture<Ran> + Send + Sync>;

/// What a connection needs from the phone.
#[derive(Clone)]
pub(crate) struct Phoneside {
    pub(crate) phone: Arc<dyn Phone>,
    pub(crate) runner: Runner,
    /// Contact names for the computer, so "text mom" matches there.
    pub(crate) contacts: Vec<String>,
}

/// The phone's network endpoint and its open connection to the computer.
#[derive(Default)]
pub(crate) struct Link {
    endpoint: Option<Endpoint>,
    session: Option<Session>,
}

struct Session {
    outbox: mpsc::Sender<String>,
    replies: mpsc::Receiver<Reply>,
    alive: Arc<AtomicBool>,
    connection: Connection,
}

impl Link {
    /// Pairs with the computer in `code`; returns its name.
    pub(crate) async fn pair(
        &mut self,
        home: &Path,
        code: &str,
        name: &str,
        capabilities: Vec<String>,
    ) -> LinkResult<String> {
        let ticket = Ticket::parse(code).map_err(|err| err.to_string())?;
        let endpoint = self.endpoint(home).await?;
        let connection = connect(&endpoint, ticket.hub).await?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(failed)?;
        let request = Pair {
            secret: ticket.secret,
            name: name.to_owned(),
            capabilities,
        };
        let message = Envelope::new(Kind::Pair, "phone", "hub", request);
        assist_net::send_frame(&mut send, &message.encode())
            .await
            .map_err(|err| err.to_string())?;
        let answer = tokio::time::timeout(ANSWER_WAIT, assist_net::recv_frame(&mut recv))
            .await
            .map_err(|_| "the computer did not answer".to_owned())?
            .map_err(|err| err.to_string())?
            .ok_or_else(|| "the computer closed the connection".to_owned())?;
        let answer = Envelope::decode(&answer).map_err(|err| err.to_string())?;
        match answer.kind {
            Kind::Paired => {
                let paired: Paired = answer.body().map_err(|err| err.to_string())?;
                let hub = Hub {
                    id: ticket.hub.to_string(),
                    name: paired.name.chars().take(64).collect(),
                };
                hub.save(home).map_err(|err| err.to_string())?;
                self.session = None;
                Ok(hub.name)
            }
            _ => Err(refusal(&answer)),
        }
    }

    /// Opens the connection to the paired computer if it is not open.
    pub(crate) async fn ensure(&mut self, home: &Path, side: &Phoneside) -> LinkResult<()> {
        if self
            .session
            .as_ref()
            .is_some_and(|session| session.alive.load(Ordering::Relaxed))
        {
            return Ok(());
        }
        let hub = Hub::load(home).ok_or_else(|| "pair with a computer first".to_owned())?;
        let key = hub.key().map_err(|err| err.to_string())?;
        let session = self.open(home, key, side.clone()).await?;
        self.session = Some(session);
        Ok(())
    }

    /// Sends `text` to the computer's assistant and returns its reply. Output of the jobs it
    /// starts arrives later through the phone's `show`.
    pub(crate) async fn say(
        &mut self,
        home: &Path,
        text: &str,
        side: &Phoneside,
    ) -> LinkResult<Reply> {
        self.ensure(home, side).await?;
        let utterance = Utterance {
            text: text.to_owned(),
            speaker_verified: false,
        };
        let message = Envelope::new(Kind::Utterance, "phone", "hub", utterance);
        let result = match self.session.as_mut() {
            Some(session) => session.ask(message.encode()).await,
            None => Err("not connected".to_owned()),
        };
        if result.is_err() {
            self.drop_session();
        }
        result
    }

    fn drop_session(&mut self) {
        if let Some(session) = self.session.take() {
            session.connection.close(0u32.into(), b"bye");
        }
    }

    async fn endpoint(&mut self, home: &Path) -> LinkResult<Endpoint> {
        if let Some(endpoint) = &self.endpoint {
            return Ok(endpoint.clone());
        }
        let endpoint = assist_net::endpoint(home)
            .await
            .map_err(|err| err.to_string())?;
        self.endpoint = Some(endpoint.clone());
        Ok(endpoint)
    }

    /// Connects, says hello with the contact names, and starts a writer task (which owns
    /// the stream to the computer) and a reader task (replies, job output, commands to run).
    async fn open(
        &mut self,
        home: &Path,
        hub: assist_net::EndpointId,
        side: Phoneside,
    ) -> LinkResult<Session> {
        let endpoint = self.endpoint(home).await?;
        let connection = connect(&endpoint, hub).await?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(failed)?;
        let hello = Hello {
            client: "assist-android".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            cwd: None,
            voice: false,
            contacts: side.contacts.clone(),
        };
        let message = Envelope::new(Kind::Hello, "phone", "hub", hello);
        assist_net::send_frame(&mut send, &message.encode())
            .await
            .map_err(|err| err.to_string())?;
        let alive = Arc::new(AtomicBool::new(true));
        let (outbox, mut outgoing) = mpsc::channel::<String>(QUEUE);
        let writer_alive = Arc::clone(&alive);
        tokio::spawn(async move {
            while let Some(text) = outgoing.recv().await {
                if assist_net::send_frame(&mut send, &text).await.is_err() {
                    break;
                }
            }
            writer_alive.store(false, Ordering::Relaxed);
        });
        let (replies, read) = mpsc::channel(QUEUE);
        let reader_alive = Arc::clone(&alive);
        let answers = outbox.clone();
        tokio::spawn(async move {
            while let Ok(Some(text)) = assist_net::recv_frame(&mut recv).await {
                let Ok(message) = Envelope::decode(&text) else {
                    continue;
                };
                handle(message, &replies, &answers, &side).await;
            }
            reader_alive.store(false, Ordering::Relaxed);
        });
        Ok(Session {
            outbox,
            replies: read,
            alive,
            connection,
        })
    }
}

impl Session {
    async fn ask(&mut self, message: String) -> LinkResult<Reply> {
        self.outbox
            .send(message)
            .await
            .map_err(|_| "the computer closed the connection".to_owned())?;
        tokio::time::timeout(ANSWER_WAIT, self.replies.recv())
            .await
            .map_err(|_| "the computer did not answer".to_owned())?
            .ok_or_else(|| "the computer closed the connection".to_owned())
    }
}

/// One message from the computer: a reply for the waiting `say`, a line of job output or a
/// failure for the chat, or a command to run here.
async fn handle(
    message: Envelope,
    replies: &mpsc::Sender<Reply>,
    answers: &mpsc::Sender<String>,
    side: &Phoneside,
) {
    let show = |line: String| {
        let phone = Arc::clone(&side.phone);
        drop(tokio::task::spawn_blocking(move || phone.show(line)));
    };
    match message.kind {
        Kind::Reply => {
            if let Ok(reply) = message.body::<Reply>() {
                let _ = replies.send(reply).await;
            }
        }
        Kind::JobUpdate => match message.body::<JobUpdate>() {
            Ok(JobUpdate::Output { text, .. }) => show(text),
            Ok(JobUpdate::State {
                error: Some(error), ..
            }) => show(error),
            _ => {}
        },
        Kind::Run => {
            if let Ok(run) = message.body::<Run>() {
                // Commands can take a while (a call, a recording); the reader keeps reading.
                let runner = Arc::clone(&side.runner);
                let answers = answers.clone();
                tokio::spawn(async move {
                    let ran = runner(run).await;
                    let answer = Envelope::new(Kind::Ran, "phone", "hub", ran);
                    let _ = answers.send(answer.encode()).await;
                });
            }
        }
        _ => {}
    }
}

async fn connect(endpoint: &Endpoint, hub: assist_net::EndpointId) -> LinkResult<Connection> {
    tokio::time::timeout(ANSWER_WAIT, endpoint.connect(hub, assist_net::ALPN))
        .await
        .map_err(|_| "the computer is not reachable; is assist serve running?".to_owned())?
        .map_err(failed)
}

fn failed(err: impl std::fmt::Display) -> String {
    NetError::Connect(err.to_string()).to_string()
}

/// The computer's reason for refusing, from its `RpcResult`.
fn refusal(answer: &Envelope) -> String {
    answer
        .body::<RpcResult>()
        .ok()
        .and_then(|result| result.error)
        .map_or_else(|| "the computer refused".to_owned(), |error| error.message)
}

/// The computer this phone is paired with, if any.
pub(crate) fn paired(home: &Path) -> Option<String> {
    Hub::load(home).map(|hub| hub.name)
}
