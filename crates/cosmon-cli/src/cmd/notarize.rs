// SPDX-License-Identifier: AGPL-3.0-only

//! `cs notarize` — operator-signed attestations over a molecule's commitment.
//!
//! Two subcommands:
//!
//! - `cs notarize issue <MOLECULE_ID>` — build the canonical
//!   [`cosmon_notary::Commitment`] from the molecule's state, sign it
//!   with Ed25519, and write the resulting [`cosmon_notary::Seal`] to
//!   `<mol_dir>/mint.json`. The legacy flat form
//!   (`cs notarize <MOLECULE_ID> [--key …]`) remains accepted as a
//!   deprecated alias for `issue`.
//! - `cs notarize verify <PATH>` — load a seal JSON file (typically
//!   `mint.json` or `slides.notarization.json`), recompute the
//!   canonical commitment bytes, and Ed25519-verify the signature
//!   full-circle via [`cosmon_notary::verify_seal`]. This replaces the
//!   Python `cryptography` shim that previously lived in
//!   `theater/pitch-*/verify.sh`.
//!
//! See ADR-056 for the protocol and the rationale.
//!
//! # Modes (issue)
//!
//! - `--dry-run` (default when no key is provided): compute the
//!   commitment and report `content_hash`, but do not sign or write
//!   any file. Useful for auditing what a seal *would* commit to.
//! - `--key <path>`: load an Ed25519 secret from `<path>` (raw 32
//!   bytes, or 64-char lowercase hex) and produce a signed
//!   [`cosmon_notary::Seal`]. Writes `mint.json` into the molecule
//!   directory.
//!
//! # Exit codes
//!
//! - `issue` — `0` mint produced (or dry-run succeeded), `1`
//!   molecule not found / commitment error / signing error.
//! - `verify` — `0` signature verifies, `1` I/O or parse error,
//!   `2` signature invalid (wrong key, forged bytes, or tampered
//!   commitment), `3` unknown scheme or unsupported canonical version.

use std::path::{Path, PathBuf};

use cosmon_hash::Hash;
use cosmon_notary::commitment::{merkle_root_stub, Nonce};
use cosmon_notary::verify::{verify_seal, SealVerifyError};
use cosmon_notary::{Commitment, Ed25519Scheme, Scheme, Seal, SigningError};

use super::Context;

/// Arguments for `cs notarize`.
///
/// Accepts either a subcommand (`issue` / `verify`) or the legacy
/// flat form (a molecule id + optional `--key` / `--dry-run` flags).
/// The legacy form is preserved so existing Makefiles
/// (`theater/pitch-*/Makefile`) do not break; new callers should
/// use `cs notarize issue …`.
#[derive(clap::Args)]
pub struct Args {
    /// Sub-command. When absent, the positional `molecule_id` and
    /// its flags take effect as a legacy `issue` invocation.
    #[command(subcommand)]
    pub command: Option<NotarizeCommand>,

    /// Legacy: molecule ID (or prefix) to notarize. Equivalent to
    /// `cs notarize issue <MOLECULE_ID>`.
    #[arg(value_name = "MOLECULE_ID")]
    pub molecule_id: Option<String>,

    /// Legacy: skip signing — compute and print the commitment only.
    #[arg(long)]
    pub dry_run: bool,

    /// Legacy: path to an Ed25519 secret-key file.
    #[arg(long, value_name = "PATH")]
    pub key: Option<PathBuf>,

    /// Legacy: override `cosmon_version` in the commitment.
    #[arg(long)]
    pub cosmon_version: Option<String>,
}

/// `cs notarize` subcommands.
#[derive(clap::Subcommand)]
pub enum NotarizeCommand {
    /// Issue a new seal — build the commitment, sign with Ed25519, write `mint.json`
    Issue(IssueArgs),
    /// Verify an existing seal — full Ed25519 + canonical commitment bytes
    Verify(VerifyArgs),
}

/// Arguments for `cs notarize issue`.
#[derive(clap::Args)]
pub struct IssueArgs {
    /// Molecule ID (or prefix) to notarize.
    pub molecule_id: String,

    /// Skip signing — compute and print the commitment only. Default
    /// when `--key` is not provided.
    #[arg(long)]
    pub dry_run: bool,

