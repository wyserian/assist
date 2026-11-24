//! Files and directories only the current user can read.
//!
//! Unix: directories `0700`, files `0600`. Windows: the default data directory is inside the
//! user profile, whose ACL already limits access to the user; a custom `--data-dir` keeps the
//! ACL of its parent.

use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Creates `path` and missing parents, owner-only.
pub fn create_dir(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Opens `path` for appending, creating it owner-only.
pub fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Replaces `path` with `bytes` owner-only and atomically: a temporary file, then a rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = Path::new(&tmp);
    let _ = std::fs::remove_file(tmp);
    let mut file = open_append(tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(tmp, path)
}
