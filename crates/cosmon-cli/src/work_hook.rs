// SPDX-License-Identifier: AGPL-3.0-only

//! Quiet `PostToolUse` delivery of declared work messages to a worker.
//!
//! This entry runs before ordinary CLI initialization. Its only stdout is one
//! structured hook document, written through a saved descriptor after all
//! incidental stdout has been redirected to `/dev/null`.

use std::fs::{self, OpenOptions};
use std::os::fd::FromRawFd as _;
use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};
use chrono::Utc;
use cosmon_core::advisory_attempt::{AdvisoryObservation, AdvisorySeatId};
use cosmon_core::id::MoleculeId;
use cosmon_core::work_message::{
    deliverable, fold, render_for_context, AdapterCapability, ContextObservability,
    ContextObservation, DeliveryAdapter, DeliveryOutcome, LiveInsertion, ObserverId, Receipt,
    SafePoint, Stage, WorkMessageStore, WORK_MESSAGE_SCHEMA_VERSION,
};
use cosmon_state::work_message::FileWorkMessageStore;
use serde::Deserialize;

/// The entry point named by the Claude and Codex hook settings.
pub const HOOK_SUBCOMMAND: &str = "work-hook";

#[derive(Deserialize)]
struct WorkRef {
    owner_molecule: MoleculeId,
    seat: AdvisorySeatId,
}

/// Intercept a work hook before the ordinary CLI emits any output or events.
///
/// Every recognized invocation exits zero. A missing work reference is a
/// normal no-op; custody errors produce a reason on stderr and no context.
#[must_use]
pub fn intercept() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next()?.to_str()? != HOOK_SUBCOMMAND {
        return None;
    }
    let adapter = match args.next().as_deref().and_then(|arg| arg.to_str()) {
        Some("claude") => Some(DeliveryAdapter::ClaudePostToolUse),
        Some("codex") => Some(DeliveryAdapter::CodexPostToolUse),
        _ => None,
    };
    let extra = args.next().is_some();

    let mut output = mute_stdout();
    let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
    if let Some(output) = output.as_mut().filter(|_| !extra) {
        if let Some(adapter) = adapter {
            if let Err(error) = run(adapter, output) {
                eprintln!("work hook: {error:#}");
            }
        }
    }
    Some(0)
}

fn mute_stdout() -> Option<fs::File> {
    // SAFETY: the hook process owns stdout. Keep a duplicate solely for the
    // final JSON document, and redirect every other writer to /dev/null.
    let saved = unsafe { libc::dup(1) };
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
        if null >= 0 {
            if libc::dup2(null, 1) < 0 {
                libc::close(1);
            }
            if null != 1 {
                libc::close(null);
            }
        } else {
            libc::close(1);
        }
    }
    if saved >= 0 {
        // SAFETY: `dup` returned an owned descriptor, consumed exactly once.
        Some(unsafe { fs::File::from_raw_fd(saved) })
    } else {
        None
    }
}

fn run(adapter: DeliveryAdapter, output: &mut impl std::io::Write) -> Result<()> {
    let Some(member_dir) = std::env::var_os("COSMON_MOL_DIR").map(PathBuf::from) else {
        return Ok(());
    };
    let ref_path = member_dir.join("work-ref.json");
    if !ref_path.exists() {
        return Ok(());
    }
    let reference: WorkRef =
        serde_json::from_slice(&fs::read(&ref_path)?).context("unreadable work reference")?;
    let member_id = member_dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("member path has no molecule id")
        .and_then(|name| MoleculeId::new(name).map_err(Into::into))?;
    let owner_dir = member_dir
        .parent()
        .context("member path has no roster")?
        .join(reference.owner_molecule.as_str());
    let store = FileWorkMessageStore::new(owner_dir);
    let lock_path = member_dir
        .parent()
        .context("member path has no roster")?
        .join(reference.owner_molecule.as_str())
        .join("work/work.lock");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path)
        .context("unreadable work lock")?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let records = store.load_all().context("unreadable work store")?;
    let scope = records.scope.context("work scope missing")?;
    if scope.owner != reference.owner_molecule || scope.seat_of(&member_id) != Some(&reference.seat)
    {
        bail!("caller is not in the current work roster");
    }
    let now = Utc::now();
    let projection = fold(&scope, &records.envelopes, &records.receipts, now)?;
    let pending = deliverable(&projection, &reference.seat, adapter, now);
    let mut blocks = Vec::new();
    for envelope in &pending {
        let payload = store.read_payload(envelope)?;
        blocks.push(render_for_context(envelope, &payload)?);
    }
    if !records
        .capabilities
        .iter()
        .any(|record| record.seat == reference.seat && record.adapter == adapter)
    {
        store.record_capability(&AdapterCapability {
            schema_version: WORK_MESSAGE_SCHEMA_VERSION,
            seat: reference.seat.clone(),
            adapter,
            harness_version: AdvisoryObservation::Unavailable {
                reason: cosmon_core::advisory_attempt::AdvisoryUnavailableReason::NotObserved,
            },
            live_insertion: LiveInsertion::Supported,
            context_observation: ContextObservability::Unavailable,
            safe_points: vec![SafePoint::AfterToolCall],
            first_observed_at: now,
        })?;
    }
    if blocks.is_empty() {
        return Ok(());
    }
    let doc = serde_json::json!({"hookSpecificOutput": {
        "hookEventName": "PostToolUse", "additionalContext": blocks.join("\n\n")
    }});
    serde_json::to_writer(&mut *output, &doc)?;
    output.write_all(b"\n")?;
    output.flush()?;
    for envelope in pending {
        let observer = ObserverId::Adapter { adapter };
        store.append_receipt(&Receipt::for_envelope(
            envelope,
            observer.clone(),
            now,
            Stage::DeliveryAttempted {
                adapter,
                mechanism: "PostToolUse".to_owned(),
                outcome: DeliveryOutcome::Submitted,
            },
        ))?;
        store.append_receipt(&Receipt::for_envelope(
            envelope,
            observer,
            now,
            Stage::ContextDelivered {
                adapter,
                observation: ContextObservation::Unknown {
                    reason: "hook stdout; model input not observable".to_owned(),
                },
            },
        ))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn stray_fd_one_output_is_muted() {
        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", "work_hook::tests::mute_child"])
            .env("COSMON_WORK_HOOK_MUTE_CHILD", "1")
            .output()
            .expect("child output");
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).expect("stdout text");
        assert!(text.contains("final\n"), "{text}");
        assert!(!text.contains("stray"), "{text}");
    }

    #[test]
    fn mute_child() {
        if std::env::var_os("COSMON_WORK_HOOK_MUTE_CHILD").is_none() {
            return;
        }
        let mut output = mute_stdout().expect("saved stdout");
        // SAFETY: fd 1 is the muted stdout of this isolated child process.
        unsafe { libc::write(1, b"stray\n".as_ptr().cast(), 6) };
        output.write_all(b"final\n").expect("final document");
    }
}
