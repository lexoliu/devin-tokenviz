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

pub fn load(path: &Path) -> Result<DbData> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("cannot open {}", path.display()))?;

    let mut session_models = HashMap::new();
    {
        let mut st = conn.prepare("SELECT id, COALESCE(model,'') FROM sessions")?;
        let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for r in rows {
            let (id, m) = r?;
            session_models.insert(id, m);
        }
    }

    // All JSON extraction and per-message_id dedup happens in SQL.
    let mut calls = Vec::new();
    {
        let mut st = conn.prepare(
            "SELECT session_id, mid, MAX(ntp), MIN(ts) FROM (
                 SELECT session_id,
                        json_extract(chat_message, '$.message_id') AS mid,
                        COALESCE(json_extract(metadata, '$.num_tokens_preceding'), 0) AS ntp,
                        created_at AS ts
                 FROM message_nodes
                 WHERE json_extract(chat_message, '$.role') = 'assistant'
             )
             WHERE mid IS NOT NULL
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
