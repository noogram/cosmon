// SPDX-License-Identifier: AGPL-3.0-only

//! `cs witness attest` — Layer-2 witness-quorum seal for stress-test
//! molecules ([ADR-085](../../../../docs/adr/085-stress-test-seal-mechanism.md) §3, M3).
//!
//! A *separate cosmon agent* — distinct from the molecule's tackler —
//! reads the operator-sealed prior, computes its BLAKE3 hash, and
//! writes an
//! [`EventV2::SealAttested`](cosmon_core::event_v2::EventV2::SealAttested)
//! envelope to the fleet's `events.jsonl`. The witness reads only the
//! file's bytes (to hash them); it never interprets the prior's
//! content, preserving the structural-independence guarantee that
//! makes the bypass cost asymmetric (ADR-085 §6).
//!
//! Layer 1 (`cosmon-runtime/src/guard.rs::check_prior_seal` — M2)
//! refuses dispatch unless a matching `SealAttested` event whose
//! `prior_b3` matches the on-disk seal has been emitted by a session
//! distinct from the worker's. This command produces that event.
//!
//! # Cheap heuristic — same-session refusal
//!
//! If the witness is invoked from the same tmux session as the
//! molecule's tackler, the command refuses with
//! [`cosmon_runtime::SameSessionRefusal`]. The hardened path
//! (`LaunchAgent` witness, separate process tree) is deferred per
//! ADR-085 §3 (M6).

use std::path::PathBuf;

use cosmon_hash::Hash;
use cosmon_runtime::witness as wit;
use cosmon_state::{event_log, MoleculeFilter};

use super::Context;

/// Top-level arguments for `cs witness`.
#[derive(clap::Args)]
pub struct Args {
    /// Witness sub-command (only `attest` exists today).
    #[command(subcommand)]
    pub command: WitnessCommand,
}

/// `cs witness` subcommands. Open enum so future hardening (e.g. a
/// `verify` counterpart that re-checks an existing attestation against
/// disk state) can land without breaking the CLI surface.
#[derive(clap::Subcommand)]
pub enum WitnessCommand {
    /// Attest a stress-test molecule's prior seal — emit `SealAttested`.
    Attest(AttestArgs),
}

/// Arguments for `cs witness attest`.
#[derive(clap::Args)]
pub struct AttestArgs {
    /// Molecule ID (or prefix) to attest. Must be a stress-test class
    /// molecule (ADR-085 §1); standard-class molecules are refused so a
    /// witness cannot accidentally lend weight to a tactical
    /// deliberation.
    pub molecule_id: String,

    /// Path to the operator-sealed prior. Defaults to
    /// `<molecule_dir>/prior.md`. The witness opens this file, computes
    /// its BLAKE3 hash, and records that hash in the
    /// [`EventV2::SealAttested::prior_b3`](cosmon_core::event_v2::EventV2::SealAttested)
    /// field. **The witness never inspects the file's content beyond
    /// the bytes-to-hash transformation.**
    #[arg(long, value_name = "PATH")]
    pub prior_path: Option<PathBuf>,

    /// Override the witness identity. Defaults to the
    /// [`cosmon_runtime::resolve_witness_id`] heuristic (`$TMUX` first,
    /// then `<host>-<pid>`). Useful for `LaunchAgent` / CI invocations
    /// that want a deterministic identity.
    #[arg(long, value_name = "ID")]
    pub witness_id: Option<String>,
}

/// Execute `cs witness <subcommand>`.
///
/// # Errors
///
/// Returns an error on I/O failure, store-read failure, missing prior,
/// non-stress-test class, or same-session refusal.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        WitnessCommand::Attest(a) => run_attest(ctx, a),
    }
}

