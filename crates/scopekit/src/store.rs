//! An app's SQLite database: key/value pairs, any SQL, the command history,
//! a session log and help texts, in one file that several processes can
//! share (WAL, a busy timeout).
//!
//! ```
//! use scopekit::store::{Store, Value};
//!
//! let store = Store::in_memory().unwrap();
//! store.put("greeting", "hello").unwrap();
//! assert_eq!(store.get("greeting").unwrap().as_deref(), Some("hello"));
//! store.exec("CREATE TABLE parts (name TEXT, qty INTEGER)", &[]).unwrap();
//! store.exec("INSERT INTO parts VALUES (?1, ?2)", &[Value::from("R1"), Value::from(4)]).unwrap();
//! let rows = store.query("SELECT name, qty FROM parts", &[]).unwrap();
//! assert_eq!(rows, vec![vec![Value::from("R1"), Value::from(4)]]);
//! ```

use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{params, Connection, OptionalExtension, ToSql};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The tables of every store; an app's own tables go next to them
const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS kv (
        key TEXT PRIMARY KEY, value TEXT NOT NULL, updated REAL NOT NULL);
    CREATE TABLE IF NOT EXISTS history (
        id INTEGER PRIMARY KEY, source TEXT NOT NULL, text TEXT NOT NULL, at REAL NOT NULL);
    CREATE TABLE IF NOT EXISTS log (
        id INTEGER PRIMARY KEY, source TEXT NOT NULL, input TEXT NOT NULL,
        output TEXT NOT NULL, seconds REAL NOT NULL, at REAL NOT NULL);
    CREATE TABLE IF NOT EXISTS help (
        name TEXT PRIMARY KEY, text TEXT NOT NULL, updated REAL NOT NULL);
";

/// A value in or out of SQL
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL NULL
    Null,
    /// An integer
    Integer(i64),
    /// A floating point number
    Real(f64),
    /// Text
    Text(String),
    /// Bytes
    Blob(Vec<u8>),
}

impl From<&str> for Value {
    fn from(text: &str) -> Value {
        Value::Text(text.to_owned())
    }
}

impl From<String> for Value {
    fn from(text: String) -> Value {
        Value::Text(text)
    }
}

impl From<i64> for Value {
    fn from(n: i64) -> Value {
        Value::Integer(n)
    }
}

impl From<i32> for Value {
    fn from(n: i32) -> Value {
        Value::Integer(n.into())
    }
}

impl From<f64> for Value {
    fn from(x: f64) -> Value {
        Value::Real(x)
    }
}

impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Borrowed(match self {
            Value::Null => ValueRef::Null,
            Value::Integer(n) => ValueRef::Integer(*n),
            Value::Real(x) => ValueRef::Real(*x),
            Value::Text(s) => ValueRef::Text(s.as_bytes()),
            Value::Blob(b) => ValueRef::Blob(b),
        }))
    }
}

impl From<ValueRef<'_>> for Value {
    fn from(value: ValueRef<'_>) -> Value {
        match value {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(n) => Value::Integer(n),
            ValueRef::Real(x) => Value::Real(x),
            ValueRef::Text(t) => Value::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => Value::Blob(b.to_vec()),
        }
    }
}

/// One entry of the session log
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    /// Who evaluated it (e.g. "quadra-lisp", "acadlisp")
    pub source: String,
    /// What was evaluated
    pub input: String,
    /// What came out
    pub output: String,
    /// How long it took
    pub seconds: f64,
    /// When, seconds since 1970
    pub at: f64,
}

/// An app's SQLite database
#[derive(Debug)]
pub struct Store {
    conn: Connection,
    path: Option<PathBuf>,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn err(e: rusqlite::Error) -> String {
    e.to_string()
}

impl Store {
    /// Where an app's database lives: `~/Library/Application Support/<app>/
    /// <app>.sqlite` on macOS, `$XDG_DATA_HOME/<app>/` (or `~/.local/share`)
    /// elsewhere, `%APPDATA%\<app>\` on Windows
    pub fn default_path(app: &str) -> PathBuf {
        let home = || PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let base = if cfg!(target_os = "macos") {
            home().join("Library/Application Support")
        } else if cfg!(windows) {
            std::env::var_os("APPDATA").map_or_else(home, PathBuf::from)
        } else {
            std::env::var_os("XDG_DATA_HOME")
                .map_or_else(|| home().join(".local/share"), PathBuf::from)
        };
        base.join(app).join(format!("{app}.sqlite"))
    }

