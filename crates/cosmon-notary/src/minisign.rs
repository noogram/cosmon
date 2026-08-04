// SPDX-License-Identifier: AGPL-3.0-only

//! Minisign signature verification — parse and check, never sign.
//!
//! # Why this module exists, and why it is only half of a signing library
//!
//! `cs sessions takeover grant` needs to tell an operator's gesture from an
//! agent imitating one. The only thing an agent running as the operator cannot
//! do is produce a signature under a key it does not hold, so the gesture is a
//! detached signature and cosmon's job is to check it.
//!
//! Checking is *all* cosmon does. There is deliberately **no signing function
//! for the takeover key anywhere in this tree**: a `sign` verb would be a verb
//! the beneficiary could call, and the whole mechanism is the absence of one.
//! The operator signs with the stock `minisign` binary, whose secret key is
//! passphrase-protected — and a passphrase is the one secret on that host an
//! agent cannot read.
//!
//! Reusing minisign's on-disk format, rather than inventing a cosmon-shaped
//! one, buys three things: the operator already has the tool, the secret is
//! encrypted by code cosmon does not have to get right, and a grant recorded
//! in the ledger stays verifiable with `minisign -V` by somebody who does not
//! trust — or no longer has — this crate.
//!
//! # The format, in the four lines it actually is
//!
//! A public key file:
//!
//! ```text
//! untrusted comment: minisign public key D9E8177654011BB4
//! <base64: "Ed" ‖ key_id[8] ‖ ed25519_public_key[32]>
//! ```
//!
//! A signature file:
//!
//! ```text
//! untrusted comment: signature from minisign secret key
//! <base64: alg[2] ‖ key_id[8] ‖ signature[64]>
//! trusted comment: <text>
//! <base64: global_signature[64]>
//! ```
//!
//! `alg` is `Ed` when the signature covers the message itself and `ED` when it
//! covers its BLAKE2b-512 digest — the default since minisign 0.6, and the one
//! that lets a signer stream a file it never holds whole. Both are accepted.
//!
//! The global signature covers `signature[64] ‖ trusted_comment`, which is
//! what makes the trusted comment evidence rather than decoration: an editor
//! can change the untrusted comment freely and change the trusted one only by
//! invalidating the file.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

/// Why a minisign artefact could not be parsed or did not verify.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MinisignError {
    /// The file did not have the line structure minisign writes.
    #[error("not a minisign {kind}: {why}")]
    Shape {
        /// `public key` or `signature`.
        kind: &'static str,
        /// What was wrong with it.
        why: String,
    },
    /// A base64 payload was not decodable, or decoded to the wrong length.
    #[error("malformed base64 payload: {0}")]
    Payload(String),
    /// An algorithm tag this verifier does not implement.
    #[error("unsupported signature algorithm {0:?} — this build verifies Ed and ED")]
    UnsupportedAlgorithm(String),
    /// The signature names a different key than the one being verified with.
    #[error("signature is from key {presented} but the public key is {expected}")]
    KeyMismatch {
        /// Key id inside the signature.
        presented: String,
        /// Key id of the public key.
        expected: String,
    },
    /// The Ed25519 check failed on the message.
    #[error("signature does not verify")]
    BadSignature,
    /// The Ed25519 check failed on the trusted comment.
    #[error("global signature does not verify — the trusted comment is not attested")]
    BadGlobalSignature,
}

/// A parsed minisign public key: the eight-byte key id and the Ed25519 key.
#[derive(Debug, Clone)]
pub struct MinisignPublicKey {
    key_id: [u8; 8],
    key: VerifyingKey,
}

impl MinisignPublicKey {
    /// Parse the two-line contents of a `.pub` file.
    ///
    /// Tolerant of a bare base64 line with no comment, because that is what an
    /// operator gets when they copy the line `minisign -G` prints rather than
    /// the file it wrote.
    ///
    /// # Errors
    ///
    /// [`MinisignError::Shape`] when no base64 line is present,
    /// [`MinisignError::Payload`] when it does not decode to 42 bytes, and
    /// [`MinisignError::UnsupportedAlgorithm`] for a non-`Ed` key.
    pub fn parse(text: &str) -> Result<Self, MinisignError> {
        let line = payload_line(text, 0).ok_or_else(|| MinisignError::Shape {
            kind: "public key",
            why: "no base64 line — expected an untrusted comment then the key".to_owned(),
        })?;
        let raw = decode(line)?;
        if raw.len() != 42 {
            return Err(MinisignError::Payload(format!(
                "public key decoded to {} bytes, expected 42",
                raw.len()
            )));
        }
        if &raw[0..2] != b"Ed" {
            return Err(MinisignError::UnsupportedAlgorithm(
                String::from_utf8_lossy(&raw[0..2]).into_owned(),
            ));
        }
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&raw[2..10]);
        let mut key_bytes = [0u8; 32];
        key_bytes.copy_from_slice(&raw[10..42]);
        let key = VerifyingKey::from_bytes(&key_bytes)
            .map_err(|e| MinisignError::Payload(format!("not an Ed25519 public key: {e}")))?;
        Ok(Self { key_id, key })
    }

    /// The key id, in file order.
    #[must_use]
    pub fn key_id(&self) -> [u8; 8] {
        self.key_id
    }
}

