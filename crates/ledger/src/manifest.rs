//! `raw/manifest.csv`: every original file kept under `raw/`, one row each.
//! Read for the files a source already has; an optional `downloaded` column
//! (ISO date) is the day a file was fetched from the institution.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RawFile {
    /// As listed: `raw/<party>/<…>`.
    pub raw_path: String,
    #[serde(default, deserialize_with = "blank_is_none")]
    pub downloaded: Option<NaiveDate>,
}

fn blank_is_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<NaiveDate>, D::Error> {
    let text = String::deserialize(d)?;
    match text.trim() {
        "" => Ok(None),
        date => date.parse().map(Some).map_err(serde::de::Error::custom),
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Manifest(pub Vec<RawFile>);

impl Manifest {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut reader =
            csv::Reader::from_path(path).with_context(|| format!("reading {}", path.display()))?;
        let files = reader
            .deserialize()
            .collect::<Result<Vec<RawFile>, _>>()
            .with_context(|| format!("parsing {}", path.display()))?;
        Ok(Manifest(files))
    }

    /// The files under `raw/<folder>/`, by path.
    pub fn under<'a>(&'a self, folder: &str) -> Vec<&'a RawFile> {
        let prefix = format!("raw/{}/", folder.trim_matches('/'));
        let mut files: Vec<_> = self.0.iter().filter(|f| f.raw_path.starts_with(&prefix)).collect();
        files.sort_by(|a, b| a.raw_path.cmp(&b.raw_path));
        files
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn load(lines: &[&str]) -> Manifest {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(lines.join("\n").as_bytes()).unwrap();
        Manifest::load(file.path()).unwrap()
    }

    #[test]
    fn lists_a_folders_files_in_path_order() {
        let manifest = load(&[
            "raw_path,original,sha256,bytes",
            "raw/card/2026-02.csv,a,00,1",
            "raw/card-two/2026-09.csv,b,00,1",
            "raw/card/2026-01.csv,c,00,1",
        ]);
        let paths: Vec<_> = manifest.under("card/").iter().map(|f| f.raw_path.as_str()).collect();
        assert_eq!(paths, ["raw/card/2026-01.csv", "raw/card/2026-02.csv"]);
        assert_eq!(manifest.0[0].downloaded, None);
    }

    #[test]
    fn reads_the_download_date_where_there_is_one() {
        let manifest = load(&[
            "raw_path,original,sha256,bytes,downloaded",
            "raw/card/2026-01.csv,a,00,1,2026-02-03",
            "raw/card/2026-02.csv,a,00,1,",
        ]);
        let dates: Vec<_> = manifest.0.iter().map(|f| f.downloaded).collect();
        assert_eq!(dates, [Some("2026-02-03".parse().unwrap()), None]);
    }
}
