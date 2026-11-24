//! "search rust tokio": opens a web search in the default browser, using `[search] url`.

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, Input, JobCtx, Lane, Manifest, Output, PipeShape,
};
use assist_core::policy::Capability;

/// Opens web searches.
pub struct Search;

static MANIFEST: Manifest = Manifest {
    name: "search",
    capabilities: &[Capability::Read],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: PipeShape::NONE,
};

impl Action for Search {
    fn manifest(&self) -> &Manifest {
        &MANIFEST
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let query = ctx.plan.slots.get("query").cloned().unwrap_or_default();
            let url = search_url(&ctx.config.search.url, &query);
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

/// `template` with `{query}` replaced by the URL-encoded query.
fn search_url(template: &str, query: &str) -> String {
    let mut encoded = String::with_capacity(query.len());
    for byte in query.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => encoded.push('+'),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    template.replacen("{query}", &encoded, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DDG: &str = "https://duckduckgo.com/?q={query}";

    #[test]
    fn query_is_url_encoded() {
        assert_eq!(
            search_url(DDG, "rust tokio"),
            "https://duckduckgo.com/?q=rust+tokio"
        );
        assert_eq!(
            search_url(DDG, "c++ & \u{e9}?"),
            "https://duckduckgo.com/?q=c%2B%2B+%26+%C3%A9%3F"
        );
        assert_eq!(
            search_url("http://localhost:8080/search?q={query}&lang=en", "a b"),
            "http://localhost:8080/search?q=a+b&lang=en"
        );
    }
}
