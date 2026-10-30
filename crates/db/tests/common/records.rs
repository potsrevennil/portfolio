//! Where the owner's own records live: gitignored, in the main checkout or
//! `$LEDGER_RECORDS_DIR`. Absent on CI, where tests over them skip.

use std::path::PathBuf;

pub fn records_dir() -> Option<PathBuf> {
    let dir = match std::env::var_os("LEDGER_RECORDS_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => {
            // A worktree's records are its main checkout's.
            let out = std::process::Command::new("git")
                .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .ok()?;
            PathBuf::from(String::from_utf8(out.stdout).ok()?.trim()).parent()?.to_path_buf()
        }
    };
    dir.join("ledger/journal.csv").is_file().then_some(dir)
}
