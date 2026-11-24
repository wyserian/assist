//! Built-in pipe transforms (PLAN.md section 3.11): `head 3`, `filter word`, `grep regex`,
//! `count`, `sort`, `uniq`, `save path`. Each reads the previous stage's lines.

use std::sync::Arc;

use assist_core::action::{
    Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane, Manifest, Output,
    PipeShape,
};
use assist_core::policy::Capability;
use tokio::io::{AsyncWriteExt, BufWriter};

/// `sort` holds every line in memory; more than this fails instead of growing.
const MAX_SORT_LINES: usize = 100_000;
/// Compiled size limit for a `grep` pattern.
const MAX_REGEX_BYTES: usize = 1 << 20;

#[derive(Debug, Clone, Copy)]
enum Kind {
    Head,
    Filter,
    Grep,
    Count,
    Sort,
    Uniq,
    Save,
}

/// One transform.
pub struct Transform {
    manifest: Manifest,
    kind: Kind,
}

const fn manifest(
    name: &'static str,
    capabilities: &'static [Capability],
    output: DataKind,
    arg: Option<&'static str>,
) -> Manifest {
    Manifest {
        name,
        capabilities,
        owner_defined: false,
        lane: Lane::Interactive,
        pipe: PipeShape::transform(output, arg),
    }
}

/// Every transform.
pub fn all() -> Vec<Arc<dyn Action>> {
    let lines = DataKind::Lines;
    [
        (manifest("head", &[], lines, Some("count")), Kind::Head),
        (manifest("filter", &[], lines, Some("text")), Kind::Filter),
        (manifest("grep", &[], lines, Some("pattern")), Kind::Grep),
        (manifest("count", &[], lines, None), Kind::Count),
        (manifest("sort", &[], lines, None), Kind::Sort),
        (manifest("uniq", &[], lines, None), Kind::Uniq),
        (
            manifest(
                "save",
                &[Capability::FsWrite],
                DataKind::Empty,
                Some("path"),
            ),
            Kind::Save,
        ),
    ]
    .into_iter()
    .map(|(manifest, kind)| Arc::new(Transform { manifest, kind }) as Arc<dyn Action>)
    .collect()
}

impl Action for Transform {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn run(&self, ctx: JobCtx, input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        let kind = self.kind;
        let name = self.manifest.name;
        let slot = self.manifest.pipe.arg;
        Box::pin(async move {
            let arg = slot
                .and_then(|slot| ctx.plan.slots.get(slot))
                .map(|value| value.trim().to_owned())
                .unwrap_or_default();
            if slot.is_some() && arg.is_empty() {
                return Err(ActionError::new(format!(
                    "{name} needs a value, e.g. {name} {}",
                    example(kind)
                )));
            }
            match kind {
                Kind::Head => head(input, &out, &arg).await,
                Kind::Filter => {
                    let word = arg.to_lowercase();
                    keep(input, &out, |line| line.to_lowercase().contains(&word)).await
                }
                Kind::Grep => {
                    let regex = regex::RegexBuilder::new(&arg)
                        .size_limit(MAX_REGEX_BYTES)
                        .build()
                        .map_err(|err| ActionError::new(format!("grep: {err}")))?;
                    keep(input, &out, |line| regex.is_match(line)).await
                }
                Kind::Count => count(input, &out).await,
                Kind::Sort => sort(input, &out).await,
                Kind::Uniq => uniq(input, &out).await,
                Kind::Save => save(input, &out, &ctx, &arg).await,
            }
        })
    }
}

fn example(kind: Kind) -> &'static str {
    match kind {
        Kind::Head => "3",
        Kind::Filter => "error",
        Kind::Grep => "^warn",
        Kind::Save => "out.txt",
        Kind::Count | Kind::Sort | Kind::Uniq => "",
    }
}

/// The next line of input; other chunks are skipped.
async fn next_line(input: &mut Input) -> Option<String> {
    loop {
        match input.recv().await? {
            Chunk::Line(line) => return Some(line),
            _ => continue,
        }
    }
}

/// Sends a line; `false` once the next stage is gone, so the caller stops early.
async fn send(out: &Output, line: String) -> bool {
    out.send(Chunk::Line(line)).await.is_ok()
}

async fn head(mut input: Input, out: &Output, arg: &str) -> Result<(), ActionError> {
    let n: usize = arg
        .parse()
        .map_err(|_| ActionError::new(format!("head: {arg:?} is not a number")))?;
    for _ in 0..n {
        let Some(line) = next_line(&mut input).await else {
            break;
        };
        if !send(out, line).await {
            break;
        }
    }
    // Dropping the input here tells earlier stages to stop.
    Ok(())
}

async fn keep(
    mut input: Input,
    out: &Output,
    keep: impl Fn(&str) -> bool,
) -> Result<(), ActionError> {
    while let Some(line) = next_line(&mut input).await {
        if keep(&line) && !send(out, line).await {
            break;
        }
    }
    Ok(())
}

