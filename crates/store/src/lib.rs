//! Persistent state on one SQLite file, `$XDG_DATA_HOME/blacksheep/blacksheep.db`.
//!
//! One connection behind a mutex; callers that run on an async runtime go
//! through [`Store::run`], which moves the work to a blocking thread.

use std::sync::{Arc, Mutex, OnceLock};

use camino::Utf8PathBuf;
use rusqlite::Connection;

pub mod kv;
pub mod launches;
pub mod notifications;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no data directory for this user")]
    NoDataDir,
    #[error("data directory is not UTF-8")]
    NotUtf8,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("store worker panicked")]
    Join,
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
}

static GLOBAL: OnceLock<Store> = OnceLock::new();

/// The process-wide store, once [`init`] has run.
pub fn global() -> Option<&'static Store> {
    GLOBAL.get()
}

/// Open the default database and make it the process-wide store.
pub fn init() -> Result<&'static Store> {
    if let Some(store) = GLOBAL.get() {
        return Ok(store);
    }
    let store = Store::open_default()?;
    let _ = GLOBAL.set(store);
    Ok(GLOBAL.get().expect("just set"))
}

pub fn default_path() -> Result<Utf8PathBuf> {
    let dir = dirs::data_dir().ok_or(Error::NoDataDir)?;
    let dir = Utf8PathBuf::from_path_buf(dir).map_err(|_| Error::NotUtf8)?;
    Ok(dir.join("blacksheep").join("blacksheep.db"))
}

impl Store {
    pub fn open_default() -> Result<Self> {
        let path = default_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        tracing::info!("store: {path}");
        Self::open(Connection::open(path)?)
    }

    /// A private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        Self::open(Connection::open_in_memory()?)
    }

    fn open(connection: Connection) -> Result<Self> {
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        migrate(&connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Run a query on the calling thread. Fine for one-row reads from the
    /// UI thread; anything heavier should use [`Store::run`].
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        Ok(f(&connection)?)
    }

    /// Run a query on a blocking thread without stalling the async runtime.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.with(f))
            .await
            .map_err(|_| Error::Join)?
    }
}

/// Schema versions, applied in order; `user_version` records the last one.
const MIGRATIONS: &[&str] = &[
    // 1: launch history and key-value state
    "CREATE TABLE launches (
        app_id TEXT NOT NULL,
        at     INTEGER NOT NULL
     );
     CREATE INDEX launches_app ON launches(app_id, at);
     CREATE TABLE kv (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
     );",
    // 2: notification history
    "CREATE TABLE notifications (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        notified_at   INTEGER NOT NULL,
        app_name      TEXT NOT NULL,
        app_icon      TEXT NOT NULL,
        summary       TEXT NOT NULL,
        body          TEXT NOT NULL,
        desktop_entry TEXT NOT NULL,
        urgency       INTEGER NOT NULL,
        read          INTEGER NOT NULL DEFAULT 0
     );
     CREATE INDEX notifications_time ON notifications(notified_at DESC);",
    // 3: stacked duplicates
    "ALTER TABLE notifications ADD COLUMN count INTEGER NOT NULL DEFAULT 1;",
];

fn migrate(connection: &Connection) -> Result<()> {
    let current: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = index as u32 + 1;
        connection.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {version}; COMMIT;"))?;
        tracing::debug!("store: migrated to schema {version}");
    }
    Ok(())
}