fn run_attest(ctx: &Context, args: &AttestArgs) -> anyhow::Result<()> {
    let state_dir = ctx.state_dir();
    let store = ctx.store();

    // Resolve the molecule by exact id or prefix — same gesture as
    // `cs notarize`, `cs run` so operators have one mental model.
    let all = store.list_molecules(&MoleculeFilter::default())?;
    let needle = &args.molecule_id;
    let matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(needle.as_str()) || m.id.as_str() == needle)
        .collect();
    let mol = match matches.as_slice() {
        [one] => (*one).clone(),
        [] => anyhow::bail!("no molecule matches '{needle}'"),
        many => anyhow::bail!("ambiguous prefix '{needle}' ({} matches)", many.len()),
    };

    // ADR-085 §1: only stress-test molecules carry the seal mechanism.
    // A witness that signed a Standard or Infra molecule would dilute
    // the audit value — refuse loudly.
    if !mol.class.requires_seal() {
        anyhow::bail!(
            "molecule {} has class {:?}; only stress-test class can be witnessed (ADR-085 §1)",
            mol.id,
            mol.class
        );
    }

    // Resolve the prior file. Default lives next to the molecule's
    // state.json so the witness can find it without any flag in the
    // common case.
    let mol_dir = store.molecule_dir(&mol.id);
    let prior_path = args
        .prior_path
        .clone()
        .unwrap_or_else(|| mol_dir.join("prior.md"));
    let prior_bytes = std::fs::read(&prior_path).map_err(|e| {
        anyhow::anyhow!(
            "could not read prior file {}: {e}; supply --prior-path or write prior.md before attesting",
            prior_path.display()
        )
    })?;
    // The witness reads bytes to hash; it does not interpret content.
    let prior_b3 = Hash::of_bytes(&prior_bytes).to_hex();

    // Sealed-at: prefer the molecule's recorded prior_seal sealed_at
    // if present (set by M4 once the operator-sealing flow lands);
    // otherwise use the prior file's mtime as the best available
    // wall-clock anchor — an attestation must commit to some sealed_at
    // (the SealAttested event variant requires it).
    let sealed_at = mol.prompt_seal.as_ref().map_or_else(
        || {
            std::fs::metadata(&prior_path)
                .ok()
                .and_then(|m| m.modified().ok())
                .map_or_else(chrono::Utc::now, chrono::DateTime::<chrono::Utc>::from)
        },
        |s| s.sealed_at,
    );

    // Witness identity. CLI override wins; otherwise the runtime
    // heuristic supplies a default.
    let witness_id = args
        .witness_id
        .clone()
        .unwrap_or_else(wit::resolve_witness_id);

    // Same-session refusal — the cheap structural-independence check
    // ADR-085 §3 specifies. The molecule's tackler is recorded in
    // session_name when `cs tackle` ran; absence short-circuits OK.
    wit::refuse_if_same_session(&witness_id, mol.session_name.as_deref())
        .map_err(|e| anyhow::anyhow!(e))?;

    let attested_at = chrono::Utc::now();
    let attestation_b3 =
        wit::compute_attestation_b3(&mol.id, &prior_b3, sealed_at, &witness_id, attested_at);

    let event = cosmon_core::event_v2::EventV2::SealAttested {
        molecule_id: mol.id.clone(),
        prior_b3: prior_b3.clone(),
        sealed_at,
        witness_id: witness_id.clone(),
        attestation_b3: attestation_b3.clone(),
    };

    let events_path = event_log::resolve_events_log_path(&state_dir);
    if let Some(parent) = events_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let seq = event_log::emit_one(&events_path, event, None)?;

    if ctx.json {
        let out = serde_json::json!({
            "molecule_id": mol.id.as_str(),
            "prior_b3": prior_b3,
            "sealed_at": sealed_at.to_rfc3339(),
            "witness_id": witness_id,
            "attested_at": attested_at.to_rfc3339(),
            "attestation_b3": attestation_b3,
            "seq": seq,
        });
        println!("{out}");
    } else {
        println!("witness attest: {} sealed", mol.id);
        println!("  prior_b3       : {prior_b3}");
        println!("  witness_id     : {witness_id}");
        println!("  attestation_b3 : {attestation_b3}");
        println!("  attested_at    : {}", attested_at.to_rfc3339());
        println!("  seq            : {seq}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::MoleculeId;
    use cosmon_core::molecule_class::MoleculeClass;
    use cosmon_filestore::FileStore;
    use cosmon_state::StateStore;
    use std::collections::{BTreeSet, HashMap};
    use tempfile::TempDir;

    fn ctx_with(state: &std::path::Path) -> Context {
        Context {
            verbose: false,
            json: false,
            config: Some(state.to_path_buf()),
        }
    }

    fn mk_stress_mol(id: &str) -> cosmon_state::MoleculeData {
        cosmon_state::MoleculeData {
            id: MoleculeId::new(id).unwrap(),
            fleet_id: cosmon_core::id::FleetId::new("default").unwrap(),
            formula_id: cosmon_core::id::FormulaId::new("deep-think").unwrap(),
            status: cosmon_core::molecule::MoleculeStatus::Pending,
            variables: HashMap::default(),
            assigned_worker: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            total_steps: 4,
            current_step: 0,
            completed_steps: Vec::new(),
            collapse_reason: None,
            collapse_cause: None,
            collapse_reason_kind: None,
            collapsed_step: None,
            links: Vec::new(),
            kind: None,
            class: MoleculeClass::StressTest,
            typed_links: Vec::new(),
            project_id: None,
            assigned_role: None,
            session_name: Some("delib-stress-aabb".to_owned()),
            tags: BTreeSet::new(),
            escalations: Vec::new(),
            freeze_on_last_step: false,
            expires_at: None,
            expiry_policy: None,
            originating_branch: None,
            pending_step: None,
            merged_at: None,
            prompt_seal: None,
            briefing_seals: Vec::new(),
            bootstrap_seals: Vec::new(),
            archived: false,
            last_progress_at: None,
            last_output_at: None,
            nudge_count: 0,
            last_nudged_at: None,
            process: None,
            energy_budget: None,
            stuck_at: None,
            tackled_by: None,
            tackled_at: None,
        }
    }

    #[test]
    fn refuses_standard_class_molecule() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mut mol = mk_stress_mol("delib-20260503-aabb");
        mol.class = MoleculeClass::Standard;
        store.save_molecule(&mol.id, &mol).unwrap();

        let prior = tmp.path().join("prior.md");
        std::fs::write(&prior, b"three pre-committed positions").unwrap();

        let err = run_attest(
            &ctx_with(tmp.path()),
            &AttestArgs {
                molecule_id: mol.id.as_str().to_owned(),
                prior_path: Some(prior),
                witness_id: Some("witness-host-42".to_owned()),
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("only stress-test class can be witnessed"),
            "expected class refusal, got: {err}"
        );
    }

    #[test]
    fn refuses_same_session_witness() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = mk_stress_mol("delib-20260503-aacc");
        store.save_molecule(&mol.id, &mol).unwrap();

        let prior = tmp.path().join("prior.md");
        std::fs::write(&prior, b"prior body").unwrap();

        let err = run_attest(
            &ctx_with(tmp.path()),
            &AttestArgs {
                molecule_id: mol.id.as_str().to_owned(),
                prior_path: Some(prior),
                // Same as session_name on the molecule above.
                witness_id: Some("delib-stress-aabb".to_owned()),
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("witness refusal"),
            "expected same-session refusal, got: {err}"
        );
    }

    #[test]
    fn happy_path_emits_seal_attested_event() {
        let tmp = TempDir::new().unwrap();
        let store = FileStore::new(tmp.path());
        store.save_fleet(&cosmon_state::Fleet::default()).unwrap();
        let mol = mk_stress_mol("delib-20260503-aadd");
        store.save_molecule(&mol.id, &mol).unwrap();

        let prior = tmp.path().join("prior.md");
        let prior_body = b"three pre-committed positions A, B, C";
        std::fs::write(&prior, prior_body).unwrap();

        run_attest(
            &ctx_with(tmp.path()),
            &AttestArgs {
                molecule_id: mol.id.as_str().to_owned(),
                prior_path: Some(prior),
                witness_id: Some("witness-host-42".to_owned()),
            },
        )
        .expect("happy path must succeed");

        // Verify the event was actually persisted in events.jsonl in
        // a shape the M2 guard will accept (correct prior_b3 + a
        // witness_id distinct from the tackler).
        let events_path = tmp.path().join("events.jsonl");
        let raw = std::fs::read_to_string(&events_path).expect("events.jsonl must exist");
        assert!(raw.contains("\"type\":\"seal_attested\""), "raw: {raw}");
        let expected_prior_b3 = Hash::of_bytes(prior_body).to_hex();
        assert!(raw.contains(&expected_prior_b3), "raw: {raw}");
        assert!(raw.contains("witness-host-42"), "raw: {raw}");
        assert!(
            !raw.contains("\"witness_id\":\"delib-stress-aabb\""),
            "tackler must not appear as witness"
        );
    }
}
