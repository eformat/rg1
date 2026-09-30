//! On-disk answer cache (SQLite): reruns and repeated lines cost zero API calls.

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Cache {
    conn: Mutex<Connection>,
}

impl Cache {
    /// Open (creating if needed) the default cache: $XDG_CACHE_HOME/rg1 or
    /// $HOME/.cache/rg1. Returns None when the cache cannot be used (it is
    /// always safe to run without it).
    pub fn open_default() -> Option<Self> {
        let dir = cache_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        Self::open(&dir.join("answers.v1.sqlite"))
    }

    pub fn open(path: &Path) -> Option<Self> {
        let conn = Connection::open(path).ok()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS answers (
                 key     TEXT PRIMARY KEY,
                 p       REAL NOT NULL,
                 created INTEGER NOT NULL
             );",
        )
        .ok()?;
        Some(Cache {
            conn: Mutex::new(conn),
        })
    }

    pub fn get(&self, key: &str) -> Option<f64> {
        let conn = self.conn.lock().ok()?;
        conn.query_row("SELECT p FROM answers WHERE key = ?1", [key], |r| r.get(0))
            .ok()
    }

    pub fn put(&self, key: &str, p: f64) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO answers (key, p, created) VALUES (?1, ?2, ?3)",
                rusqlite::params![key, p, now_secs()],
            );
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn cache_dir() -> Option<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Some(PathBuf::from(x).join("rg1"));
        }
    }
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".cache").join("rg1"))
}

/// Cache key: endpoint host | model | sha256(state + US + instructions).
pub fn key(state: &str, instructions: &str, endpoint: &str, model: &str) -> String {
    let mut h = Sha256::new();
    h.update(state.as_bytes());
    h.update([0x1f]);
    h.update(instructions.as_bytes());
    let digest = h.finalize();
    let hex: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    format!("{}|{model}|{hex}", host_of(endpoint))
}

fn host_of(endpoint: &str) -> String {
    let rest = endpoint
        .trim()
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    rest.split('/').next().unwrap_or(rest).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("rg1-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cache = Cache::open(&dir.join("t.sqlite")).unwrap();
        assert_eq!(cache.get("k"), None);
        cache.put("k", 0.42);
        assert_eq!(cache.get("k"), Some(0.42));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_differ_by_state_and_instructions() {
        let a = key("state1", "ins", "https://x.example", "auto");
        let b = key("state2", "ins", "https://x.example", "auto");
        let c = key("state1", "ins2", "https://x.example", "auto");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
