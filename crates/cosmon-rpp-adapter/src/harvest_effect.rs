// SPDX-License-Identifier: AGPL-3.0-only

//! The effect half of the harvest door — issue #51, ADR-176 §11.
//!
//! # What this module is for
//!
//! The door has a decision half and an effect half. The decision half is a
//! library ([`cosmon_filestore::harvest_door::decide`]) and answers every
//! pre-effect refusal in-process. The effect half is the sealed harvest
//! transaction — merge with lineage trailers, publish/identity/
//! confidentiality gates, the `pre_done` gate, the teardown — and it has
//! exactly one implementation in this repository: `cmd/done.rs`. Rewriting
//! it here would fork the door, which is the failure the shared
//! [`DoorRefusal`] vocabulary exists
//! to prevent.
//!
//! So the effect is a **port**, and the deployment chooses an
//! implementation:
//!
//! - [`UnavailableHarvestEffect`] — the default and the honest answer for an
//!   image that carries no `cs`. The route refuses `harvest_effect_unavailable`
//!   rather than pretending, and every pre-effect refusal still answers in
//!   full.
//! - [`CsBinaryHarvestEffect`] — the operator declared a `cs` binary in
//!   `rpp.toml`. The harvest runs as that binary, with the argv
//!   [`HarvestOptions::cs_done_argv`] builds, in the tenant's own galaxy
//!   root.
//!
//! # Why a `cs` child is legitimate here, and was not before
//!
//! ADR-080 §5.1 classed `done` as an operator-only verb, and §3.5 gave that
//! two locks: the adapter refuses to route to one, and `cs` refuses to run
//! one under `COSMON_API_REQUEST=1`. A `cs done` child of a request was
//! therefore refused by construction, which is why the harvest could not
//! reach its effect at all.
//!
//! The §5.1 amendment of issue #51 takes `done` off that list — closing a
//! molecule is lifecycle, not administration — by the same §5.2 successor
//! path `run` used in ADR-124. With `done` off the list, a child that
//! performs it is an ordinary local gesture, and it announces itself as one:
//! [`cosmon_core::api_envelope::hand_off_to_local_child`] consumes the
//! request marker, exactly as the resident drain's own `cs done` teardown
//! already does. **No security posture is consumed**: the egress variables
//! and the exposed-host refusal are carried across on purpose.
//!
//! What the child does *not* get is the server's environment. It is built
//! from a private allow-list (`INHERITED_ENV`), the security posture, and
//! the tenant's own paths — see `CsBinaryHarvestEffect::build_child_env`
//! and those constants for the two inherited variables that were enough
//! to waive a gate the request had asked for and to point the harvest at
//! another galaxy.
//!
//! What this deliberately does *not* restore is the general §3.5 clause (e)
//! subprocess envelope retired by issue #54 U6. This is one port, one verb,
//! one operator-declared binary, off by default.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmon_core::harvest_door::{DoorRefusal, HarvestOptions};
use cosmon_core::id::MoleculeId;

/// The one effect-error vocabulary, shared with the door's own seam.
///
/// Re-exported rather than redefined. Two traits — this crate's
/// [`HarvestEffectPort`] and the door's
/// [`SealedHarvestEffect`](cosmon_filestore::harvest_door::SealedHarvestEffect)
/// — are useful because they answer to different owners, but two *error*
/// types were not: the bridge between them flattened this one to a
/// `String`, and a refusal the effect named at its authority boundary
/// reached the wire as an anonymous failure. That was the PR #62 review's
/// third finding, and the repair is that there is now nothing to flatten.
pub use cosmon_core::harvest_door::EffectFailure;

