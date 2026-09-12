//! Read per-call usage out of Devin's `sessions.db` (SQLite via rusqlite).
//!
//! Transcripts only serialize the *current* chain of a session; resumed,
//! compacted or forked chains — and subagent sessions, which never get a
//! transcript — live on in the `message_nodes` table. Every message node
//! carries `metadata.num_tokens_preceding`, which equals the exact
//! `prompt_tokens` of the inference call that produced it (verified against
//! `response_dimensions` and transcript `metrics` — they agree to the token).
//!
//! The same logical message is stored twice per node (with/without metadata),
//! so calls are deduped by `message_id`, keeping the max `num_tokens_preceding`.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One inference call recovered from the sessions.db message tree.
#[derive(Debug)]
pub struct DbCall {
    pub session: String,
    /// Node creation time, unix seconds.
    pub ts: i64,
    /// Exact prompt tokens for this call; 0 when the node recorded none.
    pub prompt: u64,
}

#[derive(Debug, Default)]
pub struct DbData {
    /// session id -> model recorded on the session row (may be empty).
    pub session_models: HashMap<String, String>,
    /// Inference calls deduped by message_id.
    pub calls: Vec<DbCall>,
}

pub fn default_db_path() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".local/share/devin/cli/sessions.db")
}

/// Sequentially read the db + wal into the OS page cache on a background
/// thread. SQLite then fetches its scattered 4KB pages from RAM instead of
/// doing ~300k random reads against a live, WAL-mode multi-GB file.
/// Sequential readahead of the whole file is far faster than the pages we
/// actually need fetched at random offsets.
fn prefetch(path: &Path) {
    for suffix in ["", "-wal"] {
        let p = PathBuf::from(format!("{}{suffix}", path.display()));
        std::thread::spawn(move || {
            use std::io::Read;
            let mut f = match std::fs::File::open(&p) {
                Ok(f) => f,
                Err(_) => return,
            };
            let mut buf = vec![0u8; 8 << 20];
            while matches!(f.read(&mut buf), Ok(n) if n > 0) {}
        });
    }
}

pub fn load(path: &Path) -> Result<DbData> {
    prefetch(path);
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open {}", path.display()))?;
    // Memory-map the multi-GB file: turns the scan into page-cache hits
    // instead of read() syscalls.
    conn.pragma_update(None, "mmap_size", 8_000_000_000i64)?;
    // mmap is capped at 2GB < file size — the tail is read through the page
    // cache, so give it room. Temp b-trees (GROUP BY) stay in RAM.
    conn.pragma_update(None, "cache_size", -2_000_000i64)?;
    conn.pragma_update(None, "temp_store", 2i64)?;

    let mut session_models = HashMap::new();
    {
        let mut st = conn.prepare("SELECT id, COALESCE(model,'') FROM sessions")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (id, m) = r?;
            session_models.insert(id, m);
        }
    }

    // All extraction and per-message_id dedup happens in SQL.
    //
    // chat_message is compact JSON with a fixed key order:
    //   {"message_id":"<36-byte uuid>","role":"assistant",...
    // Both invariants are anchored with cheap byte-prefix checks instead of
    // json_extract on the multi-KB blob (which would pull every overflow
    // page). Verified over the whole table: the filters match exactly the
    // rows where json_extract($.role) = 'assistant', and substr(16,36)
    // equals $.message_id for every one of them (0 mismatches). If Devin
    // ever changes the serialization, rows are missed — never merged.
    let mut calls = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT session_id,
                    substr(chat_message, 16, 36) AS mid,
                    MAX(COALESCE(json_extract(metadata, '$.num_tokens_preceding'), 0)),
                    MIN(created_at)
             FROM message_nodes
             WHERE substr(chat_message, 1, 15) = '{\"message_id\":\"'
               AND substr(chat_message, 1, 120) LIKE '%\"role\":\"assistant\"%'
             GROUP BY session_id, mid",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, f64>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for r in rows {
            let (session, prompt, ts) = r?;
            calls.push(DbCall {
                session,
                ts,
                prompt: prompt as u64,
            });
        }
    }

    Ok(DbData {
        session_models,
        calls,
    })
}
