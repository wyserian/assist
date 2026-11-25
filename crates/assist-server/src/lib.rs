//! The assist daemon's front door (PLAN.md sections 3.3, 3.15 and 9).
//!
//! One WebSocket endpoint (`/ws`) speaks the [`assist_proto`] envelopes; `/auth` swaps a
//! one-time code for a browser session. Every request passes [`guard`] first: the `Host` and
//! `Origin` checks, then strict security headers on the response. Each connection has its
//! own dialog, so questions and confirmations never mix between clients.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use assist_core::audit::Audit;
use assist_core::dialog::LiveEngine;
use assist_core::facts::Facts;
use assist_core::scheduler::Scheduler;
use assist_core::secrets::Secrets;
use assist_core::store::Store;
use assist_proto::rpc::{ClientEntry, Otp, Waiting};
use assist_proto::{Reply, MAX_FRAME};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_SECURITY_POLICY, CONTENT_TYPE, REFERRER_POLICY, SET_COOKIE,
    X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

mod auth;
mod conn;
mod devices;
mod nodes;
mod rpc;

pub use auth::{load_or_create_token, Credential};
use auth::{Auth, Refusal};
pub use devices::{proxies, Devices};
pub use nodes::Nodes;

const INDEX_HTML: &str = include_str!("../../../web/index.html");
const APP_JS: &str = include_str!("../../../web/app.js");
const STYLE_CSS: &str = include_str!("../../../web/style.css");

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; \
                   connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// What the daemon serves.
pub struct Services {
    /// Runs jobs.
    pub scheduler: Scheduler,
    /// Matcher, policy and available commands; replaced when phrases change.
    pub engine: Arc<LiveEngine>,
    /// Current device state.
    pub facts: Facts,
    /// Proactive messages for connected clients.
    pub notices: tokio::sync::broadcast::Sender<assist_proto::Notice>,
    /// The user phrase file that `phrases.add` writes to.
    pub user_phrases: Option<PathBuf>,
    /// The config file that `settings.set` edits.
    pub config_file: Option<PathBuf>,
    /// Job history.
    pub store: Store,
    /// Audit log, for the dashboard.
    pub audit: Option<Audit>,
    /// Secrets, if a keystore is available.
    pub secrets: Option<Arc<Secrets>>,
    /// Working directory for shell jobs of clients that do not send one.
    pub default_cwd: PathBuf,
    /// Where the token file lives.
    pub data_dir: PathBuf,
    /// Paired devices over iroh; `None` when the network could not start.
    pub nodes: Option<Nodes>,
}

/// Serves `listener` until `shutdown` completes. The listener must be on a loopback address.
pub async fn serve(
    services: Services,
    listener: TcpListener,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let port = listener.local_addr()?.port();
    let auth = Auth::open(&services.data_dir)?;
    let stop = CancellationToken::new();
    let state = AppState(Arc::new(Shared {
        services,
        auth,
        port,
        clients: Mutex::new(BTreeMap::new()),
        restart_pending: std::sync::atomic::AtomicBool::new(false),
        next_client: AtomicU64::new(1),
        stop: stop.clone(),
    }));
    tokio::spawn(nodes::accept(state.clone(), stop.clone()));
    let app = Router::new()
        .route("/", get(|| page(INDEX_HTML, "text/html; charset=utf-8")))
        .route(
            "/app.js",
            get(|| page(APP_JS, "text/javascript; charset=utf-8")),
        )
        .route(
            "/style.css",
            get(|| page(STYLE_CSS, "text/css; charset=utf-8")),
        )
        .route("/ws", get(websocket))
        .route("/auth", post(sign_in))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            // Close every WebSocket too; graceful shutdown waits for them.
            stop.cancel();
        })
        .await
}

/// Shared by every request handler.
#[derive(Clone)]
pub(crate) struct AppState(Arc<Shared>);

pub(crate) struct Shared {
    pub(crate) services: Services,
    pub(crate) auth: Auth,
    port: u16,
    clients: Mutex<BTreeMap<u64, Client>>,
    /// A saved setting waits for a daemon restart.
    pub(crate) restart_pending: std::sync::atomic::AtomicBool,
    next_client: AtomicU64,
    stop: CancellationToken,
}

struct Client {
    entry: ClientEntry,
    cancel: CancellationToken,
    session: Option<String>,
    /// Answers typed for this client elsewhere (the dashboard's Waiting tab).
    inbox: mpsc::Sender<Answer>,
    /// The question this client's dialog is waiting on, and when it was asked.
    question: Option<(String, u64)>,
}

/// An answer to a client's question, given elsewhere; the client sends back its reply.
pub(crate) struct Answer {
    pub(crate) text: String,
    pub(crate) reply: oneshot::Sender<Reply>,
}

/// Answers queued per client before it has handled them.
const INBOX: usize = 8;