/// The environment a harvest child is given, beyond what this module sets
/// explicitly.
///
/// # Why an allow-list and not the server's own environment
///
/// The child inherited everything, and two inherited variables were enough
/// to break the door. `COSMON_SKIP_PRE_DONE_HOOK` waives the blocking
/// `pre_done` gate for any non-empty value, so an operator who exported it
/// once in the shell that started the server waived that gate for every
/// request that explicitly asked for it to run. `COSMON_STATE_DIR` and
/// `COSMON_CONFIG` win over walk-up discovery in
/// [`cosmon_filestore::resolve`], so the child could read and mutate a
/// different galaxy than the one the decision half had just checked — the
/// admitted molecule and the harvested molecule need not be the same
/// molecule. This is the failure class ADR-080 §3.5.1 named and the issue
/// #57 envelope closed; re-adding a `cs` child re-opened it.
///
/// So the child's environment is **built**, not inherited: this list, plus
/// the tenant's own paths, plus the security posture, and nothing else.
/// Each entry is here because a child that lacked it would misbehave
/// rather than merely differ:
///
/// - `PATH` — `cs done` runs `git`, and the galaxy's hooks run whatever
///   they run. A child with no `PATH` cannot merge.
/// - `HOME` — the git configuration the merge commit's identity comes
///   from, and the `~/.cosmon` fallback every resolver ends at.
/// - `TMPDIR` — where git writes its temporary objects and edit files.
/// - `TZ` and the four locale variables — timestamps and message
///   collation. A harvest that stamped a different timezone than the
///   server's other records would be a reader's problem forever.
/// - `USER`/`LOGNAME` — git falls back to them when no `user.name` is
///   configured.
///
/// Deliberately absent: `SSH_AUTH_SOCK` and the git credential
/// environment. `cs done` merges locally and does not push; a child that
/// could authenticate to a remote would hold an authority the door never
/// granted it.
///
/// This is **not** `scripts/no-pilot-env.sh`. That script is a gate
/// mechanism — it strips a worker's *pilotage* variables so a test suite
/// does not read its parent's instructions as configuration — and it has
/// no business on a runtime path, where stripping `COSMON_EGRESS_POLICY`
/// would weaken a real jail. This list is the opposite construction: it
/// says what may pass, and the posture below is passed on purpose.
const INHERITED_ENV: &[&str] = &[
    "PATH", "HOME", "TMPDIR", "TZ", "LANG", "LC_ALL", "LC_CTYPE", "LC_TIME", "USER", "LOGNAME",
];

/// The security posture the child inherits **because** it must.
///
/// [`cosmon_core::api_envelope::hand_off_to_local_child`] documents the
/// rule these obey: a hand-off consumes the request's correlation markers
/// and no security posture, so a child of an exposed dispatch stays as
/// confined as its parent. Clearing the environment must not
/// become the loophole that widens what the envelope narrowed, so these
/// are carried across explicitly.
const INHERITED_POSTURE: &[&str] = &[
    cosmon_core::egress::EgressPolicy::ENV_VAR,
    cosmon_core::egress::REQUIRE_NETNS_ENV,
    cosmon_core::egress::EXPOSED_MULTITENANT_ENV,
];

/// The effect half of the door, as a port.
///
/// `Send + Sync` because the implementation is held in
/// [`crate::AppState`] and shared across every request thread.
pub trait HarvestEffectPort: Send + Sync + std::fmt::Debug {
    /// Perform the sealed harvest of `molecule` in `tenant_root`, with the
    /// requester's options.
    ///
    /// # Errors
    ///
    /// [`EffectFailure`] — see its variants. An implementation that cannot
    /// run at all returns [`EffectFailure::Unavailable`], which the route
    /// maps to `501`, never to a success.
    fn harvest(
        &self,
        tenant_root: &Path,
        molecule: &MoleculeId,
        options: &HarvestOptions,
    ) -> Result<(), EffectFailure>;

    /// Whether this effect acquires the trunk flock at its own effect
    /// boundary.
    ///
    /// `true` for the `cs done` transaction in both of its shapes — it
    /// flocks before its first git mutation (ADR-172 D3) — and the door
    /// then must not hold the lock across the call, because `flock(2)` does
    /// not nest and holding it would deadlock rather than serialize.
    fn binds_trunk_lock(&self) -> bool;
}

/// The default: no effect implementation in this deployment.
///
/// Answers [`EffectFailure::Unavailable`] for every harvest the
/// decision half admits. Fail-honest rather than fail-open: the alternative
/// an adapter reaches for under pressure is a `202`, and a `202` on a
/// transaction that may integrate nothing is exactly the defect issue #51
/// reported.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableHarvestEffect;