    /// Path to an Ed25519 secret-key file (raw 32 bytes or
    /// 64-char lowercase hex). Required for a real notarization.
    #[arg(long, value_name = "PATH")]
    pub key: Option<PathBuf>,

    /// Override `cosmon_version` in the commitment (defaults to the
    /// crate-level `CARGO_PKG_VERSION`). Mostly useful for tests.
    #[arg(long)]
    pub cosmon_version: Option<String>,
}

/// Arguments for `cs notarize verify`.
#[derive(clap::Args)]
pub struct VerifyArgs {
    /// Path to a seal JSON file (e.g. `<mol_dir>/mint.json` or
    /// `theater/pitch-*/notary/slides.notarization.json`).
    pub path: PathBuf,
}

/// Execute the `notarize` command.
///
/// # Errors
///
/// Returns an error on I/O failure, parse failure, or signing failure.
/// `verify` may also exit the process with a non-zero status — see the
/// command docs above.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        Some(NotarizeCommand::Issue(issue)) => run_issue(ctx, issue),
        Some(NotarizeCommand::Verify(verify)) => run_verify(ctx, verify),
        None => {
            // Legacy flat form: cs notarize <MOLECULE_ID> [--key …]
            let molecule_id = args.molecule_id.clone().ok_or_else(|| {
                anyhow::anyhow!(
                    "missing subcommand or molecule id — try `cs notarize issue <MOLECULE_ID>` \
                     or `cs notarize verify <PATH>`"
                )
            })?;
            let issue = IssueArgs {
                molecule_id,
                dry_run: args.dry_run,
                key: args.key.clone(),
                cosmon_version: args.cosmon_version.clone(),
            };
            run_issue(ctx, &issue)
        }
    }
}

