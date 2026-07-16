// SPDX-License-Identifier: AGPL-3.0-only

//! Pre/post-fusion help-surface diff pin.
//!
//! The fusion gate is « golden `--help` byte-identical pre/post » with
//! every divergence a CONSCIOUS, argued change. 28 of the 35
//! pre-fusion goldens are reproduced byte-identical by the canon
//! projection (`help_goldens.rs` enforces them). The remaining
//! deltas are enumerated HERE, exactly, against the preserved
//! `*.pre-fusion.help.txt` snapshots:
//!
//! 1. `root` — ONE line added (the `avatar` subcommand drained from
//!    cs-thin so the delivered binary covers all 13 tenant verbs).
//!    Additive ⇒ minor, per CHANGELOG 0.2.0.
//! 2. `molecule` / `molecule thaw` — the thaw description advertised
//!    `POST …/thaw`, a route the adapter removed in v1.0.0-rc (410
//!    Gone; the pre-fusion command was factually broken). The text now
//!    names the fused freeze route it actually dials.
//! 3. `artifact` family — placeholder names aligned with the canon
//!    (`{mol_id}`→`{id}`, `{name}`→`{token}`). Description text only.
//!
//! A3 (man + doc-gen parity, CHANGELOG 0.3.0) blesses two more:
//!
//! 4. `molecule` — the tackle line gains the ` [coûteux]` marker,
//!    DERIVED from the route's `cosmon:worker:spawn` scope;
//!    never hand prose, so it is pinned as a substitution here.
//! 5. `root` — `after_long_help` is attached (TYPICAL WORKFLOW /
//!    AUTHENTICATION / EFFECT MARKERS / FORMULAS / EXIT CODES drained
//!    from the former cs-thin `help.rs` into the clap tree), which
//!    flips clap's root `--help` to long-form rendering:
//!    option lines reflow and narrative sections append. The
//!    `Commands:` catalogue — the contract surface — is still checked
//!    line-by-line: nothing lost, the avatar line the only addition.
//!
//! Anything else differing from its pre-fusion snapshot fails this
//! test: usage lines, argument lists and option blocks must be
//! byte-identical everywhere.

use std::path::PathBuf;

fn goldens_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
}

fn lines_of(name: &str) -> Vec<String> {
    let path = goldens_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("missing golden {}: {e}", path.display()));
    text.lines().map(str::to_owned).collect()
}

/// Assert `pre` and `post` are line-identical except for the listed
/// `(pre_line, post_line)` substitutions, applied in order of
/// occurrence.
fn assert_diff_is_exactly(file: &str, substitutions: &[(&str, &str)]) {
    let pre = lines_of(&format!("{file}.pre-fusion.help.txt"));
    let post = lines_of(&format!("{file}.help.txt"));
    assert_eq!(
        pre.len(),
        post.len(),
        "{file}: line count changed — only in-place description edits are blessed"
    );
    let mut expected: Vec<(&str, &str)> = substitutions.to_vec();
    for (i, (a, b)) in pre.iter().zip(post.iter()).enumerate() {
        if a == b {
            continue;
        }
        let Some(pos) = expected.iter().position(|(old, new)| a == old && b == new) else {
            panic!(
                "{file}:{}: unexpected diff —\n  pre : {a}\n  post: {b}",
                i + 1
            );
        };
        expected.remove(pos);
    }
    assert!(
        expected.is_empty(),
        "{file}: blessed substitutions not found in the diff: {expected:?}"
    );
}

/// The four diagnostic verbs gained an explicit `(diagnostic)` marker
/// in their about text (B2+C1 integration, stitch 828e) — in-place
/// description edits, blessed here as (pre, post) pairs.
const DIAGNOSTIC_SUBS: &[(&str, &str)] = &[
    (
        "  healthz   Adapter liveness probe",
        "  healthz   Adapter liveness probe (diagnostic)",
    ),
    // The `post` strings also drop the private molecule-IDs and the
    // internal `gap report ae3d` locators (cold-legibility for the
    // public tree); the pre-fusion `old` strings preserve them.
    (
        "  quota     `GET /v1/quota` \u{2014} read the current rate-limit snapshot (`task-20260522-2f91`, gap report ae3d \u{a7}h). Table by default, JSON with `--json`",
        "  quota     `GET /v1/quota` \u{2014} read the current rate-limit snapshot. Table by default, JSON with `--json`. (diagnostic)",
    ),
    (
        "  workers   Worker observability (`GET /v1/workers`, `task-20260523-f82b`, gap report ae3d \u{a7}e priority 5)",
        "  workers   Worker observability (`GET /v1/workers`). (diagnostic)",
    ),
    (
        "  noyaux    `GET /v1/noyaux` \u{2014} discovery endpoint for multi-noyau operators (`task-20260523-eb61`, gap report ae3d \u{a7}f)",
        "  noyaux    `GET /v1/noyaux` \u{2014} discovery endpoint for multi-noyau operators. (diagnostic)",
    ),
];

