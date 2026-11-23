//! "search rust tokio": opens a web search in the default browser.
//!
//! shortcut: always DuckDuckGo; make the engine configurable when someone asks.

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest, Output,
};
use assist_core::policy::Capability;

/// Opens web searches.
pub struct Search;

static MANIFEST: Manifest = Manifest {
    name: "search",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
};

impl Action for Search {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let query = ctx.plan.slots.get("query").cloned().unwrap_or_default();
            let url = search_url(&query);
            tokio::task::spawn_blocking(move || open::that_detached(url))
                .await
                .map_err(|err| ActionError::new(err.to_string()))?
                .map_err(|err| ActionError::new(format!("cannot open the browser: {err}")))?;
            let _ = out
                .send(Chunk::Info(format!("Searching for {query}")))
                .await;
            Ok(())
        })
    }
}

fn search_url(query: &str) -> String {
    let mut url = String::from("https://duckduckgo.com/?q=");
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                url.push(char::from(byte));
            }
            b' ' => url.push('+'),
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_is_url_encoded() {
        assert_eq!(
            search_url("rust tokio"),
            "https://duckduckgo.com/?q=rust+tokio"
        );
        assert_eq!(
            search_url("c++ & é?"),
            "https://duckduckgo.com/?q=c%2B%2B+%26+%C3%A9%3F"
        );
    }
}
