//! Statements given as the text `pdftotext -raw` prints: a stand-in `pdftotext`
//! on PATH serves `<name>.txt` for `<name>.pdf`, so the real subprocess path
//! runs without a PDF in the repo.

use std::{path::PathBuf, sync::Once};

static FAKE_PDFTOTEXT: Once = Once::new();

/// Puts a `pdftotext` first on PATH that prints the `.txt` beside the `.pdf`.
pub fn fake_pdftotext() {
    FAKE_PDFTOTEXT.call_once(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fake-pdftotext");
        std::fs::create_dir_all(&dir).expect("fake pdftotext dir");
        let script = dir.join("pdftotext");
        // Called as `pdftotext -raw -enc UTF-8 <file> -`.
        std::fs::write(&script, "#!/bin/sh\nexec cat \"${4%.pdf}.txt\"\n").expect("script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("executable");
        }
        let path = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(dir).chain(std::env::split_paths(&path));
        std::env::set_var("PATH", std::env::join_paths(paths).expect("PATH"));
    });
}
