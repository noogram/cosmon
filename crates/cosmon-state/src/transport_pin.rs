// SPDX-License-Identifier: AGPL-3.0-only

//! Positive dispatch evidence prevents name-based fallback after snapshot loss.

use std::collections::HashSet;
use std::io::BufRead;

use cosmon_core::event_v2::{Envelope, EventV2};
use cosmon_core::id::{MoleculeId, WorkerId};

/// Positive dispatch witnesses reconstructed together from one ordered log scan.
#[derive(Debug, Default)]
pub struct TransportPins {
    /// Workers whose latest dispatch selected a qualified transport.
    pub workers: HashSet<WorkerId>,
    /// Every molecule ever dispatched with a qualified transport, even after
    /// the worker label was reused by a later legacy dispatch.
    pub molecules: HashSet<MoleculeId>,
}

/// Fold worker and molecule pins together without retaining the event history.
/// Missing logs retain legacy compatibility. Unparseable lines are ignored just
/// as in [`crate::event_log::read_all`]. Positive history grants no authority to
/// reconstruct or control a lost terminal incarnation.
///
/// # Errors
/// Propagates log open and read failures, which must never authorize fallback.
pub fn pins(state_dir: &std::path::Path) -> std::io::Result<TransportPins> {
    let file = match std::fs::File::open(state_dir.join("events.jsonl")) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TransportPins::default());
        }
        Err(error) => return Err(error),
    };
    let mut pins = TransportPins::default();
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        // Match read_all's logical line-byte accounting, including a final row
        // without a newline. read_line propagates invalid UTF-8 before filtering.
        let line = line
            .strip_suffix('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .unwrap_or(&line);
        crate::event_log::note_journal_bytes_read(line.len() as u64 + 1);
        // JSON escapes can encode the discriminator itself. Such rows must use
        // the full parser even when the literal spelling is absent. False
        // positives (e.g. worker_spawned in a reason) are harmless: only the
        // typed event below can change a pin.
        if !line.contains("worker_spawned") && !line.contains('\\') {
            continue;
        }
        let Ok(envelope) = Envelope::from_line(line) else {
            continue;
        };
        if let EventV2::WorkerSpawned {
            worker_id,
            molecule,
            transport,
            ..
        } = envelope.event
        {
            if transport.is_some() {
                pins.workers.insert(worker_id);
                if let Some(molecule) = molecule {
                    pins.molecules.insert(molecule);
                }
            } else {
                pins.workers.remove(&worker_id);
            }
        }
    }
    Ok(pins)
}

/// Fold each worker's latest dispatch hosting witness once per operation.
/// Call [`pins`] when the operation also needs molecule pins.
///
/// # Errors
/// Propagates log read failures; missing logs retain legacy compatibility.
pub fn pinned_workers(state_dir: &std::path::Path) -> std::io::Result<HashSet<WorkerId>> {
    Ok(pins(state_dir)?.workers)
}

