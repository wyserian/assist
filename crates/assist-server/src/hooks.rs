//! Incoming webhooks (PLAN.md section 3.13): `POST /hook/<name>` with
//! `Authorization: Bearer <token>` starts the saved pipe `[webhooks] <name>` names. A hook is
//! a trigger, like a schedule or a folder watch: the caller decides when, never what. The
//! request body is not read, so nothing in it can become a command; the pipe must be marked
//! `unattended` and run without any confirmation. Each hook's token is the secret
//! `webhook_<name>`. The daemon listens on loopback only, so callers run on this machine.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use assist_core::policy::Principal;
use assist_core::secrets::constant_time_eq;
use axum::extract::{Path, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::AppState;

/// Shortest time between two calls of one hook, so a stuck caller or a guessed token cannot
/// run it in a loop.
const MIN_GAP: Duration = Duration::from_secs(1);

/// When each hook was last called.
#[derive(Default)]
pub(crate) struct Calls(Mutex<HashMap<String, Instant>>);

impl Calls {
    /// Records a call of `name`; false if the last one was too recent.
    fn admit(&self, name: &str) -> bool {
        let mut calls = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if calls
            .get(name)
            .is_some_and(|last| now.duration_since(*last) < MIN_GAP)
        {
            return false;
        }
        calls.insert(name.to_owned(), now);
        true
    }
}

/// The secret that holds hook `name`'s token.
pub fn token_secret(name: &str) -> String {
    format!("webhook_{name}")
}

/// `POST /hook/<name>`.
pub(crate) async fn call(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let engine = state.services.engine.get();
    let Some(hook) = engine.config().webhooks.get(&name) else {
        return answer(StatusCode::NOT_FOUND, json!({ "error": "no such hook" }));
    };
    let run = hook.run.clone();
    // Counted before the token check, so guessing tokens is as slow as calling.
    if !state.hook_calls.admit(&name) {
        return answer(
            StatusCode::TOO_MANY_REQUESTS,
            json!({ "error": "called again too soon" }),
        );
    }
    let given = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim()
        .to_owned();
    match expected_token(&state, &name).await {
        Some(token)
            if !given.is_empty() && constant_time_eq(given.as_bytes(), token.as_bytes()) => {}
        Some(_) => return answer(StatusCode::UNAUTHORIZED, json!({ "error": "wrong token" })),
        None => {
            println!(
                "Webhook {name}: no token yet; set one with: assist secret set {}",
                token_secret(&name)
            );
            return answer(StatusCode::UNAUTHORIZED, json!({ "error": "wrong token" }));
        }
    }
    let source = format!("webhook:{name}");
    let principal = Principal::local(&source);
    match engine.unattended(&run, &principal) {
        Ok(Some(pipe)) => {
            let submission = engine.submission(pipe);
            let cwd = state.services.default_cwd.clone();
            match state
                .services
                .scheduler
                .submit_pipe(submission, principal, cwd)
            {
                Ok(jobs) => answer(StatusCode::ACCEPTED, json!({ "run": run, "jobs": jobs })),
                Err(err) => answer(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({ "error": err.to_string() }),
                ),
            }
        }
        Ok(None) => answer(
            StatusCode::OK,
            json!({ "run": run, "skipped": "its if does not hold" }),
        ),
        Err(reason) => answer(StatusCode::CONFLICT, json!({ "error": reason })),
    }
}

/// Hook `name`'s token, if one is set.
async fn expected_token(state: &AppState, name: &str) -> Option<String> {
    let secrets = state.services.secrets.clone()?;
    let secret = token_secret(name);
    tokio::task::spawn_blocking(move || secrets.get(&secret))
        .await
        .ok()?
        .ok()
        .flatten()
}

fn answer(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hook_is_admitted_at_most_once_a_second() {
        let calls = Calls::default();
        assert!(calls.admit("backup"));
        assert!(!calls.admit("backup"));
        assert!(calls.admit("other"));
    }
}