/// Cold-legibility edits (publication prep): private molecule-IDs and
/// internal locators dropped from user-facing `--help` text. `events`
/// lost its `task-…` citation; `artifact`'s about lost its internal
/// `(smithy e653)` tag. In-place description edits, blessed here as
/// (pre, post) pairs — the pre-fusion snapshots preserve the old text.
const LEGIBILITY_SUBS: &[(&str, &str)] = &[
    (
        "  events    Server-Sent Events stream of molecule lifecycle events (`GET /v1/events`, task-20260522-c46a)",
        "  events    Server-Sent Events stream of molecule lifecycle events (`GET /v1/events`)",
    ),
    (
        "  artifact  Artifact endpoints (smithy e653)",
        "  artifact  Artifact endpoints",
    ),
];

const THAW_PRE: &str = "  thaw      `POST /v1/molecules/{id}/thaw`";
const THAW_POST: &str = "  thaw      `POST /v1/molecules/{id}/freeze` with `state: \"active\"` — resume a frozen molecule (the legacy `/thaw` route is 410 Gone)";

/// The `Commands:` block of a rendered help page — the catalogue of
/// verbs, i.e. the contract surface scripts and tenants key on.
fn commands_section(lines: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_commands = false;
    for line in lines {
        if line.trim_end() == "Commands:" {
            in_commands = true;
            continue;
        }
        if in_commands {
            if line.trim().is_empty() {
                break;
            }
            out.push(line.clone());
        }
    }
    out
}

/// Root: the `Commands:` catalogue is the contract surface scripts
/// and tenants key on. The rest of the page is consciously long-form
/// since A3 (`after_long_help` attached — see module docs), so only
/// the catalogue is compared line-by-line.
#[test]
fn root_commands_catalogue_gains_the_seven_blessed_verbs() {
    // Catalogue-scoped diff (the rest of the page is consciously
    // long-form since A3 \u{2014} `after_long_help` attached, see module
    // docs). Seven blessed verb additions, all additive (\u{21d2} minor):
    // 1. `avatar`   \u{2014} A2 fusion (drained from cs-thin), CHANGELOG 0.2.0.
    // 2. `do`       \u{2014} B2 client-side composition (task-20260610-56c4).
    // 3. `doctor`   \u{2014} C1 onboarding checks (stitch 828e).
    // 4. `converse` \u{2014} canal (b) as a TOP-LEVEL verb, deliberately
    //    LAST in the list (off the golden path; never an `avatar`
    //    subcommand \u{2014} guide \u{a7}12.2, task-20260610-0b57).
    // 5. `run`      \u{2014} GATE Q1 `do` + attributed cost delta, a
    //    client-side quota bracket; zero new routes (task-20260625-ba34).
    // 6. `login`    \u{2014} real OAuth2-PKCE user\u{2194}cosmon sign-in vs Forgejo
    //    (delib-20260710-33b7 C2/C7, task-20260710-2565); distinct from
    //    `auth login` (the Claude device flow).
    // 7. `logout`   \u{2014} forget the persisted credential; reverse of `login`.
    // Plus four in-place description edits: the diagnostic verbs
    // (healthz, quota, workers, noyaux) gained an explicit
    // `(diagnostic)` marker in the B2+C1 integration (stitch 828e) \u{2014}
    // text-only, the routes are untouched.
    let pre = commands_section(&lines_of("root.pre-fusion.help.txt"));
    let post = commands_section(&lines_of("root.help.txt"));
    let added: Vec<&String> = post.iter().filter(|l| !pre.contains(l)).collect();
    let removed: Vec<&String> = pre.iter().filter(|l| !post.contains(l)).collect();
    // In-place description substitutions: the four `(diagnostic)`
    // markers plus the two cold-legibility edits (events, artifact).
    let inplace_subs: Vec<(&str, &str)> = DIAGNOSTIC_SUBS
        .iter()
        .chain(LEGIBILITY_SUBS)
        .copied()
        .collect();
    assert_eq!(
        removed.len(),
        inplace_subs.len(),
        "root commands: the only blessed removals are the in-place description edits, got {removed:?}"
    );
    for (old_line, new_line) in &inplace_subs {
        assert!(
            removed.iter().any(|l| l.as_str() == *old_line),
            "expected the pre-fusion line to be substituted: {old_line:?}, removed = {removed:?}"
        );
        assert!(
            added.iter().any(|l| l.as_str() == *new_line),
            "missing the blessed description edit: {new_line:?}, added = {added:?}"
        );
    }
    assert_eq!(
        added.len(),
        7 + inplace_subs.len(),
        "root commands: blessed additions are avatar, do, doctor, converse, run, login, logout + the in-place edits, got {added:?}"
    );
    for verb in [
        "  avatar  ",
        "  do  ",
        "  doctor  ",
        "  converse  ",
        "  run  ",
        "  login  ",
        "  logout  ",
    ] {
        assert!(
            added.iter().any(|l| l.starts_with(verb)),
            "missing the blessed `{}` subcommand line, got {added:?}",
            verb.trim()
        );
    }
    let last_command = post
        .iter()
        .rfind(|l| !l.trim_start().starts_with("help"))
        .expect("root help has a Commands: section");
    assert!(
        last_command.trim_start().starts_with("converse"),
        "converse must stay LAST in the command list (off the golden path), got {last_command:?}"
    );
}