/// Remember every molecule that had a qualified dispatch, independently of worker
/// label reuse. Call [`pins`] when the operation also needs worker pins.
///
/// # Errors
/// Propagates log read failures; missing logs retain legacy compatibility.
pub fn pinned_molecules(state_dir: &std::path::Path) -> std::io::Result<HashSet<MoleculeId>> {
    Ok(pins(state_dir)?.molecules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::EventV2;
    use cosmon_core::id::WorkerId;
    use cosmon_core::transport_selection::{TransportKind, TransportSelection, TransportSource};

    #[test]
    fn combined_fold_keeps_escaped_tags_and_order_while_skipping_bad_rows(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let log = concat!(
            "not JSON\n",
            r#"{"type":"worker_spawned","worker_id":"reused","molecule":"witness-20261019-aaaa","transport":{"backend":"tmux","source":"default"}}"#,
            "\n",
            r#"{"kind":"worker_\u0073pawned","worker_id":"escaped","molecule":"witness-20261019-bbbb","transport":{"backend":"tmux","source":"project"}}"#,
            "\r\n",
            r#"{"type":"worker_spawned","worker_id":"reused","molecule":"witness-20261019-cccc"}"#,
            "\n",
            r#"{"type":"molecule_completed","molecule_id":"witness-20261019-dddd","reason":"worker_spawned"}"#,
            "\n",
            r#"{"type":"worker_spawned","worker_id":null}"#,
            "\n   \n",
        );
        std::fs::write(root.path().join("events.jsonl"), log)?;
        let before = crate::event_log::journal_bytes_read_by_this_thread();
        let result = pins(root.path())?;
        // read_all's counter excludes CR from CRLF, so these logical bytes are
        // one fewer than the physical log size. Both sets cost one scan.
        assert_eq!(
            crate::event_log::journal_bytes_read_by_this_thread() - before,
            log.len() as u64 - 1
        );
        assert_eq!(result.workers, HashSet::from([WorkerId::new("escaped")?]));
        assert_eq!(
            result.molecules,
            HashSet::from([
                MoleculeId::new("witness-20261019-aaaa")?,
                MoleculeId::new("witness-20261019-bbbb")?,
            ])
        );
        Ok(())
    }

    #[test]
    fn unreadable_or_invalid_utf8_log_never_authorizes_fallback(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let missing = pins(root.path())?;
        assert!(missing.workers.is_empty());
        assert!(missing.molecules.is_empty());
        let path = root.path().join("events.jsonl");
        std::fs::create_dir(&path)?;
        assert!(pins(root.path()).is_err());
        std::fs::remove_dir(&path)?;
        // An unrelated line still has to be read, including UTF-8 validation.
        std::fs::write(&path, [b'n', b'o', b't', 0xff, b'\n'])?;
        assert_eq!(
            pins(root.path()).err().map(|error| error.kind()),
            Some(std::io::ErrorKind::InvalidData)
        );
        Ok(())
    }

    #[test]
    fn positive_history_survives_snapshot_loss_without_reclassifying_legacy_workers(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let modern = WorkerId::new("modern")?;
        let legacy = WorkerId::new("legacy")?;
        assert!(pinned_workers(root.path())?.is_empty());
        for (worker, transport) in [
            (
                modern.clone(),
                Some(TransportSelection {
                    backend: TransportKind::Tmux,
                    source: TransportSource::Project,
                }),
            ),
            (legacy.clone(), None),
        ] {
            crate::event_log::emit_one(
                root.path().join("events.jsonl"),
                EventV2::WorkerSpawned {
                    transport,
                    worker_id: worker.clone(),
                    molecule: None,
                    session_name: worker.to_string(),
                    role: "polecat".into(),
                    adapter_name: "codex".into(),
                    loop_ownership: Default::default(),
                },
                None,
            )?;
        }
        let pins = pinned_workers(root.path())?;
        assert!(pins.contains(&modern));
        assert!(!pins.contains(&legacy));
        assert!(!root.path().join("molecules").exists());
        Ok(())
    }
}

#[cfg(test)]
mod name_reuse_tests {
    use super::*;
    use cosmon_core::event_v2::EventV2;
    use cosmon_core::id::{MoleculeId, WorkerId};
    use cosmon_core::transport_selection::{TransportKind, TransportSelection, TransportSource};

    #[test]
    fn a_new_legacy_molecule_does_not_erase_the_old_molecule_pin(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let worker = WorkerId::new("reused")?;
        let old = MoleculeId::new("witness-20261019-aaaa")?;
        let new = MoleculeId::new("witness-20261019-bbbb")?;
        assert!(pinned_molecules(root.path())?.is_empty());
        for (molecule, transport) in [
            (
                old.clone(),
                Some(TransportSelection {
                    backend: TransportKind::Tmux,
                    source: TransportSource::Default,
                }),
            ),
            (new.clone(), None),
        ] {
            crate::event_log::emit_one(
                root.path().join("events.jsonl"),
                EventV2::WorkerSpawned {
                    transport,
                    worker_id: worker.clone(),
                    molecule: Some(molecule),
                    session_name: worker.to_string(),
                    role: "polecat".into(),
                    adapter_name: "codex".into(),
                    loop_ownership: Default::default(),
                },
                None,
            )?;
        }
        assert!(!pinned_workers(root.path())?.contains(&worker));
        let molecules = pinned_molecules(root.path())?;
        assert!(molecules.contains(&old));
        assert!(!molecules.contains(&new));
        Ok(())
    }
}

#[cfg(test)]
mod benchmark {
    use std::io::Write;

    #[test]
    #[ignore = "manual production-scale pin fold measurement"]
    fn pin_fold_500k() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("events.jsonl");
        let mut log = std::io::BufWriter::new(std::fs::File::create(&path)?);
        for i in 0..500_000 {
            if i % 10 == 0 {
                writeln!(
                    log,
                    r#"{{"type":"worker_spawned","worker_id":"worker-{}","molecule":"task-20261019-aaaa","transport":{{"backend":"tmux","source":"project"}},"session_name":"synthetic","role":"polecat","adapter_name":"codex"}}"#,
                    i % 1000
                )?;
            } else {
                writeln!(
                    log,
                    r#"{{"type":"molecule_completed","molecule_id":"task-20261019-aaaa","reason":"{}"}}"#,
                    "synthetic payload ".repeat(14)
                )?;
            }
        }
        log.flush()?;
        let start = std::time::Instant::now();
        let pinned = super::pinned_molecules(root.path())?;
        let elapsed = start.elapsed();
        assert_eq!(pinned.len(), 1);
        println!("pin_fold_500k: lines=500000 worker_spawned=50000 bytes={} elapsed_ms={:.3} molecules={}", std::fs::metadata(path)?.len(), elapsed.as_secs_f64() * 1000.0, pinned.len());
        Ok(())
    }
}
