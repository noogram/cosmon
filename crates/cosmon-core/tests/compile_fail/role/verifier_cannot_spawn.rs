//! A `Verifier` does not implement `CanSpawn`, so `spawn_child` does not
//! exist for it. Molecule §2: "un Verifier ne peut pas spawn un
//! sub-worker s'il n'a pas le rôle."

use cosmon_core::id::WorkerId;
use cosmon_core::role::{TypedWorker, Verifier};

fn main() {
    let verifier = TypedWorker::<Verifier>::new(WorkerId::new("verify-1").unwrap());
    let _ = verifier.spawn_child(WorkerId::new("child-1").unwrap());
}
