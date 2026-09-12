//! Read per-call usage out of Devin's `sessions.db` (SQLite).
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

    // message_id/message role live in the first ~100 bytes of chat_message;
    // fetch only the head + metadata + ts, then dedupe.
    let mut calls: HashMap<(String, String), (u64, i64)> = HashMap::new();
    {
        let mut st = conn.prepare(
            "SELECT session_id, substr(chat_message,1,300), metadata, created_at \
             FROM message_nodes",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for r in rows {
            let (sid, head, meta, ts) = r?;
            if !is_assistant(&head) {
                continue;
            }
            let Some(mid) = json_str(&head, "message_id") else {
                continue;
            };
            let ntp = meta.as_deref().and_then(num_tokens_preceding).unwrap_or(0);
            let e = calls.entry((sid, mid.to_string())).or_insert((0, i64::MAX));
            e.0 = e.0.max(ntp);
            e.1 = e.1.min(ts);
        }
    }

    Ok(DbData {
        session_models,
        calls: calls
            .into_iter()
            .map(|((session, _), (prompt, ts))| DbCall {
                session,
                ts,
                prompt,
            })
            .collect(),
    })
}

/// head starts `{"message_id":"…","role":"assistant",…}` — check the role field.
fn is_assistant(head: &str) -> bool {
    let Some(i) = head.find("\"role\":") else {
        return false;
    };
    head[i + 7..].trim_start().starts_with("\"assistant\"")
}

/// `"key":"value"` extraction without a full JSON parse.
fn json_str<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = s.find(&pat)? + pat.len();
    let end = s[start..].find('"')? + start;
    Some(&s[start..end])
}

/// `…,"num_tokens_preceding":12345,…` → 12345
fn num_tokens_preceding(meta: &str) -> Option<u64> {
    const PAT: &str = "\"num_tokens_preceding\":";
    let start = meta.find(PAT)? + PAT.len();
    let rest = &meta[start..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}
