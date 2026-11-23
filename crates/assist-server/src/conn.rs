//! One WebSocket connection: its own dialog, working directory and job subscriptions.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use assist_core::action::Chunk;
use assist_core::dialog::Dialog;
use assist_core::policy::Principal;
use assist_core::scheduler::{JobEvent, JobId, JobState};
use assist_proto::{
    Envelope, Hello, JobBrief, JobUpdate, Kind, Reply, Rpc, RpcResult, Stream, Subscribe, Utterance,
};
use axum::extract::ws::{Message, WebSocket};
use tokio::sync::broadcast::error::RecvError;

use crate::{rpc, AppState, Credential};

/// Name this side uses in `from`.
pub(crate) const HUB: &str = "hub";

pub(crate) struct Conn {
    pub(crate) state: AppState,
    pub(crate) id: u64,
    pub(crate) credential: Credential,
    pub(crate) principal: Principal,
    dialog: Dialog,
    peer: String,
    cwd: PathBuf,
    /// All job updates, not only this connection's own jobs.
    subscribed: bool,
    jobs: HashSet<JobId>,
}

pub(crate) async fn run(state: AppState, mut socket: WebSocket, credential: Credential) {
    let (id, cancel) = state.connect(&credential);
    let source = match credential {
        Credential::Token => "cli",
        Credential::Session(_) => "web",
    };
    let principal = Principal::local(source);
    let mut events = state.services.scheduler.subscribe();
    let mut conn = Conn {
        dialog: Dialog::new(state.services.engine.clone(), principal.clone()),
        cwd: state.services.default_cwd.clone(),
        state,
        id,
        credential,
        principal,
        peer: source.to_owned(),
        subscribed: false,
        jobs: HashSet::new(),
    };
    loop {
        let outgoing = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            frame = socket.recv() => match frame {
                Some(Ok(Message::Text(text))) => conn.handle(text.as_str()).await,
                // Pings are answered by axum; binary frames are not part of the protocol.
                Some(Ok(Message::Binary(_) | Message::Ping(_) | Message::Pong(_))) => Vec::new(),
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
            event = events.recv() => match event {
                Ok(event) => conn.event(event).into_iter().collect(),
                Err(RecvError::Lagged(_)) => Vec::new(),
                Err(RecvError::Closed) => break,
            },
        };
        for envelope in outgoing {
            if socket
                .send(Message::Text(envelope.encode().into()))
                .await
                .is_err()
            {
                conn.state.disconnect(conn.id);
                return;
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
    conn.state.disconnect(conn.id);
}

impl Conn {
    /// Answers one text frame.
    async fn handle(&mut self, frame: &str) -> Vec<Envelope> {
        let message = match Envelope::decode(frame) {
            Ok(message) => message,
            Err(err) => {
                let error = RpcResult::err("bad_request", err.to_string());
                return vec![Envelope::new(Kind::RpcResult, HUB, &self.peer, error)];
            }
        };
        self.peer = message.from.chars().take(64).collect();
        match message.kind {
            Kind::Hello => match message.body::<Hello>() {
                Ok(hello) => vec![self.hello(&message, hello)],
                Err(err) => vec![bad_request(&message, &err)],
            },
            Kind::Utterance => match message.body::<Utterance>() {
                Ok(utterance) => vec![self.utterance(&message, &utterance.text)],
                Err(err) => vec![bad_request(&message, &err)],
            },
            Kind::Subscribe => match message.body::<Subscribe>() {
                Ok(subscribe) => {
                    self.subscribed = subscribe.topics.iter().any(|t| t == "jobs");
                    Vec::new()
                }
                Err(err) => vec![bad_request(&message, &err)],
            },
            Kind::Rpc => match message.body::<Rpc>() {
                Ok(call) => {
                    let result = rpc::call(self, call).await;
                    vec![message.answer(Kind::RpcResult, HUB, result)]
                }
                Err(err) => vec![bad_request(&message, &err)],
            },
            // Only the hub sends these; unknown kinds are ignored.
            Kind::Reply | Kind::JobUpdate | Kind::RpcResult | Kind::Unknown => Vec::new(),
        }
    }

    fn hello(&mut self, message: &Envelope, hello: Hello) -> Envelope {
        self.state.name_client(self.id, &hello.client);
        if let Some(cwd) = hello.cwd.map(PathBuf::from).filter(|p| p.is_dir()) {
            self.cwd = cwd;
        }
        let ours = Hello {
            client: "assist-server".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            cwd: None,
        };
        message.answer(Kind::Hello, HUB, ours)
    }

    fn utterance(&mut self, message: &Envelope, text: &str) -> Envelope {
        let reply = self.dialog.handle(&self.principal, text, Instant::now());
        let scheduler = &self.state.services.scheduler;
        let mut answer = Reply {
            say: reply.say,
            jobs: Vec::new(),
        };
        for plan in reply.run {
            let title = self.state.services.engine.readback(&plan);
            match scheduler.submit(plan, self.principal.clone(), title, self.cwd.clone()) {
                Ok(id) => {
                    self.jobs.insert(id);
                    // The scheduler's title has secrets redacted.
                    let title = scheduler.get(id).map(|job| job.title).unwrap_or_default();
                    answer.jobs.push(JobBrief { id, title });
                }
                Err(err) => answer.say.push(err.to_string()),
            }
        }
        message.answer(Kind::Reply, HUB, answer)
    }

    /// The update to send for a scheduler event, if this connection wants it.
    fn event(&mut self, event: JobEvent) -> Option<Envelope> {
        let id = match &event {
            JobEvent::State { id, .. } | JobEvent::Output { id, .. } => *id,
        };
        let own = self.jobs.contains(&id);
        if !own && !self.subscribed {
            return None;
        }
        let update = match event {
            JobEvent::State { id, state } => {
                if state.is_finished() {
                    self.jobs.remove(&id);
                }
                let error = (state == JobState::Failed)
                    .then(|| self.state.services.scheduler.get(id))
                    .flatten()
                    .and_then(|job| job.error)
                    .map(|err| err.message);
                JobUpdate::State { id, state, error }
            }
            JobEvent::Output { id, chunk } => {
                let (stream, text) = match chunk {
                    Chunk::Line(text) => (Stream::Out, text),
                    Chunk::Error(text) => (Stream::Err, text),
                    Chunk::Info(text) => (Stream::Info, text),
                    Chunk::Cwd(path) => {
                        if own {
                            self.cwd = path;
                        }
                        return None;
                    }
                };
                JobUpdate::Output { id, stream, text }
            }
        };
        Some(Envelope::new(Kind::JobUpdate, HUB, &self.peer, update))
    }
}

fn bad_request(message: &Envelope, err: &serde_json::Error) -> Envelope {
    message.answer(
        Kind::RpcResult,
        HUB,
        RpcResult::err("bad_request", err.to_string()),
    )
}
