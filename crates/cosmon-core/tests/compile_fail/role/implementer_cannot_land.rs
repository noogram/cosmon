//! An `Implementer` does not implement `CanWriteTrunk`, so it can neither
//! acquire the trunk lock nor `land`. Only a `Stitcher` writes the trunk.

use cosmon_core::id::WorkerId;
use cosmon_core::role::{Implementer, TypedWorker};

fn main() {
    let worker = TypedWorker::<Implementer>::new(WorkerId::new("impl-1").unwrap());
    // No `acquire_trunk` (and hence no path to a `TrunkHeld` `land`).
    let _ = worker.acquire_trunk().land();
}
