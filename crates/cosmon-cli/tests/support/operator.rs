// SPDX-License-Identifier: AGPL-3.0-only

//! A stand-in for the human at the keyboard: someone who holds a minisign
//! secret key and can sign a takeover challenge with it.
//!
//! This lives in the test harness and nowhere else, deliberately. The shipped
//! tree parses and verifies minisign artefacts and never produces one, because
//! a signing verb in `cs` would be a verb the beneficiary of a grant could
//! call — and the whole mechanism is that it cannot. Putting the signer here
//! keeps that property checkable: `cs` has no code path to this file.
//!
//! What it produces is a real minisign artefact, not a cosmon-shaped
//! lookalike: the `ED` prehashed algorithm, the 42-byte public key, the
//! 74-byte signature blob, and the global signature over
//! `signature ‖ trusted_comment`. `cosmon_notary::minisign` is pinned against
//! a genuine `minisign 0.12` fixture, so the two agree on the real format
//! rather than on a shared mistake.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use blake2::{Blake2b512, Digest as _};
use ed25519_dalek::{Signer as _, SigningKey};

/// An operator with a keypair.
pub struct Operator {
    key: SigningKey,
    key_id: [u8; 8],
}

impl Operator {
    /// Build an operator from a fixed 32-byte seed, so a failing test reruns
    /// with the same key and the same bytes.
    pub fn from_seed(seed: u8) -> Self {
        Self {
            key: SigningKey::from_bytes(&[seed; 32]),
            key_id: [seed, 1, 2, 3, 4, 5, 6, 7],
        }
    }

    /// The `.pub` file contents to pin as the galaxy's trust root.
    pub fn public_key_file(&self) -> String {
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(b"Ed");
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(self.key.verifying_key().as_bytes());
        format!(
            "untrusted comment: minisign public key {}\n{}\n",
            self.key_id_display(),
            BASE64.encode(raw),
        )
    }

    /// The key id as minisign prints it — reversed, uppercase hex.
    pub fn key_id_display(&self) -> String {
        self.key_id
            .iter()
            .rev()
            .map(|b| format!("{b:02X}"))
            .collect()
    }

    /// Sign `message`, producing the four lines of a `.minisig` file.
    pub fn sign(&self, message: &[u8]) -> String {
        let digest = Blake2b512::digest(message);
        let sig = self.key.sign(&digest).to_bytes();

        let mut blob = Vec::with_capacity(74);
        blob.extend_from_slice(b"ED");
        blob.extend_from_slice(&self.key_id);
        blob.extend_from_slice(&sig);

        let trusted_comment = "signed by the operator, in this test";
        let mut global_message = Vec::new();
        global_message.extend_from_slice(&sig);
        global_message.extend_from_slice(trusted_comment.as_bytes());
        let global = self.key.sign(&global_message).to_bytes();

        format!(
            "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: {}\n{}\n",
            BASE64.encode(blob),
            trusted_comment,
            BASE64.encode(global),
        )
    }
}