/// Issue a new seal for a molecule.
#[allow(clippy::too_many_lines)]
fn run_issue(ctx: &Context, args: &IssueArgs) -> anyhow::Result<()> {
    let state_dir = ctx.config.clone().unwrap_or_else(super::default_state_dir);
    let store = ctx.store_at(&state_dir);

    // Resolve prefix to full ID.
    let all = store.list_molecules(&cosmon_state::MoleculeFilter::default())?;
    let needle = &args.molecule_id;
    let matches: Vec<_> = all
        .iter()
        .filter(|m| m.id.as_str().starts_with(needle.as_str()) || m.id.as_str() == needle)
        .collect();
    let data = match matches.as_slice() {
        [one] => (*one).clone(),
        [] => anyhow::bail!("no molecule matches '{needle}'"),
        many => anyhow::bail!("ambiguous prefix '{needle}' ({} matches)", many.len()),
    };

    // Determine whether this is a dry-run.
    let dry = args.dry_run || args.key.is_none();

    // Operator pubkey — either the signer's, or a placeholder for
    // dry-run (we still populate the field so `content_hash` is
    // well-defined; the placeholder is the zero pubkey with tag
    // "ed25519").
    let (scheme_opt, pubkey) = if let Some(key_path) = &args.key {
        let scheme = load_ed25519_scheme(key_path)?;
        let pk = scheme.public_key();
        (Some(scheme), pk)
    } else {
        (
            None,
            cosmon_notary::PublicKey::new(Ed25519Scheme::TAG, &[0u8; 32]),
        )
    };

    let cosmon_version = args
        .cosmon_version
        .clone()
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());

    // Build commitment from molecule state.
    let prompt_hash: Hash = match &data.prompt_seal {
        Some(seal) => seal.hash.parse().map_err(|e| {
            anyhow::anyhow!("molecule {} has invalid prompt_seal.hash: {e:?}", data.id)
        })?,
        None => anyhow::bail!(
            "molecule {} has no prompt_seal — cannot mint without the prompt content hash. \
             Re-run `cs nucleate` or repair the seal before minting.",
            data.id
        ),
    };

    let briefing_leaves: Vec<Hash> = data
        .briefing_seals
        .iter()
        .map(|s| {
            s.hash
                .parse::<Hash>()
                .map_err(|e| anyhow::anyhow!("briefing seal hex parse: {e:?}"))
        })
        .collect::<Result<_, _>>()?;
    let briefing_root = merkle_root_stub(&briefing_leaves);

    // Phase-0 validator set = {operator_pubkey} → epoch 0 singleton.
    let pk_hash = Hash::of_bytes(&pubkey.to_bytes());
    let validator_set_root = merkle_root_stub(&[pk_hash]);

    // Hash the formula-toml if we can find it; otherwise record a
    // well-defined placeholder so the commitment is never undefined.
    let formula_id_str = data.formula_id.to_string();
    let formula_version_hash = guess_formula_version_hash(&formula_id_str, &state_dir);

    let commitment = Commitment {
        molecule_id: data.id.as_str().to_owned(),
        kind: format!("{:?}", data.kind).to_lowercase(),
        prompt_content_hash: prompt_hash,
        briefing_seals_root: briefing_root,
        parent_commitments: vec![],
        formula_id: formula_id_str,
        formula_version_hash,
        cosmon_version,
        operator_pubkey: pubkey,
        validator_set_epoch: 0,
        validator_set_root,
        nucleated_at_unix_ms: data.created_at.timestamp_millis(),
        nonce: Nonce::random(),
        dedup_key: None,
        canonical_version: cosmon_notary::CANONICAL_COMMITMENT_VERSION,
    };

    let content_hash = commitment
        .content_hash()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if dry {
        let payload = serde_json::json!({
            "molecule_id": data.id.as_str(),
            "content_hash": content_hash.to_hex(),
            "dry_run": true,
            "canonical_version": commitment.canonical_version,
        });
        if ctx.json {
            println!("{payload}");
        } else {
            println!("mint dry-run for {}", data.id);
            println!("  content_hash: {}", content_hash.to_hex());
            println!("  (no signature produced — pass --key to mint)");
        }
        return Ok(());
    }

    // Sign.
    let scheme = scheme_opt.expect("dry branch already returned when scheme was None");
    let seal = Seal::issue(commitment, &scheme).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mol_dir = store.molecule_dir(&data.id);
    let mint_path = mol_dir.join("mint.json");
    let bytes = serde_json::to_vec_pretty(&seal)?;
    std::fs::write(&mint_path, &bytes)?;

    if ctx.json {
        let out = serde_json::json!({
            "molecule_id": data.id.as_str(),
            "content_hash": content_hash.to_hex(),
            "mint_path": mint_path.display().to_string(),
        });
        println!("{out}");
    } else {
        println!(
            "minted {} — content_hash {} → {}",
            data.id,
            content_hash.to_hex(),
            mint_path.display()
        );
    }
    Ok(())
}

