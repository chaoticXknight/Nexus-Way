// Owns SQLite connection options, WAL mode, permissions, and migration execution.
// Migration SQL owns schema history; domain modules own all application queries.

// Metadata store (Hive_design_doc.md §3.1): SQLite in WAL mode, real
// migrations from day one so SQLite → Postgres is a config change + data
// copy, not a rewrite.

use anyhow::Result;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::path::Path;

pub async fn open(data_dir: &Path) -> Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(data_dir.join("hive.db"))
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::migrate!("./migrations").run(&pool).await?;
    Ok(pool)
}
