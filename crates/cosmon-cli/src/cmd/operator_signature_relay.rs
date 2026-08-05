// SPDX-License-Identifier: AGPL-3.0-only

//! Hand the challenge to `minisign(1)` and take back its signature — the one
//! command that used to be three (ADR-171, operator ergonomics).
//!
//! # What this is, and what it deliberately is not
//!
//! Before this module, authorising a transfer was three commands, a temporary
//! file, and a `--by` that had to be repeated identically on two of them or
//! the signature covered nothing:
//!
//! ```text
//! cs sessions takeover challenge --mission M --to S --by N > /tmp/c.txt
//! minisign -Sm /tmp/c.txt
//! cs sessions takeover grant --mission M --to S --by N --attestation /tmp/c.txt.minisig
//! ```
//!
//! The operator's verdict on that, 2026-08-05, was that it is laboratory
//! ergonomics; and "it is secure" is not a licence for "it is painful". So
//! `--sign-with` folds the three into one. What it does **not** do is give
//! cosmon a stamp:
//!
//! > cosmon still owns no signing path. It writes the challenge to a file,
//! > runs the operator's `minisign` binary on it, and reads back the `.minisig`
//! > that binary produced. The secret key is opened by minisign, the
//! > passphrase is read by minisign from the terminal, and neither ever
//! > crosses this process.
//!
//! The child inherits stdin, stdout and stderr precisely so that the
//! passphrase prompt is between the operator and `minisign(1)`. Capturing that
//! stream — to "make the output nicer" — would put the passphrase in cosmon's
//! address space and is the one change this module must never take.
//!
//! # Why the challenge is printed before signing
//!
//! Signing blind is not a gesture, it is a reflex. The bytes are echoed to
//! stderr, above minisign's own prompt, so the transfer the operator is about
//! to authorise — which mission, which session, which epoch — is on screen
//! while the passphrase is being typed. Stderr rather than stdout, so that
//! `cs … grant --json | jq` still parses.
//!
//! # Why no `.minisig` is left behind
//!
//! Both the challenge and its signature live in a [`tempfile::TempDir`] that
//! is removed when this function returns, on the success path and on every
//! error path. The operator asked for one command, not for one command plus a
//! cleanup they must remember.

use std::path::Path;
use std::process::Command;

use cosmon_core::operator_attestation::GrantChallenge;

/// Environment variable naming the `minisign` binary to run.
///
/// Exists so a test can stand a stub in for the real signer, and so an
/// operator whose minisign is not on `PATH` (a Nix store path, a wrapper that
/// talks to a smartcard) can say where it is. It changes *which external
/// signer* is invoked; it never makes cosmon the signer.
pub const MINISIGN_BIN_ENV: &str = "COSMON_MINISIGN_BIN";

/// Run the operator's `minisign` over `challenge` and return the `.minisig`
/// text it produced.
///
/// The passphrase prompt happens in the child, on the operator's terminal.
/// This function never sees it, and the temporary files are gone by the time
/// it returns.
///
/// # Errors
///
/// - the signer binary is not installed or not executable;
/// - the operator's secret key file does not exist;
/// - minisign exited non-zero — a wrong passphrase, or an interrupted prompt;
/// - minisign exited zero but wrote no signature next to the challenge.
///
/// Every one of those leaves the ledger untouched: this runs before the grant
/// is appended, and a grant with no attestation is refused anyway.
pub fn sign_challenge(challenge: &GrantChallenge, secret_key: &Path) -> anyhow::Result<String> {
    if !secret_key.exists() {
        return Err(anyhow::anyhow!(
            "no secret key at {} — `--sign-with` wants the operator's minisign \
             secret key, the half `minisign -G` keeps off this repository",
            secret_key.display(),
        ));
    }

    let bin = std::env::var(MINISIGN_BIN_ENV).unwrap_or_else(|_| "minisign".to_owned());
    let dir = tempfile::Builder::new()
        .prefix("cosmon-takeover-")
        .tempdir()
        .map_err(|e| anyhow::anyhow!("failed to make a private directory to sign in: {e}"))?;
    let challenge_path = dir.path().join("challenge.txt");
    std::fs::write(&challenge_path, challenge.canonical_bytes())
        .map_err(|e| anyhow::anyhow!("failed to write the challenge to sign: {e}"))?;

    // Read before you sign. Non-negotiable: the whole point of an operator
    // gesture is that a human saw what it authorises.
    eprintln!(
        "\nabout to authorise this transfer — read it before you type your passphrase:\n\n\
         {indented}\n\
         signing with {bin} and {key}; cosmon never sees your passphrase.\n",
        indented = indent(&challenge.to_string()),
        key = secret_key.display(),
    );

    // Inherited stdio: the passphrase prompt belongs to minisign and the
    // operator, and to nothing in between.
    let status = Command::new(&bin)
        .arg("-S")
        .arg("-s")
        .arg(secret_key)
        .arg("-m")
        .arg(&challenge_path)
        .status()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "no `{bin}` to run: {e}\n\
                     cosmon signs nothing itself — it relays to the operator's minisign. \
                     Install minisign, or point ${MINISIGN_BIN_ENV} at yours."
                )
            } else {
                anyhow::anyhow!("failed to run `{bin}`: {e}")
            }
        })?;

    if !status.success() {
        return Err(anyhow::anyhow!(
            "{bin} did not sign ({status}) — a wrong passphrase or an interrupted \
             prompt. Nothing was granted; the seat is unchanged.",
        ));
    }

    let signature_path = challenge_path.with_extension("txt.minisig");
    let text = std::fs::read_to_string(&signature_path).map_err(|e| {
        anyhow::anyhow!(
            "{bin} reported success but left no signature to read ({e}) — \
             nothing was granted"
        )
    })?;
    // `dir` drops here: neither the challenge nor the signature outlives the
    // command that made them.
    Ok(text)
}

/// Two-space-indent a block, so the challenge reads as a quotation rather than
/// as more of cosmon's own prose.
fn indent(block: &str) -> String {
    block
        .lines()
        .map(|l| format!("  {l}\n"))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmon_core::id::{MoleculeId, SessionId};
    use cosmon_core::pilot_lease::LeaseEpoch;

    fn challenge() -> GrantChallenge {
        GrantChallenge::new(
            MoleculeId::new("task-20260805-2b6d").expect("mission id"),
            SessionId::new("claude-successor").expect("session id"),
            LeaseEpoch::first(),
            "emmanuel",
            None,
        )
        .expect("challenge")
    }

    #[test]
    fn a_missing_secret_key_is_named_before_any_signer_runs() {
        let err = sign_challenge(&challenge(), Path::new("/nonexistent/minisign.key"))
            .expect_err("a key that is not there cannot sign");
        assert!(
            err.to_string().contains("no secret key at"),
            "unhelpful: {err}"
        );
    }

    #[test]
    fn the_challenge_is_indented_line_by_line_for_reading() {
        let rendered = indent("a\nb\n");
        assert_eq!(rendered, "  a\n  b\n");
    }
}
