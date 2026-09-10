// SPDX-License-Identifier: AGPL-3.0-only

//! `cs-server-image-init-discipline` — boot-time materialization.
//!
//! These tests prove behavioural **equivalence** with the former
//! `cosmon-server-init.sh` ENTRYPOINT (the 8 steps), **idempotence**
//! (safe to re-run on every container restart — B2 eager), and the new
//! **multi-noyau** capability (one galaxy tree per noyau). Since issue
//! #54 the whole materialization is library-direct — `ImageInit` holds
//! no `cs` path at all, so "never shells `cs`" is structural rather
//! than a tripwire: there is no seam left to hand a binary to. `git`
//! is real. Step 3c (Anthropic key injection) moved to the worker
//! envelope and is proven in `cosmon_rpp_adapter::worker_env`'s unit
//! tests (the env build is a pure function there).

use std::path::Path;

use cosmon_rpp_adapter::image_init::{ImageInit, StepOutcome};
use cosmon_rpp_adapter::nucleon_map::Noyau;
use serde_json::Value;

fn image_init_for(td: &Path) -> ImageInit {
    ImageInit {
        inbox_root: td.join("whispers/inbox"),
        galaxies_root: td.join("galaxies"),
        claude_home: td.join("home"),
        // No formula seed dir by default — the library upgrade seeds
        // the builtins; individual tests opt into the belt-and-braces
        // copy.
        formulas_seed_dir: None,
    }
}

/// Assert every artifact the 8-step script produced exists for `noyau`.
fn assert_noyau_materialized(galaxies_root: &Path, noyau: &str) {
    let root = galaxies_root.join(noyau);
    // Step 2 — state subtree (the script's three dirs).
    for sub in ["events", "molecules", "fleets/default"] {
        assert!(
            root.join(".cosmon/state").join(sub).is_dir(),
            "noyau {noyau}: missing .cosmon/state/{sub}",
        );
    }
    // Step 2a — the library upgrade pass produced config.toml with a
    // real generated project_id, plus the artifacts only the real
    // `cs init --upgrade` writes: canonical formulas, the neurion
    // registry, the gitleaks baseline. A stub could fake config.toml;
    // it could not fake these.
    let config = root.join(".cosmon/config.toml");
    assert!(
        config.is_file(),
        "noyau {noyau}: init --upgrade did not produce config.toml",
    );
    let body = std::fs::read_to_string(&config).unwrap();
    assert!(
        body.contains("project_id = "),
        "noyau {noyau}: config.toml carries no project_id:\n{body}",
    );
    assert!(
        root.join(".cosmon/formulas/task-work.formula.toml")
            .is_file(),
        "noyau {noyau}: canonical formulas were not backfilled",
    );
    assert!(
        root.join(".cosmon/registry.sqlite").is_file(),
        "noyau {noyau}: registry.sqlite was not seeded",
    );
    assert!(
        root.join(".gitleaks.toml").is_file(),
        "noyau {noyau}: gitleaks baseline was not written",
    );
    // Step 2b — git repo with at least the initial commit.
    assert!(root.join(".git").is_dir(), "noyau {noyau}: missing .git");
}

