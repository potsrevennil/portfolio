//! A statement PDF's text.

use std::{path::Path, process::Command};

use anyhow::{ensure, Context, Result};
use rust_decimal::Decimal;

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

/// `1,234` or `-30`.
pub fn money(s: &str) -> Option<Decimal> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let plain = digits.chars().all(|c| c.is_ascii_digit() || c == ',')
        && digits.starts_with(|c: char| c.is_ascii_digit());
    plain.then(|| s.replace(',', "").parse().ok()).flatten()
}