impl HarvestEffectPort for UnavailableHarvestEffect {
    fn harvest(
        &self,
        _tenant_root: &Path,
        _molecule: &MoleculeId,
        _options: &HarvestOptions,
    ) -> Result<(), EffectFailure> {
        Err(EffectFailure::Unavailable)
    }

    fn binds_trunk_lock(&self) -> bool {
        // Nothing runs, so nothing is serialized; the answer keeps the
        // door from taking a lock it would only release again.
        true
    }
}

/// The harvest as the operator's own `cs` binary, run in the tenant's
/// galaxy root.
///
/// The binary is **operator-declared** (`harvest_cs_binary` in `rpp.toml`)
/// and there is no PATH fallback. A door that discovered its own executor
/// would change behaviour when someone else's `cs` appeared on the host's
/// PATH, which is a deployment fact no operator reviewed.
#[derive(Debug, Clone)]
pub struct CsBinaryHarvestEffect {
    /// Absolute path to the `cs` binary the operator declared.
    binary: PathBuf,
}

impl CsBinaryHarvestEffect {
    /// Wire the effect to an operator-declared `cs` binary.
    #[must_use]
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}

impl CsBinaryHarvestEffect {
    /// Build the child's environment from nothing.
    ///
    /// Three sources, in this order, and no fourth: the [`INHERITED_ENV`]
    /// allow-list, the [`INHERITED_POSTURE`] the envelope requires, and
    /// the tenant's own two paths. The tenant paths are set **last** and
    /// unconditionally, so they cannot be shadowed by an allow-listed
    /// value, and they are the same two the decision half read
    /// (`<tenant_root>/.cosmon/state` and `.../config.toml`) — which is
    /// the property that makes "the molecule the door admitted" and "the
    /// molecule the effect harvests" the same molecule.
    fn build_child_env(cmd: &mut Command, tenant_root: &Path) {
        cmd.env_clear();
        for name in INHERITED_ENV.iter().chain(INHERITED_POSTURE) {
            if let Some(value) = std::env::var_os(name) {
                cmd.env(name, value);
            }
        }
        let cosmon_dir = tenant_root.join(".cosmon");
        cmd.env("COSMON_STATE_DIR", cosmon_dir.join("state"));
        cmd.env("COSMON_CONFIG", cosmon_dir.join("config.toml"));
        // Vacuous after `env_clear`, and kept anyway: it is the statement
        // that this child is a local gesture of the tenant's own
        // machinery rather than the request, and it stays correct if the
        // allow-list above ever grows a marker.
        cosmon_core::api_envelope::hand_off_to_local_child(cmd);
    }
}

