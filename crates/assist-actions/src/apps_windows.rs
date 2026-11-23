//! Windows: the shell's "open", which also finds apps registered under App Paths
//! (e.g. `firefox.exe` that is not on PATH).

pub(super) fn launch(program: &str) -> std::io::Result<()> {
    open::that_detached(program)
}
