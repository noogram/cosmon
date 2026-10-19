// SPDX-License-Identifier: AGPL-3.0-only

//! Positive dispatch evidence prevents name-based fallback after snapshot loss.

/// Fold each worker's latest dispatch hosting witness once per application operation.
/// Missing logs retain legacy compatibility; positive history cannot reconstruct
/// the lost incarnation and therefore grants no authority to control a terminal.
///
/// # Errors
/// Propagates log read failures, which must never authorize fallback.
pub fn pinned_workers(
    state_dir: &std::path::Path,
) -> std::io::Result<std::collections::HashSet<cosmon_core::id::WorkerId>> {
    let events = match crate::event_log::read_all(state_dir.join("events.jsonl")) {
        Ok(events) => events,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let mut pinned = std::collections::HashSet::new();
    for envelope in events {
        if let cosmon_core::event_v2::EventV2::WorkerSpawned {
            worker_id,
            transport,
            ..
        } = envelope.event
        {
            if transport.is_some() {
                pinned.insert(worker_id);
            } else {
                pinned.remove(&worker_id);
            }
        }
    }
    Ok(pinned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::event_v2::EventV2;
    use cosmon_core::id::WorkerId;
    use cosmon_core::transport_selection::{TransportKind, TransportSelection, TransportSource};

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