impl HarvestEffectPort for CsBinaryHarvestEffect {
    fn harvest(
        &self,
        tenant_root: &Path,
        molecule: &MoleculeId,
        options: &HarvestOptions,
    ) -> Result<(), EffectFailure> {
        let mut cmd = Command::new(&self.binary);
        cmd.args(options.cs_done_argv(molecule.as_str()))
            .current_dir(tenant_root);
        Self::build_child_env(&mut cmd, tenant_root);

        let output = cmd
            .output()
            .map_err(|e| EffectFailure::Failed(format!("spawning `cs done` failed: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        match output.status.code().and_then(DoorRefusal::from_exit_code) {
            Some(refusal) => Err(EffectFailure::Refused(refusal)),
            None => Err(EffectFailure::Failed(format!(
                "`cs done` exited with {}",
                output
                    .status
                    .code()
                    .map_or_else(|| "a signal".to_owned(), |c| c.to_string())
            ))),
        }
    }

    fn binds_trunk_lock(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_effect_refuses_instead_of_pretending() {
        let err = UnavailableHarvestEffect
            .harvest(
                Path::new("/nonexistent"),
                &MoleculeId::new("task-20260101-abcd").unwrap(),
                &HarvestOptions::new("close it"),
            )
            .unwrap_err();
        assert!(matches!(err, EffectFailure::Unavailable));
    }

    /// Write an executable stub `cs` that appends its own argv to
    /// `log`, one argument per line, and exits zero.
    ///
    /// A stub rather than a mock object because the claim under test is
    /// about a **child process**: what `Command` was actually built with,
    /// including `current_dir`, the argv the operator's binary would see,
    /// and the environment it would read its configuration out of. An
    /// in-process assertion on `cs_done_argv` cannot fail if `harvest`
    /// stops passing it, and no in-process assertion at all can observe an
    /// inherited variable — only the child can.
    ///
    /// `env` receives one `NAME=value` line per variable the child was
    /// actually given. The whole environment, not a selection: a test that
    /// only asked about the names it expected could not fail on the one
    /// that leaked. The assertions below report **names**, never values —
    /// the environment this test deliberately fails against is the
    /// developer's own, and a red test that printed it would put every key
    /// in that shell into a CI log.
    #[cfg(unix)]
    fn stub_cs(dir: &Path, log: &Path, env: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let script = dir.join("cs");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\npwd >> '{}'\nenv > '{}'\nexit 0\n",
                log.display(),
                log.display(),
                env.display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        script
    }

    /// Serializes the tests that must poison the **parent** process
    /// environment to make their claim. `std::env::set_var` is
    /// process-wide, and two of these running concurrently would read each
    /// other's poison.
    #[cfg(unix)]
    static ENV_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Read the child's recorded environment back as a map.
    #[cfg(unix)]
    fn child_env(path: &Path) -> std::collections::HashMap<String, String> {
        std::fs::read_to_string(path)
            .expect("the child must have run")
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    /// The falsifier for "the parameter reaches the effect": the argv the
    /// `cs` child is **spawned** with is the one the requester's options
    /// build, including a non-default strategy and the caller's own
    /// reason — read back from the child itself, not from the builder
    /// this side of the fork.
    #[cfg(unix)]
    #[test]
    fn the_cs_child_is_spawned_with_the_requested_options() {
        let tmp = tempfile::tempdir().unwrap();
        let tenant_root = tmp.path().join("galaxy");
        std::fs::create_dir_all(&tenant_root).unwrap();
        let log = tmp.path().join("argv.txt");
        let env_log = tmp.path().join("env.txt");
        let effect = CsBinaryHarvestEffect::new(stub_cs(tmp.path(), &log, &env_log));

        let mut options = HarvestOptions::new("the spike answered its question");
        options.strategy = cosmon_core::harvest_door::MergeStrategy::FfOnly;
        options.force = true;

        effect
            .harvest(
                &tenant_root,
                &MoleculeId::new("task-20260101-abcd").unwrap(),
                &options,
            )
            .expect("the stub child exits zero");

        let seen = std::fs::read_to_string(&log).expect("the child must have run");
        let argv: Vec<&str> = seen.lines().collect();
        assert_eq!(argv[0], "done");
        assert_eq!(argv[1], "task-20260101-abcd");
        let strategy_at = argv.iter().position(|a| *a == "--strategy").unwrap();
        assert_eq!(argv[strategy_at + 1], "ff-only");
        let reason_at = argv.iter().position(|a| *a == "--reason").unwrap();
        assert_eq!(argv[reason_at + 1], "the spike answered its question");
        assert!(argv.iter().any(|a| *a == "--force"));
        // The child also runs in the tenant's own galaxy root: a `cs done`
        // executed anywhere else would harvest a different kernel.
        let cwd = std::fs::canonicalize(argv.last().unwrap()).unwrap();
        assert_eq!(cwd, std::fs::canonicalize(&tenant_root).unwrap());
    }

    /// A child that exits with a door refusal code is reported as that
    /// **named** refusal, never as an anonymous failure — the mirror
    /// `DoorRefusal::from_exit_code` exists for.
    #[cfg(unix)]
    #[test]
    fn a_refusing_child_is_reported_as_its_named_refusal() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("cs");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nexit {}\n", DoorRefusal::BacklogFull.exit_code()),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let err = CsBinaryHarvestEffect::new(&script)
            .harvest(
                tmp.path(),
                &MoleculeId::new("task-20260101-abcd").unwrap(),
                &HarvestOptions::new("close it"),
            )
            .unwrap_err();
        match err {
            EffectFailure::Refused(r) => assert_eq!(r, DoorRefusal::BacklogFull),
            other => panic!("a door exit code must stay named, got {other:?}"),
        }
    }

    /// The falsifier for the environment boundary: what the parent
    /// exported does not decide what the child does.
    ///
    /// Two poisons, both real. `COSMON_SKIP_PRE_DONE_HOOK=1` in the
    /// server's own environment waived the galaxy's blocking `pre_done`
    /// gate for **every** harvest, including this one, whose options say
    /// `skip_pre_done_hook: false` — a request asking for the gate and
    /// getting it waived by a variable nobody in the request ever saw.
    /// `COSMON_STATE_DIR` pointing elsewhere sent the effect to a
    /// different galaxy than the decision half had just admitted the
    /// molecule from, so "the harvest the door authorised" and "the
    /// harvest that happened" need not be the same harvest.
    ///
    /// Remove `build_child_env`'s `env_clear` and this test goes red on
    /// the first assertion; remove the two explicit tenant paths and it
    /// goes red on the next two.
    #[cfg(unix)]
    #[test]
    fn the_child_reads_the_tenants_configuration_and_not_the_servers() {
        let _serial = ENV_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let tenant_root = tmp.path().join("galaxy");
        std::fs::create_dir_all(&tenant_root).unwrap();
        let log = tmp.path().join("argv.txt");
        let env_log = tmp.path().join("env.txt");
        let effect = CsBinaryHarvestEffect::new(stub_cs(tmp.path(), &log, &env_log));

        let elsewhere = tmp.path().join("someone-elses-galaxy");
        std::env::set_var("COSMON_SKIP_PRE_DONE_HOOK", "1");
        std::env::set_var("COSMON_STATE_DIR", elsewhere.join(".cosmon/state"));
        std::env::set_var("COSMON_CONFIG", elsewhere.join(".cosmon/config.toml"));
        std::env::set_var("COSMON_API_REQUEST", "1");
        std::env::set_var(cosmon_core::egress::EgressPolicy::ENV_VAR, "strict");

        let options = HarvestOptions::new("the request asked for the gate to run");
        assert!(
            !options.skip_pre_done_hook,
            "the premise of this test: the REQUEST does not waive the gate",
        );
        let result = effect.harvest(
            &tenant_root,
            &MoleculeId::new("task-20260101-abcd").unwrap(),
            &options,
        );

        for name in [
            "COSMON_SKIP_PRE_DONE_HOOK",
            "COSMON_STATE_DIR",
            "COSMON_CONFIG",
            "COSMON_API_REQUEST",
            cosmon_core::egress::EgressPolicy::ENV_VAR,
        ] {
            std::env::remove_var(name);
        }
        result.expect("the stub child exits zero");

        let seen = child_env(&env_log);
        assert!(
            !seen.contains_key("COSMON_SKIP_PRE_DONE_HOOK"),
            "an inherited kill-switch waives a gate this request asked to run; the \
             child was given {} variables",
            seen.len(),
        );
        assert!(
            !seen.contains_key("COSMON_API_REQUEST"),
            "the hand-off must leave the request marker behind",
        );
        let cosmon_dir = tenant_root.join(".cosmon");
        assert_eq!(
            seen.get("COSMON_STATE_DIR").map(String::as_str),
            Some(cosmon_dir.join("state").to_str().unwrap()),
            "the child must read the state the door checked, not the server's override",
        );
        assert_eq!(
            seen.get("COSMON_CONFIG").map(String::as_str),
            Some(cosmon_dir.join("config.toml").to_str().unwrap()),
        );
        assert_eq!(
            seen.get(cosmon_core::egress::EgressPolicy::ENV_VAR)
                .map(String::as_str),
            Some("strict"),
            "clearing the environment must not become the loophole that widens \
             the posture the envelope narrowed",
        );
        assert!(
            seen.contains_key("PATH"),
            "a child with no PATH cannot run `git` and cannot merge",
        );
    }

    #[test]
    fn a_bare_harvest_passes_no_strategy_and_gets_the_documented_default() {
        let argv = HarvestOptions::new("close it").cs_done_argv("task-20260101-abcd");
        assert!(
            !argv.iter().any(|a| a == "--strategy"),
            "a bare harvest must not pin a strategy; `cs done`'s own default is the contract"
        );
    }
}