/// Verify a seal JSON file end-to-end (full Ed25519 + canonical bytes).
///
/// The seal file is any `cosmon_notary::Seal` written as JSON — `mint.json`
/// under `.cosmon/state/molecules/<id>/`, or a standalone
/// `slides.notarization.json` under `theater/pitch-*/notary/`. This
/// function replaces the Python `cryptography` shim that previously
/// approximated the check at `theater/pitch-*/verify.sh`.
///
/// Exit codes match `theater/pitch-*/verify.sh`:
/// - `0` — signature verifies.
/// - `1` — I/O or parse error (file missing, malformed JSON).
/// - `2` — signature invalid (wrong key, forged bytes, tampered
///   commitment).
/// - `3` — unknown scheme or unsupported canonical version (this build
///   cannot verify this seal without upgrade).
///
/// Returns `Ok(())` on PASS; otherwise calls `std::process::exit(n)`
/// with `n` as above.
#[allow(clippy::unnecessary_wraps)]
fn run_verify(ctx: &Context, args: &VerifyArgs) -> anyhow::Result<()> {
    let path = &args.path;

    // Layer 1 — I/O + parse.
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            emit_verify(
                ctx,
                path,
                None,
                "error",
                &format!("read {}: {e}", path.display()),
            );
            std::process::exit(1);
        }
    };
    let seal: Seal = match serde_json::from_slice(&bytes) {
        Ok(s) => s,
        Err(e) => {
            emit_verify(ctx, path, None, "error", &format!("parse seal JSON: {e}"));
            std::process::exit(1);
        }
    };

    // Layer 2 — full Ed25519 verify over canonical commitment bytes.
    match verify_seal(&seal) {
        Ok(()) => {
            emit_verify(
                ctx,
                path,
                Some(&seal),
                "pass",
                "Ed25519 signature binds operator pubkey to canonical commitment bytes",
            );
            Ok(())
        }
        Err(SealVerifyError::Signature(SigningError::VerifyFailed)) => {
            emit_verify(
                ctx,
                path,
                Some(&seal),
                "fail",
                "signature verification failed — wrong key, forged signature, or tampered commitment",
            );
            std::process::exit(2);
        }
        Err(SealVerifyError::Signature(e)) => {
            emit_verify(
                ctx,
                path,
                Some(&seal),
                "fail",
                &format!("signature decode error: {e}"),
            );
            std::process::exit(2);
        }
        Err(SealVerifyError::UnknownScheme(tag)) => {
            emit_verify(
                ctx,
                path,
                Some(&seal),
                "error",
                &format!("unknown signature scheme '{tag}' — rebuild with scheme support"),
            );
            std::process::exit(3);
        }
        Err(SealVerifyError::UnsupportedVersion(v)) => {
            emit_verify(
                ctx,
                path,
                Some(&seal),
                "error",
                &format!("unsupported canonical_version {v} — upgrade cosmon-notary"),
            );
            std::process::exit(3);
        }
        Err(SealVerifyError::Canonical(e)) => {
            emit_verify(
                ctx,
                path,
                Some(&seal),
                "fail",
                &format!("canonicalization failed: {e}"),
            );
            std::process::exit(2);
        }
    }
}

/// Print the result of a verify invocation, respecting `--json`.
fn emit_verify(ctx: &Context, path: &Path, seal: Option<&Seal>, status: &str, detail: &str) {
    if ctx.json {
        let mut obj = serde_json::json!({
            "path": path.display().to_string(),
            "status": status,
            "detail": detail,
        });
        if let Some(seal) = seal {
            obj["molecule_id"] = seal.commitment.molecule_id.clone().into();
            obj["operator_pubkey"] = seal.commitment.operator_pubkey.bytes_hex.clone().into();
            obj["canonical_version"] = seal.commitment.canonical_version.into();
        }
        println!("{obj}");
    } else {
        let label = match status {
            "pass" => "PASS",
            "fail" => "FAIL",
            _ => "ERROR",
        };
        println!("notarize verify: {label} — {}", path.display());
        if let Some(seal) = seal {
            println!("  molecule    : {}", seal.commitment.molecule_id);
            let pk = &seal.commitment.operator_pubkey.bytes_hex;
            let tail = pk.get(pk.len().saturating_sub(4)..).unwrap_or("");
            let head = &pk[..pk.len().min(12)];
            println!("  pubkey      : ed25519:{head}…{tail}");
            println!("  sealed at   : {}", seal.sealed_at.to_rfc3339());
        }
        println!("  detail      : {detail}");
    }
}

/// Read an Ed25519 secret from a file.
///
/// Accepts either:
/// - 32 raw bytes (binary), or
/// - 64-char lowercase hex (with trailing whitespace OK).
fn load_ed25519_scheme(path: &Path) -> anyhow::Result<Ed25519Scheme> {
    let raw = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("could not read key file {}: {e}", path.display()))?;
    // Try raw 32 bytes first.
    if raw.len() == 32 {
        return Ed25519Scheme::from_secret_bytes(&raw).map_err(|e| anyhow::anyhow!("{e}"));
    }
    // Otherwise parse as hex.
    let trimmed = std::str::from_utf8(&raw)
        .map_err(|e| anyhow::anyhow!("key file is not 32 raw bytes and not UTF-8 hex: {e}"))?
        .trim();
    if trimmed.len() != 64 {
        anyhow::bail!(
            "key file must be 32 raw bytes or 64-char hex; got {} chars",
            trimmed.len()
        );
    }
    let bytes = hex_decode(trimmed)?;
    Ed25519Scheme::from_secret_bytes(&bytes).map_err(|e| anyhow::anyhow!("{e}"))
}

