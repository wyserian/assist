//! Job history (PLAN.md section 3.16): SQLite `assist.db` plus one `jobs/<id>.log` per job.
//!
//! All writes go through one writer thread, in order, so the async runtime never blocks on
//! disk. Reads ([`Store::history`], [`Store::log_tail`]) are blocking and run on their own
//! read-only connection; call them from `spawn_blocking` in async code.
//!
//! On open, jobs still marked queued or running belong to a daemon that stopped, and become
//! `interrupted`. Jobs finished more than `[jobs] retention_days` ago are deleted, and the
//! oldest logs are deleted while all logs together exceed `[jobs] log_cap_mb`.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use assist_proto::{now_ms, JobState, JobSummary, Lane};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use crate::config::JobsConfig;
use crate::private_fs;
use crate::scheduler::{JobId, JobInfo};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS jobs (
    id INTEGER PRIMARY KEY,
    command TEXT NOT NULL,
    title TEXT NOT NULL,
    source TEXT NOT NULL,
    user TEXT NOT NULL,
    trust TEXT NOT NULL,
    lane TEXT NOT NULL,
    state TEXT NOT NULL,
    created INTEGER NOT NULL,
    started INTEGER,
    finished INTEGER,
    error TEXT,
    exit INTEGER
);
CREATE INDEX IF NOT EXISTS jobs_finished ON jobs (finished);
";

/// Why the store could not be opened or read.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The database failed.
    #[error("job database: {0}")]
    Db(#[from] rusqlite::Error),
    /// A file or directory failed.
    #[error("job store: {0}")]
    Io(#[from] io::Error),
}

/// What [`Store::open`] cleaned up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OpenReport {
    /// Jobs marked interrupted because the previous daemon stopped while they ran.
    pub interrupted: usize,
    /// Finished jobs deleted for being older than the retention period.
    pub expired: usize,
    /// Logs deleted to stay under the size cap.
    pub logs_trimmed: usize,
}

enum Op {
    Insert(Row),
    Update(Row),
    Output(JobId, String),
    Flush(mpsc::Sender<()>),
    Close(mpsc::Sender<()>),
}

/// Writes job history. Cheap to clone; all clones share one writer thread.
#[derive(Debug, Clone)]
pub struct Store {
    writes: mpsc::Sender<Op>,
    db_path: PathBuf,
    logs: PathBuf,
    next_id: JobId,
    last_error: Arc<Mutex<Option<String>>>,
}

