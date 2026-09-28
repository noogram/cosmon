// SPDX-License-Identifier: AGPL-3.0-only

//! Worker-attempt plan samples: atomic replacement under a per-source lock.
//! Reads never create directories or configure providers. Launch composition
//! requires explicitly resolved settings; this adapter never discovers or
//! rewrites global configuration.

use cosmon_core::{
    id::WorkerId,
    plan_observation::{PlanObservation, PlanObservationStore, PlanSource},
    usage::UnavailableReason,
};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

/// Filesystem adapter rooted in a durable worker-attempt observation directory.
///
/// The caller owns its lifecycle: remove the directory when retiring the
/// attempt. Files contain only normalized samples, not raw provider payloads.
pub struct FilePlanObservationStore {
    root: PathBuf,
}

impl FilePlanObservationStore {
    /// Bind to an explicit attempt root without creating or modifying it.
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, worker: &WorkerId, source: PlanSource) -> PathBuf {
        self.root
            .join(format!("{}-{}.json", worker.as_str(), source.schema()))
    }
}

impl PlanObservationStore for FilePlanObservationStore {
    type Error = io::Error;

    fn save(&self, worker: &WorkerId, sample: &PlanObservation) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        let path = self.path(worker, sample.source);
        let lock = private_file(&path.with_extension("lock"))?;
        fs2::FileExt::lock_exclusive(&lock)?;
        // The lock lives through rename. A slow older invocation cannot
        // replace a newer capture, including after reconstructing this store.
        if self
            .load(worker, sample.source)?
            .is_some_and(|old| old.captured_at >= sample.captured_at)
        {
            return Ok(());
        }
        let bytes = serde_json::to_vec(sample).map_err(io::Error::other)?;
        let pending = path.with_extension("pending");
        let mut file = private_file(&pending)?;
        file.set_len(0)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&pending, &path)?;
        fs::File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    fn load(&self, worker: &WorkerId, source: PlanSource) -> io::Result<Option<PlanObservation>> {
        match fs::read(self.path(worker, source)) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Compose a worker-only status-line overlay from explicitly resolved settings.
///
/// `collector` is a shell-quoted invocation of the internal plan hook. It
/// passes the original stdin through even if persistence fails, so an existing
/// status command sees the same input and owns all display output. Only the
/// status-line object is copied; unrelated settings never enter the overlay.
/// Use the returned JSON with the worker's `--settings` launch carrier. The
/// observer must never call this function as a side effect of reading usage.
///
/// # Errors
/// Returns unsupported if effective settings are unknown, non-object, or have
/// an unrecognized status-line contract. It never guesses which global,
/// managed, project, local, or command-line configuration won precedence.
pub fn claude_statusline_overlay(
    effective: Option<&Value>,
    collector: &str,
) -> Result<Value, UnavailableReason> {
    let effective = effective
        .filter(|v| v.is_object())
        .ok_or(UnavailableReason::Unsupported)?;
    let mut status = match effective.get("statusLine") {
        None | Some(Value::Null) => json!({"type":"command"}),
        Some(value)
            if value.get("type").and_then(Value::as_str) == Some("command")
                && value.get("command").and_then(Value::as_str).is_some() =>
        {
            value.clone()
        }
        Some(_) => return Err(UnavailableReason::Unsupported),
    };
    let command = match status.get("command").and_then(Value::as_str) {
        Some(original) if !original.trim().is_empty() => format!("{collector} | (\n{original}\n)"),
        _ => format!("{collector} > /dev/null"),
    };
    status["command"] = Value::String(command);
    Ok(json!({"statusLine":status}))
}