fn hex_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        anyhow::bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> anyhow::Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => anyhow::bail!("non-hex character: {}", char::from(other)),
    }
}

/// Hash the formula TOML if it can be located on disk. Phase-0
/// best-effort: if we cannot find the formula file we record a
/// deterministic placeholder keyed on the formula ID so that two mints
/// of the same molecule produce the same placeholder (and therefore
/// the same `content_hash`).
fn guess_formula_version_hash(formula_id: &str, state_dir: &Path) -> Hash {
    // Look under `.cosmon/formulas/<id>.formula.toml` relative to the
    // state dir's parent.
    let maybe = state_dir.parent().map(|p| {
        p.join("formulas")
            .join(format!("{formula_id}.formula.toml"))
    });
    if let Some(path) = maybe {
        if let Ok(bytes) = std::fs::read(&path) {
            return Hash::of_bytes(&bytes);
        }
    }
    // Placeholder — deterministic per formula ID.
    let mut buf = b"cosmon-mint/v1/formula-placeholder\x00".to_vec();
    buf.extend_from_slice(formula_id.as_bytes());
    Hash::of_bytes(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_notary::commitment::{merkle_root_stub, Commitment, Nonce};
    use tempfile::TempDir;

    fn sample_seal() -> (Ed25519Scheme, Seal) {
        let op = Ed25519Scheme::generate_from_seed([42u8; 32]);
        let pk = op.public_key();
        let pk_hash = Hash::of_bytes(&pk.to_bytes());
        let commitment = Commitment {
            molecule_id: "task-verify-fix".into(),
            kind: "task".into(),
            prompt_content_hash: Hash::of_bytes(b"p"),
            briefing_seals_root: merkle_root_stub(&[Hash::of_bytes(b"b")]),
            parent_commitments: vec![],
            formula_id: "task-work".into(),
            formula_version_hash: Hash::of_bytes(b"f"),
            cosmon_version: "0.1.0".into(),
            operator_pubkey: pk,
            validator_set_epoch: 0,
            validator_set_root: merkle_root_stub(&[pk_hash]),
            nucleated_at_unix_ms: 1_714_000_000_000,
            nonce: Nonce::from_bytes([7u8; 32]),
            dedup_key: None,
            canonical_version: 1,
        };
        let seal = Seal::issue(commitment, &op).unwrap();
        (op, seal)
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = vec![0u8, 1, 2, 3, 255];
        let s = "00010203ff";
        let decoded = hex_decode(s).unwrap();
        assert_eq!(bytes, decoded);
    }

    #[test]
    fn hex_rejects_odd_length() {
        assert!(hex_decode("abc").is_err());
    }

    #[test]
    fn guess_formula_hash_is_deterministic() {
        let dir = std::env::temp_dir();
        let a = guess_formula_version_hash("task-work", &dir);
        let b = guess_formula_version_hash("task-work", &dir);
        assert_eq!(a, b);
        let c = guess_formula_version_hash("deep-think", &dir);
        assert_ne!(a, c);
    }

    /// Happy path: a freshly-issued seal verifies via `verify_seal`.
    /// This is the contract that the CLI `verify` subcommand exposes.
    #[test]
    fn seal_verifies_via_full_ed25519() {
        let (_op, seal) = sample_seal();
        verify_seal(&seal).expect("fresh seal must verify");
    }

    /// Tampering with the commitment after signing must flip verify to FAIL.
    /// This is the core guarantee the Python shim could not make.
    #[test]
    fn tampered_seal_fails_full_verify() {
        let (_op, mut seal) = sample_seal();
        seal.commitment.molecule_id = "task-wrong".into();
        assert!(matches!(
            verify_seal(&seal),
            Err(SealVerifyError::Signature(SigningError::VerifyFailed))
        ));
    }

    /// Writing a seal to disk and reading it back preserves verification.
    /// This is the round-trip `cs notarize verify <path>` performs.
    #[test]
    fn seal_roundtrips_through_disk() {
        let (_op, seal) = sample_seal();
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("mint.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&seal).unwrap()).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let loaded: Seal = serde_json::from_slice(&bytes).unwrap();
        verify_seal(&loaded).expect("round-trip seal must verify");
        assert_eq!(loaded, seal);
    }
}