    /// Opens (creates) the app's database at its [default path](Self::default_path)
    pub fn open_app(app: &str) -> Result<Store, String> {
        Store::open(Store::default_path(app))
    }

    /// Opens (creates) a database file, and its folder
    pub fn open(path: impl AsRef<Path>) -> Result<Store, String> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let conn = Connection::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        // Several processes (an app and its REPL client) share the file
        conn.busy_timeout(Duration::from_secs(3)).map_err(err)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(err)?;
        Store::init(conn, Some(path.to_owned()))
    }

    /// A database in memory (tests, or nothing to keep)
    pub fn in_memory() -> Result<Store, String> {
        Store::init(Connection::open_in_memory().map_err(err)?, None)
    }

    fn init(conn: Connection, path: Option<PathBuf>) -> Result<Store, String> {
        conn.execute_batch(SCHEMA).map_err(err)?;
        Ok(Store { conn, path })
    }

    /// The file, or `None` in memory
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The SQLite connection, for anything else
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    // Key/value pairs

    /// Sets a key's value
    pub fn put(&self, key: &str, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO kv (key, value, updated) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = ?2, updated = ?3",
                params![key, value, now()],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// A key's value
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(err)
    }

    /// Removes a key; whether it was there
    pub fn delete(&self, key: &str) -> Result<bool, String> {
        self.conn
            .execute("DELETE FROM kv WHERE key = ?1", [key])
            .map(|n| n > 0)
            .map_err(err)
    }

    /// The keys starting with `prefix`, sorted
    pub fn keys(&self, prefix: &str) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT key FROM kv WHERE substr(key, 1, length(?1)) = ?1 ORDER BY key")
            .map_err(err)?;
        let rows = stmt.query_map([prefix], |row| row.get(0)).map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    // Any SQL

    /// Runs a statement with `?1 ?2 ...` parameters; the rows changed.
    /// Without parameters, several statements separated by `;` run.
    pub fn exec(&self, sql: &str, params: &[Value]) -> Result<usize, String> {
        if params.is_empty() {
            let before = self.conn.total_changes();
            self.conn.execute_batch(sql).map_err(err)?;
            return Ok((self.conn.total_changes() - before) as usize);
        }
        self.conn
            .execute(sql, rusqlite::params_from_iter(params))
            .map_err(err)
    }

