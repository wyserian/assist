//! Bulk rename with a preview and undo (PLAN.md section 3.9):
//!
//! - `rename IMG_ to trip_ in ~/Pictures` replaces text in the names of the files in one
//!   folder. The confirmation lists every change; the run refuses if the folder changed.
//! - `undo` reverses the last rename (or `undo 12` the rename of job 12), with its own
//!   preview and confirmation.
//!
//! Each rename writes an undo record to `<data dir>/undo/<job>.json`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use assist_core::action::{
    matches_preview, Action, ActionError, BoxFuture, Chunk, DataKind, Input, JobCtx, Lane,
    Manifest, Output, PipeShape,
};
use assist_core::config::Config;
use assist_core::dialog::Plan;
use assist_core::policy::Capability;
use assist_core::private_fs;
use serde::{Deserialize, Serialize};

/// Most files one rename may touch.
const MAX_FILES: usize = 1000;
const UNDO_DIR: &str = "undo";

const SHAPE: PipeShape = PipeShape {
    input: DataKind::Empty,
    output: DataKind::Lines,
    arg: Some("spec"),
    untrusted: false,
};

/// Rename and undo.
pub fn all() -> Vec<Arc<dyn Action>> {
    vec![Arc::new(Rename), Arc::new(Undo)]
}

/// `rename <find> [to] <replace> in <folder>`.
pub struct Rename;

static RENAME: Manifest = Manifest {
    name: "rename",
    capabilities: &[Capability::RenameBulk],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: SHAPE,
};

/// `undo [job]`: reverses a rename.
pub struct Undo;

static UNDO: Manifest = Manifest {
    name: "undo",
    capabilities: &[Capability::RenameBulk],
    owner_defined: false,
    lane: Lane::Interactive,
    pipe: SHAPE,
};

/// One file's change, as full paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Change {
    from: PathBuf,
    to: PathBuf,
}

/// What `undo` needs to reverse a rename.
#[derive(Debug, Serialize, Deserialize)]
struct UndoRecord {
    job: u64,
    changes: Vec<Change>,
}

impl Action for Rename {
    fn manifest(&self) -> &Manifest {
        &RENAME
    }

    fn preview(
        &self,
        plan: &Plan,
        _config: &Config,
        _data_dir: Option<&Path>,
    ) -> Option<Result<Vec<String>, String>> {
        Some(rename_changes(plan).map(|changes| lines(&changes)))
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let job = ctx.job;
            let data = ctx.data_dir.clone();
            let plan = ctx.plan.clone();
            let done = blocking(move || {
                let changes = rename_changes(&plan)?;
                check_preview(&plan, &changes)?;
                let done = apply(&changes)?;
                if let Some(data) = data {
                    save_undo(&data, job, &done)?;
                }
                Ok(done)
            })
            .await?;
            send_lines(&out, &done).await;
            Ok(())
        })
    }
}

impl Action for Undo {
    fn manifest(&self) -> &Manifest {
        &UNDO
    }

    fn preview(
        &self,
        plan: &Plan,
        _config: &Config,
        data_dir: Option<&Path>,
    ) -> Option<Result<Vec<String>, String>> {
        Some(undo_changes(plan, data_dir).map(|(_, changes)| lines(&changes)))
    }

    fn run(&self, ctx: JobCtx, _input: Input, out: Output) -> BoxFuture<Result<(), ActionError>> {
        Box::pin(async move {
            let plan = ctx.plan.clone();
            let data = ctx.data_dir.clone();
            let done = blocking(move || {
                let (record, changes) = undo_changes(&plan, data.as_deref())?;
                check_preview(&plan, &changes)?;
                let done = apply(&changes)?;
                let _ = fs::remove_file(record);
                Ok(done)
            })
            .await?;
            send_lines(&out, &done).await;
            Ok(())
        })
    }
}

/// Runs file work off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, ActionError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| ActionError::new(err.to_string()))?
        .map_err(ActionError::new)
}

async fn send_lines(out: &Output, changes: &[Change]) {
    for line in lines(changes) {
        if out.send(Chunk::Line(line)).await.is_err() {
            break;
        }
    }
}

/// `old -> new` per change, by file name.
fn lines(changes: &[Change]) -> Vec<String> {
    let name = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    changes
        .iter()
        .map(|c| format!("{} -> {}", name(&c.from), name(&c.to)))
        .collect()
}

fn check_preview(plan: &Plan, changes: &[Change]) -> Result<(), String> {
    if matches_preview(plan, &lines(changes)) {
        Ok(())
    } else {
        Err("the files changed since you confirmed; run it again".to_owned())
    }
}