impl Store {
    /// Opens (or creates) the store in `dir` and cleans up after the previous run.
    pub fn open(dir: &Path, limits: &JobsConfig) -> Result<(Self, OpenReport), StoreError> {
        let logs = dir.join("jobs");
        private_fs::create_dir(&logs)?;
        let db_path = dir.join("assist.db");
        // Created owner-only before SQLite opens it.
        drop(private_fs::open_append(&db_path)?);
        let db = Connection::open(&db_path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(SCHEMA)?;

        let now = to_db(now_ms());
        let interrupted = db.execute(
            "UPDATE jobs SET state = 'interrupted', finished = ?1
             WHERE state IN ('queued', 'running')",
            params![now],
        )?;
        let expired = expire(&db, &logs, limits, now)?;
        let logs_trimmed = trim_logs(&logs, limits)?;
        let last: Option<i64> = db
            .query_row("SELECT MAX(id) FROM jobs", [], |row| row.get(0))
            .optional()?
            .flatten();
        let next_id = last
            .and_then(|id| JobId::try_from(id).ok())
            .map_or(1, |id| id + 1);

        let (writes, queue) = mpsc::channel();
        let last_error = Arc::new(Mutex::new(None));
        let writer = Writer {
            db,
            logs: logs.clone(),
            open_logs: HashMap::new(),
            last_error: Arc::clone(&last_error),
        };
        std::thread::Builder::new()
            .name("assist-store".to_owned())
            .spawn(move || writer.run(&queue))?;
        let store = Self {
            writes,
            db_path,
            logs,
            next_id,
            last_error,
        };
        let report = OpenReport {
            interrupted,
            expired,
            logs_trimmed,
        };
        Ok((store, report))
    }

    /// The first id after every job in the store.
    pub fn next_id(&self) -> JobId {
        self.next_id
    }

    pub(crate) fn insert(&self, job: &JobInfo) {
        let _ = self.writes.send(Op::Insert(Row::from(job)));
    }

    pub(crate) fn update(&self, job: &JobInfo) {
        let _ = self.writes.send(Op::Update(Row::from(job)));
    }

    pub(crate) fn output(&self, id: JobId, line: &str) {
        let _ = self.writes.send(Op::Output(id, line.to_owned()));
    }

    /// Blocks until every write sent so far is on disk.
    pub fn flush(&self) {
        let (done, wait) = mpsc::channel();
        if self.writes.send(Op::Flush(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    /// Writes everything sent so far, closes the database and logs, and stops the writer.
    /// Later writes from any clone are dropped. Blocking.
    pub fn close(&self) {
        let (done, wait) = mpsc::channel();
        if self.writes.send(Op::Close(done)).is_ok() {
            let _ = wait.recv();
        }
    }

    /// The last write error, if any write failed.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Up to `limit` jobs, newest first, older than `before` if given. Blocking.
    pub fn history(
        &self,
        limit: usize,
        before: Option<JobId>,
    ) -> Result<Vec<JobSummary>, StoreError> {
        let db = Connection::open_with_flags(&self.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut query = db.prepare(
            "SELECT id, command, title, source, lane, state, created, started, finished, error
             FROM jobs WHERE id < ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let before = before.map_or(i64::MAX, to_db);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = query.query_map(params![before, limit], |row| {
            Ok(JobSummary {
                id: from_db(row.get(0)?),
                command: row.get(1)?,
                title: row.get(2)?,
                source: row.get(3)?,
                lane: lane_from(&row.get::<_, String>(4)?),
                state: state_from(&row.get::<_, String>(5)?),
                created: from_db(row.get(6)?),
                started: row.get::<_, Option<i64>>(7)?.map(from_db),
                finished: row.get::<_, Option<i64>>(8)?.map(from_db),
                error: row.get(9)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The last lines of a job's log, at most `max_bytes`. Empty if there is no log. Blocking.
    pub fn log_tail(&self, id: JobId, max_bytes: u64) -> io::Result<Vec<String>> {
        let mut file = match File::open(log_path(&self.logs, id)) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let len = file.metadata()?.len();
        let start = len.saturating_sub(max_bytes);
        file.seek(SeekFrom::Start(start))?;
        let mut text = Vec::new();
        file.read_to_end(&mut text)?;
        let text = String::from_utf8_lossy(&text);
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        if start > 0 && !lines.is_empty() {
            // The first line is cut in the middle.
            lines.remove(0);
        }
        Ok(lines)
    }
}

/// One `jobs` row.
struct Row {
    id: i64,
    command: String,
    title: String,
    source: String,
    user: String,
    trust: &'static str,
    lane: &'static str,
    state: &'static str,
    created: i64,
    started: Option<i64>,
    finished: Option<i64>,
    error: Option<String>,
    exit: Option<i32>,
}

impl From<&JobInfo> for Row {
    fn from(job: &JobInfo) -> Self {
        Self {
            id: to_db(job.id),
            command: job.command.clone(),
            title: job.title.clone(),
            source: job.principal.source.clone(),
            user: job.principal.user.clone(),
            trust: job.principal.trust.as_str(),
            lane: job.lane.as_str(),
            state: job.state.as_str(),
            created: to_db(job.created),
            started: job.started.map(to_db),
            finished: job.finished.map(to_db),
            error: job.error.as_ref().map(|e| e.message.clone()),
            exit: job.error.as_ref().and_then(|e| e.exit),
        }
    }
}

struct Writer {
    db: Connection,
    logs: PathBuf,
    open_logs: HashMap<JobId, BufWriter<File>>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl Writer {
    fn run(mut self, queue: &mpsc::Receiver<Op>) {
        while let Ok(op) = queue.recv() {
            if let Op::Close(done) = op {
                for log in self.open_logs.values_mut() {
                    let _ = log.flush();
                }
                drop(self);
                let _ = done.send(());
                return;
            }
            if let Err(err) = self.apply(op) {
                *self
                    .last_error
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Some(err.to_string());
            }
        }
    }

    fn apply(&mut self, op: Op) -> Result<(), StoreError> {
        match op {
            Op::Insert(row) => {
                self.db.execute(
                    "INSERT OR REPLACE INTO jobs (id, command, title, source, user, trust, lane,
                     state, created, started, finished, error, exit)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        row.id,
                        row.command,
                        row.title,
                        row.source,
                        row.user,
                        row.trust,
                        row.lane,
                        row.state,
                        row.created,
                        row.started,
                        row.finished,
                        row.error,
                        row.exit
                    ],
                )?;
            }
            Op::Update(row) => {
                self.db.execute(
                    "UPDATE jobs SET state = ?2, started = ?3, finished = ?4, error = ?5,
                     exit = ?6 WHERE id = ?1",
                    params![
                        row.id,
                        row.state,
                        row.started,
                        row.finished,
                        row.error,
                        row.exit
                    ],
                )?;
                if row.finished.is_some() {
                    if let Some(mut log) = self.open_logs.remove(&from_db(row.id)) {
                        log.flush()?;
                    }
                }
            }
            Op::Output(id, line) => {
                let log = match self.open_logs.entry(id) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        let file = private_fs::open_append(&log_path(&self.logs, id))?;
                        entry.insert(BufWriter::new(file))
                    }
                };
                writeln!(log, "{line}")?;
            }
            Op::Flush(done) => {
                for log in self.open_logs.values_mut() {
                    log.flush()?;
                }
                let _ = done.send(());
            }
            Op::Close(_) => {}
        }
        Ok(())
    }
}

fn log_path(logs: &Path, id: JobId) -> PathBuf {
    logs.join(format!("{id}.log"))
}

/// Deletes jobs (and their logs) that finished before the retention period.
fn expire(
    db: &Connection,
    logs: &Path,
    limits: &JobsConfig,
    now: i64,
) -> Result<usize, StoreError> {
    let days = i64::try_from(limits.retention_days).unwrap_or(i64::MAX);
    let cutoff = now.saturating_sub(days.saturating_mul(86_400_000));
    let ids: Vec<i64> = db
        .prepare("SELECT id FROM jobs WHERE finished < ?1")?
        .query_map(params![cutoff], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    for id in &ids {
        remove_if_present(&log_path(logs, from_db(*id)))?;
    }
    db.execute("DELETE FROM jobs WHERE finished < ?1", params![cutoff])?;
    Ok(ids.len())
}

/// Deletes the oldest logs while all logs together exceed the size cap.
fn trim_logs(logs: &Path, limits: &JobsConfig) -> Result<usize, StoreError> {
    let cap = limits.log_cap_mb.saturating_mul(1024 * 1024);
    let mut files: Vec<(JobId, PathBuf, u64)> = Vec::new();
    for entry in fs::read_dir(logs)? {
        let entry = entry?;
        let path = entry.path();
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .and_then(|stem| stem.parse::<JobId>().ok());
        if let Some(id) = id {
            files.push((id, path, entry.metadata()?.len()));
        }
    }
    files.sort_by_key(|(id, _, _)| *id);
    let mut total: u64 = files.iter().map(|(_, _, size)| size).sum();
    let mut removed = 0;
    for (_, path, size) in files {
        if total <= cap {
            break;
        }
        remove_if_present(&path)?;
        total -= size;
        removed += 1;
    }
    Ok(removed)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

fn to_db(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn from_db(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn lane_from(text: &str) -> Lane {
    if text == Lane::Background.as_str() {
        Lane::Background
    } else {
        Lane::Interactive
    }
}

fn state_from(text: &str) -> JobState {
    [
        JobState::Queued,
        JobState::Running,
        JobState::Done,
        JobState::Failed,
        JobState::Cancelled,
        JobState::TimedOut,
        JobState::Interrupted,
    ]
    .into_iter()
    .find(|state| state.as_str() == text)
    .unwrap_or(JobState::Interrupted)
}

/// Reads a whole log, for tests.
#[cfg(test)]
fn read_log(store: &Store, id: JobId) -> Vec<String> {
    use std::io::BufRead;
    let file = File::open(log_path(&store.logs, id)).unwrap();
    io::BufReader::new(file)
        .lines()
        .map(Result::unwrap)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionError;
    use crate::policy::Principal;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("assist-store-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn job(id: JobId, state: JobState) -> JobInfo {
        let now = now_ms();
        JobInfo {
            id,
            command: "shell".to_owned(),
            title: format!("$ job {id}"),
            principal: Principal::local("test"),
            lane: Lane::Interactive,
            state,
            created: now,
            started: Some(now),
            finished: state.is_finished().then_some(now),
            error: None,
        }
    }

    #[test]
    fn history_logs_and_restart() {
        let dir = temp_dir("restart");
        let limits = JobsConfig::default();
        let (store, report) = Store::open(&dir, &limits).unwrap();
        assert_eq!((store.next_id(), report), (1, OpenReport::default()));

        let mut done = job(1, JobState::Running);
        store.insert(&done);
        store.output(1, "hello");
        store.output(1, "! oops");
        done.state = JobState::Failed;
        done.finished = Some(now_ms());
        done.error = Some(ActionError::exit(2));
        store.update(&done);
        store.insert(&job(2, JobState::Running));
        store.flush();

        assert_eq!(read_log(&store, 1), ["hello", "! oops"]);
        assert_eq!(store.log_tail(1, 8).unwrap(), ["! oops"]);
        assert!(store.log_tail(9, 100).unwrap().is_empty());
        let history = store.history(10, None).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].state, JobState::Failed);
        assert_eq!(history[1].error.as_deref(), Some("exited with code 2"));
        assert_eq!(store.history(10, Some(2)).unwrap().len(), 1);
        store.close();

        let (store, report) = Store::open(&dir, &limits).unwrap();
        assert_eq!(report.interrupted, 1);
        assert_eq!(store.next_id(), 3);
        assert_eq!(
            store.history(1, None).unwrap()[0].state,
            JobState::Interrupted
        );
        assert!(store.last_error().is_none());
        store.close();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn retention_and_log_cap() {
        let dir = temp_dir("retention");
        let (store, _) = Store::open(&dir, &JobsConfig::default()).unwrap();
        let mut old = job(1, JobState::Done);
        old.finished = Some(1);
        store.insert(&old);
        store.output(1, "old");
        for id in 2..=3 {
            store.insert(&job(id, JobState::Done));
            store.output(id, &"x".repeat(1024));
        }
        store.flush();
        store.close();

        let limits = JobsConfig {
            log_cap_mb: 0,
            ..JobsConfig::default()
        };
        let (store, report) = Store::open(&dir, &limits).unwrap();
        assert_eq!(report.expired, 1);
        assert_eq!(report.logs_trimmed, 2);
        assert_eq!(store.history(10, None).unwrap().len(), 2);
        store.close();
        fs::remove_dir_all(&dir).unwrap();
    }
}
