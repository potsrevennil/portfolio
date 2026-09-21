use anyhow::Result;
use portfolio::db;
use sqlx::SqlitePool;
use tempfile::NamedTempFile;

pub async fn create_temp_db() -> Result<(SqlitePool, NamedTempFile)> {
    let db_file = NamedTempFile::new_in("tests")?;
    let db_url = format!("sqlite:{}", db_file.path().to_str().unwrap());

    let pool = db::init_db(&db_url).await?;

    Ok((pool, db_file))
}
