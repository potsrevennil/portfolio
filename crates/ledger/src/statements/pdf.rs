//! A statement PDF's text.

use std::{path::Path, process::Command};

use anyhow::{ensure, Context, Result};

/// The PDF's text, as `pdftotext -raw` lays it out.
pub fn text(path: &Path) -> Result<String> {
    let out = Command::new("pdftotext")
        .args(["-raw", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .output()
        .context("running pdftotext (poppler); it ships in the nix devShell")?;
    ensure!(
        out.status.success(),
        "pdftotext {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    String::from_utf8(out.stdout).context("pdftotext output is not UTF-8")
}