/// How the message was presented to Ed25519.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// `Ed` — the signature covers the message bytes.
    Legacy,
    /// `ED` — the signature covers `BLAKE2b-512(message)`.
    Prehashed,
}

/// A parsed minisign signature file.
#[derive(Debug, Clone)]
pub struct MinisignSignature {
    /// Which key signed.
    pub key_id: [u8; 8],
    /// Whether the message was prehashed.
    pub algorithm: Algorithm,
    /// The 64-byte Ed25519 signature.
    pub signature: [u8; 64],
    /// The signed comment.
    pub trusted_comment: String,
    /// The 64-byte signature over `signature ‖ trusted_comment`.
    pub global_signature: [u8; 64],
    /// The unsigned comment, kept only so the file can be reproduced verbatim.
    pub untrusted_comment: String,
}

impl MinisignSignature {
    /// Parse the four-line contents of a `.minisig` file.
    ///
    /// # Errors
    ///
    /// [`MinisignError::Shape`] when the four lines are not there,
    /// [`MinisignError::Payload`] on a bad base64 or wrong length, and
    /// [`MinisignError::UnsupportedAlgorithm`] for an algorithm tag other than
    /// `Ed` or `ED`.
    pub fn parse(text: &str) -> Result<Self, MinisignError> {
        let lines: Vec<&str> = text.lines().collect();
        let shape = |why: &str| MinisignError::Shape {
            kind: "signature",
            why: why.to_owned(),
        };
        if lines.len() < 4 {
            return Err(shape(
                "expected four lines — untrusted comment, signature, trusted comment, \
                 global signature",
            ));
        }
        let untrusted_comment = strip_comment(lines[0], "untrusted comment:")
            .ok_or_else(|| shape("first line is not an untrusted comment"))?;
        let trusted_comment = strip_comment(lines[2], "trusted comment:")
            .ok_or_else(|| shape("third line is not a trusted comment"))?;

        let raw = decode(lines[1])?;
        if raw.len() != 74 {
            return Err(MinisignError::Payload(format!(
                "signature decoded to {} bytes, expected 74",
                raw.len()
            )));
        }
        let algorithm = match &raw[0..2] {
            b"Ed" => Algorithm::Legacy,
            b"ED" => Algorithm::Prehashed,
            other => {
                return Err(MinisignError::UnsupportedAlgorithm(
                    String::from_utf8_lossy(other).into_owned(),
                ))
            }
        };
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&raw[2..10]);
        let mut signature = [0u8; 64];
        signature.copy_from_slice(&raw[10..74]);

        let global = decode(lines[3])?;
        if global.len() != 64 {
            return Err(MinisignError::Payload(format!(
                "global signature decoded to {} bytes, expected 64",
                global.len()
            )));
        }
        let mut global_signature = [0u8; 64];
        global_signature.copy_from_slice(&global);

        Ok(Self {
            key_id,
            algorithm,
            signature,
            trusted_comment,
            global_signature,
            untrusted_comment,
        })
    }

    /// The base64 signature line, as it appears in the file.
    #[must_use]
    pub fn signature_line(&self) -> String {
        let tag: &[u8; 2] = match self.algorithm {
            Algorithm::Legacy => b"Ed",
            Algorithm::Prehashed => b"ED",
        };
        let mut raw = Vec::with_capacity(74);
        raw.extend_from_slice(tag);
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(&self.signature);
        BASE64.encode(raw)
    }

    /// The base64 global-signature line, as it appears in the file.
    #[must_use]
    pub fn global_signature_line(&self) -> String {
        BASE64.encode(self.global_signature)
    }
}

/// Verify `signature` over `message` under `public_key`.
///
/// Both the message signature and the global signature are checked. Verifying
/// only the first would leave the trusted comment editable by anyone holding
/// the file, and the trusted comment is the only place minisign records who
/// and when.
///
/// # Errors
///
/// [`MinisignError::KeyMismatch`] when the signature names another key,
/// [`MinisignError::BadSignature`] when the message check fails, and
/// [`MinisignError::BadGlobalSignature`] when the comment check fails.
pub fn verify(
    public_key: &MinisignPublicKey,
    message: &[u8],
    signature: &MinisignSignature,
) -> Result<(), MinisignError> {
    if signature.key_id != public_key.key_id {
        return Err(MinisignError::KeyMismatch {
            presented: hex_key_id(signature.key_id),
            expected: hex_key_id(public_key.key_id),
        });
    }
    let sig = Signature::from_bytes(&signature.signature);
    let signed: Vec<u8> = match signature.algorithm {
        Algorithm::Legacy => message.to_vec(),
        Algorithm::Prehashed => Blake2b512::digest(message).to_vec(),
    };
    public_key
        .key
        .verify(&signed, &sig)
        .map_err(|_| MinisignError::BadSignature)?;

    let mut global_message = Vec::with_capacity(64 + signature.trusted_comment.len());
    global_message.extend_from_slice(&signature.signature);
    global_message.extend_from_slice(signature.trusted_comment.as_bytes());
    public_key
        .key
        .verify(&global_message, &Signature::from_bytes(&signature.global_signature))
        .map_err(|_| MinisignError::BadGlobalSignature)
}