    /// Runs a query; its rows
    pub fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Vec<Value>>, String> {
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        let columns = stmt.column_count();
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params))
            .map_err(err)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next().map_err(err)? {
            let mut values = Vec::with_capacity(columns);
            for i in 0..columns {
                values.push(Value::from(row.get_ref(i).map_err(err)?));
            }
            out.push(values);
        }
        Ok(out)
    }

    // The command history

    /// Adds a command to the history, unless it repeats the last one
    pub fn history_add(&self, source: &str, text: &str) -> Result<(), String> {
        if text.trim().is_empty() {
            return Ok(());
        }
        let last: Option<String> = self
            .conn
            .query_row(
                "SELECT text FROM history WHERE source = ?1 ORDER BY id DESC LIMIT 1",
                [source],
                |row| row.get(0),
            )
            .optional()
            .map_err(err)?;
        if last.as_deref() == Some(text) {
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO history (source, text, at) VALUES (?1, ?2, ?3)",
                params![source, text, now()],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// The last `limit` commands of a source, oldest first
    pub fn history(&self, source: &str, limit: usize) -> Result<Vec<String>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT text FROM (SELECT id, text FROM history WHERE source = ?1
                 ORDER BY id DESC LIMIT ?2) ORDER BY id",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map(params![source, limit as i64], |row| row.get(0))
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    // The session log

    /// Logs an evaluation: what went in, what came out, how long it took
    pub fn log(&self, source: &str, input: &str, output: &str, seconds: f64) -> Result<(), String> {
        self.conn
            .execute(
                "INSERT INTO log (source, input, output, seconds, at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![source, input, output, seconds, now()],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// The last `limit` log entries, oldest first
    pub fn log_entries(&self, limit: usize) -> Result<Vec<LogEntry>, String> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source, input, output, seconds, at FROM (SELECT * FROM log
                 ORDER BY id DESC LIMIT ?1) ORDER BY id",
            )
            .map_err(err)?;
        let rows = stmt
            .query_map([limit as i64], |row| {
                Ok(LogEntry {
                    source: row.get(0)?,
                    input: row.get(1)?,
                    output: row.get(2)?,
                    seconds: row.get(3)?,
                    at: row.get(4)?,
                })
            })
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }

    // Help texts

    /// Sets a name's help text (an empty text removes it)
    pub fn help_set(&self, name: &str, text: &str) -> Result<(), String> {
        if text.is_empty() {
            return self
                .conn
                .execute("DELETE FROM help WHERE name = ?1", [name])
                .map(|_| ())
                .map_err(err);
        }
        self.conn
            .execute(
                "INSERT INTO help (name, text, updated) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET text = ?2, updated = ?3",
                params![name, text, now()],
            )
            .map(|_| ())
            .map_err(err)
    }

    /// A name's help text
    pub fn help_get(&self, name: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row("SELECT text FROM help WHERE name = ?1", [name], |row| {
                row.get(0)
            })
            .optional()
            .map_err(err)
    }

    /// Every help text: (name, text), sorted by name
    pub fn help_all(&self) -> Result<Vec<(String, String)>, String> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, text FROM help ORDER BY name")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_values_and_sql() {
        let s = Store::in_memory().unwrap();
        s.put("a/1", "x").unwrap();
        s.put("a/2", "y").unwrap();
        s.put("a/1", "z").unwrap();
        s.put("b", "w").unwrap();
        assert_eq!(s.get("a/1").unwrap().as_deref(), Some("z"));
        assert_eq!(s.keys("a/").unwrap(), ["a/1", "a/2"]);
        assert!(s.delete("b").unwrap());
        assert!(!s.delete("b").unwrap());
        assert_eq!(s.get("b").unwrap(), None);
        assert_eq!(
            s.exec(
                "CREATE TABLE t (n INTEGER, x REAL); INSERT INTO t VALUES (1, 0.5)",
                &[]
            )
            .unwrap(),
            1
        );
        s.exec("INSERT INTO t VALUES (?1, ?2)", &[2.into(), Value::Null])
            .unwrap();
        let rows = s
            .query("SELECT n, x FROM t WHERE n >= ?1 ORDER BY n", &[1.into()])
            .unwrap();
        assert_eq!(
            rows,
            vec![
                vec![Value::Integer(1), Value::Real(0.5)],
                vec![Value::Integer(2), Value::Null]
            ]
        );
        assert!(s.query("SELECT nope FROM t", &[]).is_err());
    }

    #[test]
    fn history_log_and_help() {
        let s = Store::in_memory().unwrap();
        for text in ["(a)", "(b)", "(b)", "  ", "(c)"] {
            s.history_add("repl", text).unwrap();
        }
        s.history_add("other", "(z)").unwrap();
        assert_eq!(s.history("repl", 10).unwrap(), ["(a)", "(b)", "(c)"]);
        assert_eq!(s.history("repl", 2).unwrap(), ["(b)", "(c)"]);
        s.log("repl", "(+ 1 2)", "3", 0.001).unwrap();
        let log = s.log_entries(5).unwrap();
        assert_eq!(
            (log[0].input.as_str(), log[0].output.as_str()),
            ("(+ 1 2)", "3")
        );
        s.help_set("MAC-CLICK", "clicks").unwrap();
        assert_eq!(s.help_get("MAC-CLICK").unwrap().as_deref(), Some("clicks"));
        assert_eq!(s.help_all().unwrap().len(), 1);
        s.help_set("MAC-CLICK", "").unwrap();
        assert_eq!(s.help_get("MAC-CLICK").unwrap(), None);
    }

    #[test]
    fn a_file_shared_by_two_connections() {
        let dir = std::env::temp_dir().join(format!("scopekit-store-{}", std::process::id()));
        let path = dir.join("app.sqlite");
        let one = Store::open(&path).unwrap();
        let two = Store::open(&path).unwrap();
        one.history_add("repl", "(from one)").unwrap();
        assert_eq!(two.history("repl", 5).unwrap(), ["(from one)"]);
        drop((one, two));
        let _ = std::fs::remove_dir_all(dir);
    }
}