#[test]
fn single_noyau_materializes_all_steps() {
    // Equivalence: one noyau, the exact tenant-demo V1 case. Every step the
    // shell ENTRYPOINT performed must land.
    let td = tempfile::tempdir().unwrap();
    let init = image_init_for(td.path());

    let report = init.run(&[Noyau::new("tenant-demo-sandbox")]);

    assert!(report.all_ok(), "report had a failed step: {report:?}");

    // Step 1 — whispers/inbox (instance-level).
    assert!(td.path().join("whispers/inbox").is_dir());

    // Steps 2/2a/2b — per noyau.
    assert_noyau_materialized(&td.path().join("galaxies"), "tenant-demo-sandbox");

    // Steps 3a/3b — Claude Code gates in the worker $HOME.
    let claude_json = td.path().join("home/.claude.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&claude_json).unwrap()).unwrap();
    assert_eq!(v["hasCompletedOnboarding"], Value::Bool(true));

    let settings = td.path().join("home/.claude/settings.json");
    let s: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(s["skipDangerousModePermissionPrompt"], Value::Bool(true));
}

#[test]
fn rerun_is_idempotent() {
    // B2 eager runs on every restart — a second pass must be a no-op:
    // every step `AlreadyPresent`, config.toml untouched, exactly one
    // git commit, no churn.
    let td = tempfile::tempdir().unwrap();
    let init = image_init_for(td.path());
    let noyaux = [Noyau::new("tenant-demo-sandbox")];

    let first = init.run(&noyaux);
    assert!(first.all_ok());

    // Mutate config.toml so we can detect a clobbering re-init.
    let config = td
        .path()
        .join("galaxies/tenant-demo-sandbox/.cosmon/config.toml");
    let sentinel = "[project]\nproject_id = \"sentinel-do-not-clobber\"\n";
    std::fs::write(&config, sentinel).unwrap();

    let second = init.run(&noyaux);
    assert!(second.all_ok(), "second run failed: {second:?}");

    // Every per-noyau step idempotent (no re-creation, no re-init).
    let n = &second.noyaux[0];
    assert_eq!(n.state_dirs, StepOutcome::AlreadyPresent);
    assert_eq!(n.cs_init, StepOutcome::AlreadyPresent);
    assert_eq!(n.git_init, StepOutcome::AlreadyPresent);
    assert_eq!(second.inbox, StepOutcome::AlreadyPresent);
    assert_eq!(second.claude_onboarding, StepOutcome::AlreadyPresent);
    assert_eq!(second.claude_skip_dangerous, StepOutcome::AlreadyPresent);

    // config.toml was NOT clobbered — step 2a was correctly skipped.
    assert_eq!(std::fs::read_to_string(&config).unwrap(), sentinel);
}

#[test]
fn multi_noyau_materializes_each_independently() {
    // The whole point of Phase 1: more than one convive. Two noyaux,
    // two independent galaxy trees, each fully materialised.
    let td = tempfile::tempdir().unwrap();
    let init = image_init_for(td.path());

    let report = init.run(&[Noyau::new("tenant-demo-sandbox"), Noyau::new("democorp")]);
    assert!(report.all_ok(), "report had a failed step: {report:?}");
    assert_eq!(report.noyaux.len(), 2);

    let galaxies = td.path().join("galaxies");
    assert_noyau_materialized(&galaxies, "tenant-demo-sandbox");
    assert_noyau_materialized(&galaxies, "democorp");

    // The two trees are genuinely separate repos (separate .git).
    assert_ne!(
        galaxies.join("tenant-demo-sandbox/.git"),
        galaxies.join("democorp/.git"),
    );
    assert!(galaxies.join("tenant-demo-sandbox/.git").is_dir());
    assert!(galaxies.join("democorp/.git").is_dir());
}

#[test]
fn no_noyaux_still_materializes_instance_level() {
    // Empty HabilitationMap (a freshly-provisioned instance with no binding
    // yet) must not panic — instance-level steps still run, the
    // per-noyau loop is simply empty. Non-regression: the adapter boots.
    let td = tempfile::tempdir().unwrap();
    let init = image_init_for(td.path());

    let report = init.run(&[]);
    assert!(report.all_ok());
    assert!(report.noyaux.is_empty());
    assert!(td.path().join("whispers/inbox").is_dir());
    assert!(td.path().join("home/.claude.json").is_file());
}

#[test]
fn materializes_with_no_cs_binary_anywhere() {
    // The falsifier for issue #54 U2. The shipped container image
    // carries no `cs` binary, so step 2a must not need one — and since
    // U6 `ImageInit` has no `cs` path field at all. The run must still
    // materialise a complete galaxy.
    //
    // Stub the function this test covers and it goes red: without
    // `upgrade_project`, no `config.toml`, no formulas, no registry.
    let td = tempfile::tempdir().unwrap();
    let init = image_init_for(td.path());

    let report = init.run(&[Noyau::new("tenant-demo-sandbox")]);

    assert!(report.all_ok(), "report had a failed step: {report:?}");
    assert_eq!(report.noyaux[0].cs_init, StepOutcome::Done);
    assert_noyau_materialized(&td.path().join("galaxies"), "tenant-demo-sandbox");
}