/// Render a key id the way `minisign -G` prints it: reversed, uppercase hex.
#[must_use]
pub fn hex_key_id(key_id: [u8; 8]) -> String {
    key_id.iter().rev().fold(String::new(), |mut acc, b| {
        use std::fmt::Write as _;
        let _ = write!(acc, "{b:02X}");
        acc
    })
}

/// The `n`-th line that is not a comment and not blank.
fn payload_line(text: &str, n: usize) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.contains("comment:"))
        .nth(n)
}

fn strip_comment(line: &str, prefix: &str) -> Option<String> {
    line.strip_prefix(prefix)
        .map(|rest| rest.strip_prefix(' ').unwrap_or(rest).to_owned())
}

fn decode(line: &str) -> Result<Vec<u8>, MinisignError> {
    BASE64
        .decode(line.trim())
        .map_err(|e| MinisignError::Payload(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `minisign -G` / `minisign -S` pair, generated with minisign
    /// 0.12 and pasted verbatim. A hand-rolled fixture would only prove this
    /// parser agrees with itself.
    const PUBLIC_KEY: &str = "untrusted comment: minisign public key D9E8177654011BB4\n\
        RWS0GwFUdhfo2cXncCJhDMZm6ICY0A8vKStQI2LO4//C4saj3AAlazcj\n";

    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key\n\
        RUS0GwFUdhfo2b9XGd7ZHJNB88Y1WY6jD+2gd6YU+NzllvELLvNq+DAeNYgOPp/R78QIMGbJM3ZcC1yqpbt3MoXxBG3ZYTYq+wo=\n\
        trusted comment: cosmon test\n\
        Ri5xtQ3LYSPQw9juk7aYqB0fcQJPk1yhD9N7RyNhOEjHJwwq1jnLcMD+FHOrjsH0m2f+0pdQKYfJGDzBN80ZAw==\n";

    /// The exact bytes that were signed.
    const MESSAGE: &[u8] = b"hello\n";

    fn key() -> MinisignPublicKey {
        MinisignPublicKey::parse(PUBLIC_KEY).expect("parse the minisign public key")
    }

    fn sig() -> MinisignSignature {
        MinisignSignature::parse(SIGNATURE).expect("parse the minisign signature")
    }

    #[test]
    fn a_real_minisign_signature_verifies() {
        assert_eq!(verify(&key(), MESSAGE, &sig()), Ok(()));
    }

    #[test]
    fn the_key_id_matches_what_minisign_printed() {
        assert_eq!(hex_key_id(key().key_id()), "D9E8177654011BB4");
        assert_eq!(sig().key_id, key().key_id());
    }

    #[test]
    fn minisign_defaults_to_the_prehashed_algorithm() {
        assert_eq!(sig().algorithm, Algorithm::Prehashed);
    }

    #[test]
    fn one_flipped_byte_of_the_message_fails() {
        assert_eq!(
            verify(&key(), b"hellp\n", &sig()),
            Err(MinisignError::BadSignature)
        );
    }

    #[test]
    fn editing_the_trusted_comment_invalidates_the_file() {
        let mut tampered = sig();
        tampered.trusted_comment = "signed by somebody else".to_owned();
        assert_eq!(
            verify(&key(), MESSAGE, &tampered),
            Err(MinisignError::BadGlobalSignature)
        );
    }

    #[test]
    fn a_signature_from_another_key_is_named_as_such() {
        let mut foreign = sig();
        foreign.key_id = [0; 8];
        assert!(matches!(
            verify(&key(), MESSAGE, &foreign),
            Err(MinisignError::KeyMismatch { .. })
        ));
    }

    #[test]
    fn the_lines_round_trip_to_the_bytes_they_came_from() {
        let parsed = sig();
        let original: Vec<&str> = SIGNATURE.lines().collect();
        assert_eq!(parsed.signature_line(), original[1]);
        assert_eq!(parsed.global_signature_line(), original[3]);
        assert_eq!(parsed.untrusted_comment, "signature from minisign secret key");
        assert_eq!(parsed.trusted_comment, "cosmon test");
    }

    #[test]
    fn a_bare_base64_line_is_accepted_as_a_public_key() {
        let bare = PUBLIC_KEY.lines().nth(1).expect("the key line");
        assert_eq!(
            MinisignPublicKey::parse(bare)
                .expect("parse a bare key line")
                .key_id(),
            key().key_id()
        );
    }

    #[test]
    fn a_truncated_signature_file_is_a_shape_error() {
        assert!(matches!(
            MinisignSignature::parse("untrusted comment: x\nAAAA\n"),
            Err(MinisignError::Shape { .. })
        ));
    }

    #[test]
    fn a_public_key_of_the_wrong_length_is_a_payload_error() {
        assert!(matches!(
            MinisignPublicKey::parse("RWQ="),
            Err(MinisignError::Payload(_))
        ));
    }
}
