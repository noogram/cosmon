// SPDX-License-Identifier: AGPL-3.0-only

//! Read only the usage projection of the `OpenCode` 2 session database.

use std::path::{Path, PathBuf};

use cosmon_core::id::{MoleculeId, WorkerId};
use cosmon_observability::opencode::{parse_opencode_session, OpencodeReading};
use cosmon_transport::opencode::{OpencodeSessionBinding, SESSION_BINDING_FILE};
use rusqlite::{Connection, OpenFlags};

fn database_path() -> Option<PathBuf> {
    let file = std::env::var_os("OPENCODE_DB").unwrap_or_else(|| "opencode.db".into());
    let path = PathBuf::from(file);
    if path == Path::new(":memory:") {
        return None;
    }
    if path.is_absolute() {
        return Some(path);
    }
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local/share")))?;
    Some(data_home.join("opencode").join(path))
}

/// The database path and session key stay local so observation IDs can be
/// derived from the actual source without serializing its native identifier.
pub(crate) struct SessionReading {
    pub(crate) db_path: PathBuf,
    pub(crate) session_key: String,
    pub(crate) reading: OpencodeReading,
}

pub(crate) fn read_session(
    state_dir: &Path,
    molecule: &MoleculeId,
    worker: &WorkerId,
) -> Option<SessionReading> {
    let dir = cosmon_state::archive::resolve_molecule_dir(state_dir, molecule)?;
    let binding: OpencodeSessionBinding =
        serde_json::from_slice(&std::fs::read(dir.join(SESSION_BINDING_FILE)).ok()?).ok()?;
    if binding.worker_id != *worker || binding.title.is_empty() {
        return None;
    }
    let db_path = database_path()?;
    let mut db = Connection::open_with_flags(&db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    db.busy_timeout(std::time::Duration::ZERO).ok()?;
    let tx = db.transaction().ok()?;
    let (session_key, model) = {
        let mut query = tx
            .prepare("SELECT id, model FROM session_v2 WHERE title = ?1 LIMIT 2")
            .ok()?;
        let mut matches = query.query([&binding.title]).ok()?;
        let row = matches.next().ok()??;
        let key: String = row.get(0).ok()?;
        let model: Option<String> = row.get(1).ok()?;
        // A duplicated binding cannot establish which session owns the usage.
        if matches.next().ok()?.is_some() {
            return None;
        }
        (key, model)
    };
    let mut query = tx.prepare(
        "SELECT type,
                CASE WHEN type = 'assistant' THEN json_object(
                    'model', json_extract(data, '$.model'),
                    'finish', json_extract(data, '$.finish'),
                    'tokens', json_extract(data, '$.tokens'),
                    'cost', json_extract(data, '$.cost'))
                WHEN type = 'idle' THEN json_object(
                    'error', json_object('type', json_extract(data, '$.error.type'),
                                         'retryAfterMs', json_extract(data, '$.error.retryAfterMs')))
                ELSE '{}' END
         FROM session_message WHERE session_id = ?1 ORDER BY seq",
    ).ok()?;
    let messages = query
        .query_map([&session_key], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(SessionReading {
        db_path,
        session_key,
        reading: parse_opencode_session(model.as_deref(), messages),
    })
}
