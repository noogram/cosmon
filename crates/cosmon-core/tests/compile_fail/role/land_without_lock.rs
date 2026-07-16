//! Even a `Stitcher` cannot `land` while `Unlocked`: `land` exists only
//! on the `TrunkHeld` lock-state. The lock-state typestate forbids
//! merging to trunk without first acquiring the write-token (ADR-110 I1).

use cosmon_core::id::WorkerId;
use cosmon_core::role::{Stitcher, TypedWorker};

fn main() {
    let stitcher = TypedWorker::<Stitcher>::new(WorkerId::new("stitch-1").unwrap());
    // `stitcher` is in the `Unlocked` state — `land` is not defined here.
    let _ = stitcher.land();
}