/// Renames in order; stops at the first failure and reports what was already done.
fn apply(changes: &[Change]) -> Result<Vec<Change>, String> {
    let mut done = Vec::with_capacity(changes.len());
    for change in changes {
        if change.to.exists() {
            return Err(format!(
                "{} already exists; renamed {} of {}",
                change.to.display(),
                done.len(),
                changes.len()
            ));
        }
        fs::rename(&change.from, &change.to).map_err(|err| {
            format!(
                "{}: {err}; renamed {} of {}",
                change.from.display(),
                done.len(),
                changes.len()
            )
        })?;
        done.push(change.clone());
    }
    Ok(done)
}

/// The renames `rename` would make, sorted by name, refusing clashes.
fn rename_changes(plan: &Plan) -> Result<Vec<Change>, String> {
    let spec = Spec::parse(plan.slots.get("spec").map_or("", String::as_str))?;
    let entries = fs::read_dir(&spec.folder)
        .map_err(|err| format!("cannot read {}: {err}", spec.folder.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect();
    files.sort();
    let existing: BTreeSet<PathBuf> = files.iter().cloned().collect();
    let mut targets = BTreeSet::new();
    let mut changes = Vec::new();
    for from in files {
        let Some(name) = from.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.contains(&spec.find) {
            continue;
        }
        let new = name.replace(&spec.find, &spec.replace);
        if new.is_empty() || new == name || new.contains(['/', '\\']) {
            continue;
        }
        let to = spec.folder.join(&new);
        if (existing.contains(&to) && !changes.iter().any(|c: &Change| c.from == to))
            || !targets.insert(to.clone())
        {
            return Err(format!("{new} would exist twice; nothing renamed"));
        }
        changes.push(Change { from, to });
        if changes.len() > MAX_FILES {
            return Err(format!("more than {MAX_FILES} files; narrow it down"));
        }
    }
    // A target that is also a source must be renamed away before it is reused.
    if changes
        .iter()
        .any(|c| changes.iter().any(|d| d.from == c.to))
    {
        return Err("names would swap; rename in two steps".to_owned());
    }
    Ok(changes)
}

/// The newest undo record (or the one for `undo <job>`) and its reverse changes.
fn undo_changes(plan: &Plan, data: Option<&Path>) -> Result<(PathBuf, Vec<Change>), String> {
    let dir = data
        .ok_or("undo records need the data directory")?
        .join(UNDO_DIR);
    let wanted = plan
        .slots
        .get("spec")
        .map(|s| s.trim().trim_start_matches('#').to_owned());
    let record = match wanted.filter(|w| !w.is_empty()) {
        Some(job) => {
            let job: u64 = job
                .parse()
                .map_err(|_| format!("undo {job:?}: give a job number"))?;
            dir.join(format!("{job}.json"))
        }
        None => newest(&dir).ok_or("nothing to undo")?,
    };
    let text =
        fs::read_to_string(&record).map_err(|_| "nothing to undo for that job".to_owned())?;
    let undo: UndoRecord =
        serde_json::from_str(&text).map_err(|err| format!("damaged undo record: {err}"))?;
    let changes = undo
        .changes
        .into_iter()
        .rev()
        .map(|c| Change {
            from: c.to,
            to: c.from,
        })
        .collect();
    Ok((record, changes))
}

fn newest(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            let job: u64 = e.path().file_stem()?.to_str()?.parse().ok()?;
            Some((job, e.path()))
        })
        .max_by_key(|(job, _)| *job)
        .map(|(_, path)| path)
}

fn save_undo(data: &Path, job: u64, done: &[Change]) -> Result<(), String> {
    let dir = data.join(UNDO_DIR);
    private_fs::create_dir(&dir).map_err(|err| err.to_string())?;
    let record = UndoRecord {
        job,
        changes: done.to_vec(),
    };
    let json = serde_json::to_vec(&record).map_err(|err| err.to_string())?;
    private_fs::write_atomic(&dir.join(format!("{job}.json")), &json)
        .map_err(|err| format!("renamed, but the undo record failed: {err}"))
}

/// `<find> [to] <replace> in <folder>`; quote a value with spaces, `""` for nothing.
struct Spec {
    find: String,
    replace: String,
    folder: PathBuf,
}