impl std::ops::Deref for AppState {
    type Target = Shared;
    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl Shared {
    fn clients(&self) -> MutexGuard<'_, BTreeMap<u64, Client>> {
        self.clients.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Registers a connection; its token is cancelled on revoke or shutdown, and answers
    /// given for it elsewhere arrive on the receiver.
    pub(crate) fn connect(
        &self,
        credential: &Credential,
    ) -> (u64, CancellationToken, mpsc::Receiver<Answer>) {
        let id = self.next_client.fetch_add(1, Ordering::Relaxed);
        let cancel = self.stop.child_token();
        let (kind, session) = match credential {
            Credential::Token => ("cli", None),
            Credential::Session(session) => ("web", Some(session.clone())),
            Credential::Node { .. } => ("node", None),
        };
        let entry = ClientEntry {
            id,
            kind: kind.to_owned(),
            client: String::new(),
            connected: assist_proto::now_ms(),
        };
        let (inbox, answers) = mpsc::channel(INBOX);
        self.clients().insert(
            id,
            Client {
                entry,
                cancel: cancel.clone(),
                session,
                inbox,
                question: None,
            },
        );
        (id, cancel, answers)
    }

    /// Records the question a client waits on (`None` when it waits on nothing).
    pub(crate) fn set_question(&self, id: u64, question: Option<String>) {
        if let Some(client) = self.clients().get_mut(&id) {
            client.question = question.map(|q| (q, assist_proto::now_ms()));
        }
    }

    /// Every client's open question.
    pub(crate) fn waiting(&self) -> Vec<Waiting> {
        self.clients()
            .values()
            .filter_map(|client| {
                let (question, asked) = client.question.clone()?;
                Some(Waiting {
                    client: client.entry.id,
                    kind: client.entry.kind.clone(),
                    program: client.entry.client.clone(),
                    question,
                    asked,
                })
            })
            .collect()
    }

    /// Hands `text` to client `id` as the answer to its question; `None` if it has none or
    /// its inbox is full.
    pub(crate) fn answer(&self, id: u64, text: String) -> Option<oneshot::Receiver<Reply>> {
        let clients = self.clients();
        let client = clients.get(&id).filter(|c| c.question.is_some())?;
        let (reply, receive) = oneshot::channel();
        client.inbox.try_send(Answer { text, reply }).ok()?;
        Some(receive)
    }

    pub(crate) fn name_client(&self, id: u64, name: &str) {
        if let Some(client) = self.clients().get_mut(&id) {
            client.entry.client = name.chars().take(64).collect();
        }
    }

    pub(crate) fn disconnect(&self, id: u64) {
        self.clients().remove(&id);
    }

    pub(crate) fn client_list(&self) -> Vec<ClientEntry> {
        self.clients().values().map(|c| c.entry.clone()).collect()
    }

    /// Closes a connection and ends its browser session.
    pub(crate) fn revoke(&self, id: u64) -> bool {
        let Some(client) = self.clients().remove(&id) else {
            return false;
        };
        client.cancel.cancel();
        if let Some(session) = client.session {
            self.auth.end_session(&session);
        }
        true
    }
}

/// Refuses requests that do not name this daemon or come from another site, and adds
/// security headers to every response.
async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    if !auth::host_ok(headers, state.port) {
        return (StatusCode::MISDIRECTED_REQUEST, "wrong host").into_response();
    }
    if !auth::origin_ok(headers, state.port) {
        return (StatusCode::FORBIDDEN, "cross-site request").into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// A file of the web UI. The page holds no data; everything comes over the authenticated
/// WebSocket, so it is served without sign-in.
async fn page(body: &'static str, content_type: &'static str) -> Response {
    ([(CONTENT_TYPE, content_type)], body).into_response()
}

async fn websocket(
    State(state): State<AppState>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    match state.auth.check(&headers) {
        Ok(credential) => upgrade
            .max_message_size(MAX_FRAME)
            .on_upgrade(move |socket| conn::run(state, socket, credential, None)),
        Err(refusal) => refused(refusal),
    }
}

/// Swaps a one-time code (`{"code": "..."}`) for a session cookie.
async fn sign_in(State(state): State<AppState>, Json(otp): Json<Otp>) -> Response {
    match state.auth.redeem(&otp.code) {
        Ok(session) => {
            let cookie = format!(
                "{}={session}; HttpOnly; SameSite=Strict; Path=/",
                auth::COOKIE_NAME
            );
            match HeaderValue::from_str(&cookie) {
                Ok(value) => (StatusCode::NO_CONTENT, [(SET_COOKIE, value)]).into_response(),
                Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            }
        }
        Err(refusal) => refused(refusal),
    }
}

fn refused(refusal: Refusal) -> Response {
    match refusal {
        Refusal::Unauthorized => {
            (StatusCode::UNAUTHORIZED, "sign in with: assist ui").into_response()
        }
        Refusal::TooMany => {
            (StatusCode::TOO_MANY_REQUESTS, "too many failed attempts").into_response()
        }
    }
}
