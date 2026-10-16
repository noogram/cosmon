// SPDX-License-Identifier: AGPL-3.0-only

//! `cs collapse-receipt` — read back what happened to a collapse key.
//!
//! A broker whose `cs collapse --key` response was lost asks this instead of
//! guessing. The read takes no lock and changes nothing. Its four answers are
//! `committed` (with the receipt `cs collapse` returned), `reserved_not_applied`
//! and `unknown` (both settled by resubmitting the identical request), and
//! `inconsistent` (the key record and the state disagree; a human looks).
//! A key bound to another molecule answers `idempotency_key_reused` and
//! discloses nothing about that molecule.

use cosmon_core::collapse_contract::{IdempotencyKey, ReceiptLookup};
use cosmon_core::id::MoleculeId;

use super::Context;

/// Arguments for the `collapse-receipt` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// Molecule the key was sent for.
    molecule: String,

    /// The idempotency key sent with `cs collapse --key`.
    key: String,

    /// Path to the state store root (overrides walk-up discovery).
    #[arg(long)]
    ops_dir: Option<std::path::PathBuf>,
}

/// Look the key up and print the answer.
///
/// # Errors
/// A malformed id or key, a missing molecule, a store failure; and, for a
/// script to branch on, exit 20 for a key bound to another molecule and exit
/// 26 for an inconsistent record.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    use super::collapse::{exit_code, CollapseRefused};

    let mol_id = MoleculeId::new(&args.molecule)?;
    let key = IdempotencyKey::parse(&args.key)?;
    let ops_dir = cosmon_filestore::resolve_state_dir(args.ops_dir.as_deref());
    let store = ctx.store_at(&ops_dir);
    let lookup = cosmon_state::ops::collapse::lookup_receipt(&*store, &mol_id, &key)?;

    let (receipt, digest, detail) = match &lookup {
        ReceiptLookup::Committed(receipt) => (Some(receipt), None, None),
        ReceiptLookup::ReservedNotApplied { digest } => (None, Some(digest.as_str()), None),
        ReceiptLookup::Inconsistent { detail } => (None, None, Some(detail.as_str())),
        ReceiptLookup::BoundElsewhere | ReceiptLookup::Unknown => (None, None, None),
    };
    if ctx.json {
        let out = serde_json::json!({
            "molecule": mol_id.as_str(),
            "key": key.as_str(),
            "state": lookup.as_str(),
            "receipt": receipt,
            "digest": digest.or_else(|| receipt.map(|r| r.digest.as_str())),
            "detail": detail,
        });
        println!("{}", serde_json::to_string(&out)?);
    } else {
        println!("{mol_id} {key}: {}", lookup.as_str());
        if let Some(receipt) = receipt {
            println!(
                "  collapsed from {} at {} — digest {}",
                receipt.previous_status, receipt.committed_at, receipt.digest
            );
        }
        if let Some(digest) = digest {
            println!("  reserved for digest {digest}; resubmit the identical request");
        }
        if matches!(lookup, ReceiptLookup::Unknown) {
            println!("  nothing admitted under this key; resubmit the identical request");
        }
        if let Some(detail) = detail {
            println!("  {detail}");
        }
    }

    let code = match lookup {
        ReceiptLookup::BoundElsewhere => Some(exit_code::IDEMPOTENCY_KEY_REUSED),
        ReceiptLookup::Inconsistent { .. } => Some(exit_code::RECEIPT_INCONSISTENT),
        _ => None,
    };
    match code {
        Some(code) => Err(CollapseRefused {
            molecule: mol_id.as_str().to_owned(),
            outcome: if code == exit_code::RECEIPT_INCONSISTENT {
                "inconsistent"
            } else {
                "idempotency_key_reused"
            },
            detail: String::new(),
            code,
        }
        .into()),
        None => Ok(()),
    }
}
