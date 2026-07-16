// SPDX-License-Identifier: AGPL-3.0-only

//! PKCE helpers — verifier + S256 challenge.
//!
//! `cosmon-remote auth login` is a thin envelope around the
//! `/v1/auth/claude/{start,email,confirm}` triplet of the
//! cosmon-rpp-adapter. The PKCE crypto lives on the server (the adapter
//! talks to Anthropic), so this module exposes only the small bits the
//! CLI needs: parsing a verification URL and rendering the
//! "open this URL, paste the code back" loop. No PKCE secret material
//! is ever stored on the CLI side.
//!
//! The `start → email → confirm` ping-pong is single-shot and
//! interactive by design (RFC 7636 manual-paste). Storing a session id
//! across CLI invocations would tempt batch tooling to drive auth
//! headlessly; the `no-direct-shell` invariant of ADR-0017 explicitly
//! reserves the gesture to a human-in-loop. The CLI keeps the session
//! id in process memory only, exactly like the justfile kept it in
//! `/tmp/cosmon-auth-session-id.txt` (the justfile crossed process
//! boundaries because each recipe was its own subprocess; one Rust
//! binary does not need that crutch).

use std::io::{self, BufRead, Write};

use crate::error::{Error, Result};

/// Render the manual-paste loop in a TTY: print the verification URL,
/// pause until the operator presses Enter, then read the authorization
/// code from stdin and return it. Trailing whitespace is trimmed; an
/// empty input is an error (matches the adapter's `empty_code` 400).
pub fn prompt_for_code(verification_url: &str) -> Result<String> {
    let mut out = io::stdout();
    writeln!(
        out,
        "\n────────────────────────────────────────────────────────────"
    )?;
    writeln!(out, " Visit this URL in your browser to authenticate:\n")?;
    writeln!(out, "   {verification_url}\n")?;
    writeln!(
        out,
        " After signing in, copy the displayed authorization code"
    )?;
    writeln!(
        out,
        " (the page shows it after the redirect) and paste it below."
    )?;
    writeln!(
        out,
        "────────────────────────────────────────────────────────────"
    )?;
    write!(out, "\nauthorization code › ")?;
    out.flush()?;

    let stdin = io::stdin();
    let mut line = String::new();
    stdin.lock().read_line(&mut line)?;
    let code = line.trim().to_owned();
    if code.is_empty() {
        return Err(Error::Auth("empty authorization code".into()));
    }
    Ok(code)
}

/// Optional helper for callers that want to feed a code from a fixture
/// (tests, smoke harness) without going through the prompt.
pub fn validate_code(code: &str) -> Result<String> {
    let trimmed = code.trim();
    if trimmed.is_empty() {
        return Err(Error::Auth("empty authorization code".into()));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_code_rejects_empty() {
        assert!(validate_code("").is_err());
        assert!(validate_code("   ").is_err());
    }

    #[test]
    fn validate_code_trims() {
        let c = validate_code("  abc#xyz \n").unwrap();
        assert_eq!(c, "abc#xyz");
    }
}