impl Spec {
    fn parse(text: &str) -> Result<Self, String> {
        let usage = || "use: rename <find> to <replace> in <folder>".to_owned();
        let words = split_quoted(text).ok_or_else(usage)?;
        let at = words.iter().position(|w| w == "in").ok_or_else(usage)?;
        let (before, after) = words.split_at(at);
        let (find, replace) = match before {
            [find, to, replace] if to == "to" => (find, replace),
            [find, replace] => (find, replace),
            _ => return Err(usage()),
        };
        if find.is_empty() {
            return Err("give the text to find".to_owned());
        }
        let folder = crate::expand_home(&after[1..].join(" "));
        if !folder.is_absolute() {
            return Err("give the full folder path, e.g. ~/Pictures".to_owned());
        }
        Ok(Self {
            find: find.clone(),
            replace: replace.clone(),
            folder,
        })
    }
}

/// Words split on spaces, with `"..."` or `'...'` kept whole; `None` for an open quote.
fn split_quoted(text: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '"' || c == '\'' {
            chars.next();
            let mut word = String::new();
            loop {
                match chars.next()? {
                    q if q == c => break,
                    ch => word.push(ch),
                }
            }
            words.push(word);
        } else {
            let mut word = String::new();
            while let Some(&ch) = chars.peek().filter(|ch| !ch.is_whitespace()) {
                word.push(ch);
                chars.next();
            }
            words.push(word);
        }
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ctx, run};
    use assist_core::action::PREVIEW_SLOT;

    fn temp(name: &str, files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("assist-rename-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for f in files {
            fs::write(dir.join(f), "x").unwrap();
        }
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn plan(command: &str, spec: &str) -> Plan {
        Plan {
            command: command.to_owned(),
            slots: [("spec".to_owned(), spec.to_owned())].into_iter().collect(),
            typed: true,
        }
    }

    #[test]
    fn parses_specs() {
        let folder = std::env::temp_dir().join("My Pics");
        let s = Spec::parse(&format!("IMG_ to \"trip \" in {}", folder.display())).unwrap();
        assert_eq!((s.find.as_str(), s.replace.as_str()), ("IMG_", "trip "));
        assert_eq!(s.folder, folder);
        assert!(Spec::parse("IMG_ trip_").is_err());
        assert!(Spec::parse("IMG_ to x in relative").is_err());
        assert!(Spec::parse("\"open to x in /x").is_err());
    }

    #[tokio::test]
    async fn rename_previews_applies_and_undoes() {
        let dir = temp("apply", &["IMG_1.jpg", "IMG_2.jpg", "notes.txt"]);
        let data = temp("data", &[]);
        let spec = format!("IMG_ to trip_ in {}", dir.display());
        let preview = Rename
            .preview(
                &plan("rename", &spec),
                &Config::from_toml("").unwrap(),
                None,
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            preview,
            ["IMG_1.jpg -> trip_1.jpg", "IMG_2.jpg -> trip_2.jpg"]
        );

        let slots = [("spec", spec.as_str()), (PREVIEW_SLOT, &preview.join("\n"))];
        let mut job = ctx("rename", &slots, "", dir.clone());
        job.data_dir = Some(data.clone());
        let (result, out) = run(&Rename, job).await;
        result.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(names(&dir), ["notes.txt", "trip_1.jpg", "trip_2.jpg"]);

        let undo = plan("undo", "");
        let back = Undo
            .preview(&undo, &Config::from_toml("").unwrap(), Some(&data))
            .unwrap()
            .unwrap();
        assert_eq!(back, ["trip_2.jpg -> IMG_2.jpg", "trip_1.jpg -> IMG_1.jpg"]);
        let slots = [("spec", ""), (PREVIEW_SLOT, &back.join("\n"))];
        let mut job = ctx("undo", &slots, "", dir.clone());
        job.data_dir = Some(data.clone());
        run(&Undo, job).await.0.unwrap();
        assert_eq!(names(&dir), ["IMG_1.jpg", "IMG_2.jpg", "notes.txt"]);
        assert!(names(&data.join(UNDO_DIR)).is_empty());
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_dir_all(&data).unwrap();
    }

    #[tokio::test]
    async fn changed_folder_or_clashes_rename_nothing() {
        let dir = temp("clash", &["a1.txt", "b1.txt", "IMG_3.jpg"]);
        let clash = format!("a to b in {}", dir.display());
        assert!(rename_changes(&plan("rename", &clash))
            .unwrap_err()
            .contains("twice"));

        let spec = format!("IMG_ to x_ in {}", dir.display());
        let slots = [("spec", spec.as_str()), (PREVIEW_SLOT, "stale -> preview")];
        let (result, _) = run(&Rename, ctx("rename", &slots, "", dir.clone())).await;
        assert!(result.unwrap_err().message.contains("changed since"));
        assert_eq!(names(&dir), ["IMG_3.jpg", "a1.txt", "b1.txt"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
