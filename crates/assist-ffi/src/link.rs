//! The phone's link to its computer (PLAN.md section 3.13): pairing with the code from
//! `assist pair`, then talking to the computer's assistant over iroh as its owner. The
//! computer checks the phone's key on every connection; the phone checks the computer's.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use assist_net::{Endpoint, Hub, NetError, Ticket};
use assist_proto::{Envelope, Hello, JobUpdate, Kind, Pair, Paired, Reply, RpcResult, Utterance};
use iroh::endpoint::{Connection, SendStream};
use tokio::sync::mpsc;

use crate::phone::Phone;

/// How long to wait for the computer to answer.
const ANSWER_WAIT: Duration = Duration::from_secs(30);
/// Replies read ahead.
const REPLIES: usize = 4;

/// Why the link failed, for the user.
pub(crate) type LinkResult<T> = Result<T, String>;

/// The phone's network endpoint, its computer and an open conversation with it.
#[derive(Default)]
pub(crate) struct Link {
    endpoint: Option<Endpoint>,
    session: Option<Session>,
}

struct Session {
    send: SendStream,
    replies: mpsc::Receiver<Reply>,
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

    /// Sends `text` to the computer's assistant and returns its reply. Output of the jobs it
    /// starts arrives later through `phone`'s `show`.
    pub(crate) async fn say(
        &mut self,
        home: &Path,
        text: &str,
        phone: &Arc<dyn Phone>,
    ) -> LinkResult<Reply> {
        let hub = Hub::load(home).ok_or_else(|| "pair with a computer first".to_owned())?;
        if self.session.is_none() {
            let key = hub.key().map_err(|err| err.to_string())?;
            self.session = Some(self.open(home, key, Arc::clone(phone)).await?);
        }
        let utterance = Utterance {
            text: text.to_owned(),
            speaker_verified: false,
        };
        let result = match self.session.as_mut() {
            Some(session) => session.ask(utterance).await,
            None => Err("not connected".to_owned()),
        };
        if result.is_err() {
            // Reconnect next time.
            if let Some(session) = self.session.take() {
                session.connection.close(0u32.into(), b"bye");
            }
        }
        result
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

    /// Connects, says hello and starts reading what the computer sends.
    async fn open(
        &mut self,
        home: &Path,
        hub: assist_net::EndpointId,
        phone: Arc<dyn Phone>,
    ) -> LinkResult<Session> {
        let endpoint = self.endpoint(home).await?;
        let connection = connect(&endpoint, hub).await?;
        let (mut send, mut recv) = connection.open_bi().await.map_err(failed)?;
        let hello = Hello {
            client: "assist-android".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            cwd: None,
            voice: false,
        };
        let message = Envelope::new(Kind::Hello, "phone", "hub", hello);
        assist_net::send_frame(&mut send, &message.encode())
            .await
            .map_err(|err| err.to_string())?;
        let (replies, read) = mpsc::channel(REPLIES);
        tokio::spawn(async move {
            while let Ok(Some(text)) = assist_net::recv_frame(&mut recv).await {
                let Ok(message) = Envelope::decode(&text) else {
                    continue;
                };
                if let Some(line) = route(&message, &replies).await {
                    let phone = Arc::clone(&phone);
                    drop(tokio::task::spawn_blocking(move || phone.show(line)));
                }
            }
        });
        Ok(Session {
            send,
            replies: read,
            connection,
        })
    }
}

impl Session {
    async fn ask(&mut self, utterance: Utterance) -> LinkResult<Reply> {
        let message = Envelope::new(Kind::Utterance, "phone", "hub", utterance);
        assist_net::send_frame(&mut self.send, &message.encode())
            .await
            .map_err(|err| err.to_string())?;
        tokio::time::timeout(ANSWER_WAIT, self.replies.recv())
            .await
            .map_err(|_| "the computer did not answer".to_owned())?
            .ok_or_else(|| "the computer closed the connection".to_owned())
    }
}

/// Hands a reply to the waiting `say`; returns a line to show for job output and failures.
async fn route(message: &Envelope, replies: &mpsc::Sender<Reply>) -> Option<String> {
    match message.kind {
        Kind::Reply => {
            if let Ok(reply) = message.body::<Reply>() {
                let _ = replies.send(reply).await;
            }
            None
        }
        Kind::JobUpdate => match message.body::<JobUpdate>().ok()? {
            JobUpdate::Output { text, .. } => Some(text),
            JobUpdate::State { error, .. } => error,
        },
        _ => None,
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