const TACKLE_PRE: &str = "  tackle    `POST /v1/molecules/{id}/tackle`";
const TACKLE_POST: &str = "  tackle    `POST /v1/molecules/{id}/tackle` [co\u{fb}teux]";

#[test]
fn molecule_diff_is_the_thaw_and_tackle_corrections_plus_the_run_line() {
    // In-place substitutions: the thaw about correction (A2, blessed
    // in CHANGELOG 0.2.0) and the scope-derived [co\u{fb}teux] marker on
    // tackle (A3, task-20260610-10d2). One blessed addition: the
    // `run` verb (B2 bounded drain, task-20260610-56c4).
    let pre = lines_of("molecule.pre-fusion.help.txt");
    let post = lines_of("molecule.help.txt");
    let added: Vec<&String> = post.iter().filter(|l| !pre.contains(l)).collect();
    let removed: Vec<&String> = pre.iter().filter(|l| !post.contains(l)).collect();
    assert_eq!(
        removed.len(),
        2,
        "blessed removals: the pre-fusion thaw + tackle lines, got {removed:?}"
    );
    for old_line in [THAW_PRE, TACKLE_PRE] {
        assert!(
            removed.iter().any(|l| l.as_str() == old_line),
            "expected the pre-fusion line to be substituted: {old_line:?}, removed = {removed:?}"
        );
    }
    assert_eq!(
        added.len(),
        3,
        "blessed: thaw correction + tackle marker + run line, got {added:?}"
    );
    assert!(
        added.iter().any(|l| l.as_str() == THAW_POST),
        "missing the blessed thaw correction, got {added:?}"
    );
    assert!(
        added.iter().any(|l| l.as_str() == TACKLE_POST),
        "missing the blessed tackle [co\u{fb}teux] marker, got {added:?}"
    );
    assert!(
        added
            .iter()
            .any(|l| l.starts_with("  run       `POST /v1/molecules/{id}/run`")),
        "missing the blessed run verb line, got {added:?}"
    );
}

/// No operator-only verb may appear on
/// the client surface. Scans the blessed root + molecule help goldens
/// for the ADR-080 \u{a7}5.1 tokens as command names \u{2014} `do` is NOT `done`,
/// hence the exact-token match.
#[test]
fn no_operator_only_verb_on_the_client_surface() {
    let forbidden = [
        "done",
        "evolve",
        "complete",
        "stitch",
        "kill",
        "purge",
        "reconcile",
        "verify",
        "whisper",
        "drop",
        "security",
    ];
    for page in ["root", "molecule"] {
        for line in lines_of(&format!("{page}.help.txt")) {
            let Some(first) = line.split_whitespace().next() else {
                continue;
            };
            assert!(
                !forbidden.contains(&first),
                "operator-only verb `{first}` leaked into the {page} help surface: {line:?}",
            );
        }
    }
}

#[test]
fn molecule_thaw_diff_is_the_about_correction() {
    assert_diff_is_exactly(
        "molecule_thaw",
        &[(
            "`POST /v1/molecules/{id}/thaw`",
            "`POST /v1/molecules/{id}/freeze` with `state: \"active\"` — resume a frozen \
             molecule (the legacy `/thaw` route is 410 Gone)",
        )],
    );
}

