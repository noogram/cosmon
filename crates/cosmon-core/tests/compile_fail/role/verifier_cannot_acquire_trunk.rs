//! A `Verifier` does not implement `CanWriteTrunk`, so it cannot acquire
//! the trunk write-token. ADR-110 I1 WRITER-UNIQUE at the type level.

use cosmon_core::id::WorkerId;
use cosmon_core::role::{TypedWorker, Verifier};

fn main() {
    let verifier = TypedWorker::<Verifier>::new(WorkerId::new("verify-1").unwrap());
    let _held = verifier.acquire_trunk();
}