async fn count(mut input: Input, out: &Output) -> Result<(), ActionError> {
    let mut n: u64 = 0;
    while next_line(&mut input).await.is_some() {
        n += 1;
    }
    send(out, n.to_string()).await;
    Ok(())
}

async fn sort(mut input: Input, out: &Output) -> Result<(), ActionError> {
    let mut lines = Vec::new();
    while let Some(line) = next_line(&mut input).await {
        if lines.len() == MAX_SORT_LINES {
            return Err(ActionError::new(format!(
                "sort: more than {MAX_SORT_LINES} lines"
            )));
        }
        lines.push(line);
    }
    lines.sort_unstable();
    for line in lines {
        if !send(out, line).await {
            break;
        }
    }
    Ok(())
}

/// Drops a line equal to the one before it, like Unix `uniq`.
async fn uniq(mut input: Input, out: &Output) -> Result<(), ActionError> {
    let mut last: Option<String> = None;
    while let Some(line) = next_line(&mut input).await {
        if last.as_deref() == Some(line.as_str()) {
            continue;
        }
        last = Some(line.clone());
        if !send(out, line).await {
            break;
        }
    }
    Ok(())
}

/// Writes the lines to a file, relative to the job's directory; replaces an existing file
/// (the policy asks before `fs.write`).
async fn save(mut input: Input, out: &Output, ctx: &JobCtx, arg: &str) -> Result<(), ActionError> {
    let path = ctx.cwd.join(arg);
    let error = |err: std::io::Error| ActionError::new(format!("save {}: {err}", path.display()));
    let file = tokio::fs::File::create(&path).await.map_err(error)?;
    let mut writer = BufWriter::new(file);
    let mut n: u64 = 0;
    while let Some(line) = next_line(&mut input).await {
        writer.write_all(line.as_bytes()).await.map_err(error)?;
        writer.write_all(b"\n").await.map_err(error)?;
        n += 1;
    }
    writer.flush().await.map_err(error)?;
    let _ = out
        .send(Chunk::Info(format!(
            "Saved {n} lines to {}",
            path.display()
        )))
        .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx, run_with_input};

    async fn apply(name: &str, arg: &str, lines: &[&str]) -> (Result<(), ActionError>, Vec<Chunk>) {
        let action = all()
            .into_iter()
            .find(|a| a.manifest().name == name)
            .unwrap();
        let slots: Vec<(&str, &str)> = action
            .manifest()
            .pipe
            .arg
            .map(|s| (s, arg))
            .into_iter()
            .collect();
        let ctx = ctx(name, &slots, "", std::env::temp_dir());
        run_with_input(action.as_ref(), ctx, lines).await
    }

    fn lines(chunks: &[Chunk]) -> Vec<&str> {
        chunks
            .iter()
            .filter_map(|c| match c {
                Chunk::Line(l) => Some(l.as_str()),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn line_transforms() {
        let input = ["b error", "a", "a", "c Error", "b error"];
        let (_, out) = apply("head", "2", &input).await;
        assert_eq!(lines(&out), ["b error", "a"]);
        let (_, out) = apply("filter", "ERROR", &input).await;
        assert_eq!(lines(&out), ["b error", "c Error", "b error"]);
        let (_, out) = apply("grep", "^[ab]", &input).await;
        assert_eq!(lines(&out), ["b error", "a", "a", "b error"]);
        let (_, out) = apply("count", "", &input).await;
        assert_eq!(lines(&out), ["5"]);
        let (_, out) = apply("sort", "", &input).await;
        assert_eq!(lines(&out), ["a", "a", "b error", "b error", "c Error"]);
        let (_, out) = apply("uniq", "", &input).await;
        assert_eq!(lines(&out), ["b error", "a", "c Error", "b error"]);
    }

    #[tokio::test]
    async fn bad_arguments_fail_with_a_hint() {
        let (result, _) = apply("head", "three", &["x"]).await;
        assert!(result.unwrap_err().message.contains("not a number"));
        let (result, _) = apply("grep", "(", &["x"]).await;
        assert!(result.unwrap_err().message.starts_with("grep:"));
        let (result, _) = apply("filter", "", &["x"]).await;
        assert_eq!(
            result.unwrap_err().message,
            "filter needs a value, e.g. filter error"
        );
    }

    #[tokio::test]
    async fn save_writes_the_lines() {
        let name = format!("assist-save-{}.txt", std::process::id());
        let (result, out) = apply("save", &name, &["one", "two"]).await;
        result.unwrap();
        let path = std::env::temp_dir().join(&name);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(text, "one\ntwo\n");
        assert!(matches!(&out[..], [Chunk::Info(info)] if info.starts_with("Saved 2 lines")));
    }
}