#[test]
fn artifact_diffs_are_the_canon_placeholder_names() {
    assert_diff_is_exactly(
        "artifact",
        &[
            // Cold-legibility: the about lost its internal `(smithy
            // e653)` tag (publication prep).
            ("Artifact endpoints (smithy e653)", "Artifact endpoints"),
            (
                "  list  `GET /v1/molecules/{mol_id}/artifacts`",
                "  list  `GET /v1/molecules/{id}/artifacts`",
            ),
            (
                "  get   `GET /v1/molecules/{mol_id}/artifacts/{token}`",
                "  get   `GET /v1/molecules/{id}/artifacts/{token}`",
            ),
            (
                "  push  `PUT /v1/molecules/{mol_id}/artifacts/{name}`",
                "  push  `PUT /v1/molecules/{id}/artifacts/{token}`",
            ),
        ],
    );
    assert_diff_is_exactly(
        "artifact_list",
        &[(
            "`GET /v1/molecules/{mol_id}/artifacts`",
            "`GET /v1/molecules/{id}/artifacts`",
        )],
    );
    // `artifact_get` diverges further since replay-Dave D2
    // (task-20260610-828e, merged via task-20260611-7c7f): the
    // positional token's clap id collided with the global `--token`
    // (JWT override), so the positional value was sent as the bearer.
    // The fix gives the positional its own id, which (a) documents it
    // in the Arguments block and (b) un-masks the global `--token`
    // row the collision was swallowing. Three blessed divergences.
    {
        let pre = lines_of("artifact_get.pre-fusion.help.txt");
        let post = lines_of("artifact_get.help.txt");
        let added: Vec<&String> = post.iter().filter(|l| !pre.contains(l)).collect();
        let removed: Vec<&String> = pre.iter().filter(|l| !post.contains(l)).collect();
        assert_eq!(
            removed.len(),
            2,
            "artifact_get: blessed removals are the pre-canon usage line + the bare <TOKEN> row, got {removed:?}"
        );
        assert!(
            removed
                .iter()
                .any(|l| l.as_str() == "`GET /v1/molecules/{mol_id}/artifacts/{token}`"),
            "missing the pre-canon usage line in removals, got {removed:?}"
        );
        assert!(
            removed.iter().any(|l| l.trim() == "<TOKEN>"),
            "missing the bare (undocumented) <TOKEN> row in removals, got {removed:?}"
        );
        assert_eq!(
            added.len(),
            3,
            "artifact_get: blessed additions are the canon usage line, the documented <TOKEN> row and the un-masked global --token row, got {added:?}"
        );
        assert!(
            added
                .iter()
                .any(|l| l.as_str() == "`GET /v1/molecules/{id}/artifacts/{token}`"),
            "missing the canon usage line, got {added:?}"
        );
        assert!(
            added
                .iter()
                .any(|l| l.starts_with("  <TOKEN>   Artifact token")),
            "missing the documented <TOKEN> row (D2), got {added:?}"
        );
        assert!(
            added
                .iter()
                .any(|l| l.trim_start().starts_with("--token <TOKEN>")),
            "missing the un-masked global --token row (D2), got {added:?}"
        );
    }
    assert_diff_is_exactly(
        "artifact_push",
        &[(
            "`PUT /v1/molecules/{mol_id}/artifacts/{name}`",
            "`PUT /v1/molecules/{id}/artifacts/{token}`",
        )],
    );
}

/// Every OTHER pre-fusion snapshot must be byte-identical to its live
/// golden — the heart of the « we do not break userspace » gate.
#[test]
fn all_other_pre_fusion_snapshots_are_byte_identical() {
    let consciously_changed = [
        "root",
        "molecule",
        "molecule_thaw",
        "artifact",
        "artifact_list",
        "artifact_get",
        "artifact_push",
    ];
    let dir = goldens_dir();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("goldens dir") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".pre-fusion.help.txt") else {
            continue;
        };
        if consciously_changed.contains(&stem) {
            continue;
        }
        let pre = std::fs::read(dir.join(&name)).expect("read pre");
        let post = std::fs::read(dir.join(format!("{stem}.help.txt"))).expect("read post");
        assert_eq!(
            pre, post,
            "{stem}: pre-fusion snapshot differs from live golden without a blessed entry"
        );
        checked += 1;
    }
    // The 7 conscious changes carry pre-fusion snapshots; others may
    // or may not. This test is meaningful as soon as one exists.
    let _ = checked;
}
