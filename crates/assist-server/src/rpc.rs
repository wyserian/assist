//! [`assist_proto::rpc`] methods. Blocking work (store, keystore) runs on `spawn_blocking`.

use assist_core::policy::Trust;
use assist_core::scheduler::JobInfo;
use assist_proto::rpc::{
    self, AuditPage, ClientRef, Explain, History, JobRef, Otp, SecretEntry, SecretName, SecretSet,
};
use assist_proto::{Rpc, RpcResult};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::conn::Conn;
use crate::Credential;

/// Bytes of an old job's log returned by `jobs.tail`.
const TAIL_BYTES: u64 = 64 * 1024;
/// Most jobs returned by one `jobs.history` call.
const MAX_HISTORY: usize = 500;

pub(crate) async fn call(conn: &mut Conn, call: Rpc) -> RpcResult {
    match dispatch(conn, &call.method, call.params).await {
        Ok(result) => result,
        Err(error) => error,
    }
}

async fn dispatch(conn: &mut Conn, method: &str, params: Value) -> Result<RpcResult, RpcResult> {
    let services = &conn.state.services;
    let scheduler = &services.scheduler;
    Ok(match method {
        rpc::JOBS_LIST => {
            let jobs: Vec<_> = scheduler.list().iter().map(JobInfo::summary).collect();
            RpcResult::ok(jobs)
        }
        rpc::JOBS_HISTORY => {
            let page: History = parse(params)?;
            let store = services.store.clone();
            let limit = page.limit.unwrap_or(50).min(MAX_HISTORY);
            let jobs = blocking(move || store.history(limit, page.before)).await?;
            RpcResult::ok(jobs)
        }
        rpc::JOBS_TAIL => {
            let job: JobRef = parse(params)?;
            match scheduler.tail(job.id) {
                Some(lines) => RpcResult::ok(lines),
                None => {
                    let store = services.store.clone();
                    RpcResult::ok(blocking(move || store.log_tail(job.id, TAIL_BYTES)).await?)
                }
            }
        }
        rpc::JOBS_STOP => {
            let job: JobRef = parse(params)?;
            RpcResult::ok(scheduler.cancel(job.id))
        }
        rpc::JOBS_STOP_ALL => RpcResult::ok(scheduler.cancel_all()),
        rpc::SECRETS_LIST => {
            let secrets = services.secrets.as_ref().ok_or_else(no_secrets)?;
            let entries: Vec<SecretEntry> = secrets
                .list()
                .into_iter()
                .map(|(name, info)| SecretEntry {
                    name,
                    set: info.set,
                    last4: info.last4,
                })
                .collect();
            RpcResult::ok(entries)
        }
        rpc::SECRETS_SET => {
            require_local(conn)?;
            let secret: SecretSet = parse(params)?;
            let secrets = services.secrets.clone().ok_or_else(no_secrets)?;
            blocking(move || secrets.set(&secret.name, &secret.value)).await?;
            RpcResult::ok(true)
        }
        rpc::SECRETS_DELETE => {
            require_local(conn)?;
            let secret: SecretName = parse(params)?;
            let secrets = services.secrets.clone().ok_or_else(no_secrets)?;
            RpcResult::ok(blocking(move || secrets.delete(&secret.name)).await?)
        }
        rpc::COMMANDS_EXPLAIN => {
            let explain: Explain = parse(params)?;
            RpcResult::ok(services.engine.explain(&conn.principal, &explain.text))
        }
        rpc::CLIENTS_LIST => RpcResult::ok(conn.state.client_list()),
        rpc::CLIENTS_REVOKE => {
            require_local(conn)?;
            let client: ClientRef = parse(params)?;
            RpcResult::ok(conn.state.revoke(client.id))
        }
        rpc::AUDIT_TAIL => {
            let page: AuditPage = parse(params)?;
            let audit = services
                .audit
                .clone()
                .ok_or_else(|| RpcResult::err("unavailable", "this daemon has no audit log"))?;
            let limit = page.limit.unwrap_or(100).min(MAX_HISTORY);
            RpcResult::ok(blocking(move || audit.tail(limit)).await?)
        }
        rpc::AUTH_OTP => {
            // Only the token holder can let a browser in.
            if conn.credential != Credential::Token {
                return Err(RpcResult::err(
                    "denied",
                    "only the CLI can create sign-in codes",
                ));
            }
            let code = conn
                .state
                .auth
                .new_otp()
                .map_err(|err| RpcResult::err("internal", err.to_string()))?;
            RpcResult::ok(Otp { code })
        }
        other => RpcResult::err("unknown_method", format!("no method {other}")),
    })
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcResult> {
    serde_json::from_value(params).map_err(|err| RpcResult::err("bad_request", err.to_string()))
}

fn require_local(conn: &Conn) -> Result<(), RpcResult> {
    if conn.principal.trust == Trust::Local {
        Ok(())
    } else {
        Err(RpcResult::err("denied", "only local clients can do this"))
    }
}

fn no_secrets() -> RpcResult {
    RpcResult::err("unavailable", "no keystore on this machine")
}

/// Runs blocking work off the async runtime; errors become `internal` results.
async fn blocking<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, RpcResult>
where
    T: Send + 'static,
    E: std::fmt::Display + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(RpcResult::err("failed", err.to_string())),
        Err(err) => Err(RpcResult::err("internal", err.to_string())),
    }
}
