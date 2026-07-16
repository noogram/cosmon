// SPDX-License-Identifier: AGPL-3.0-only

//! `cs spark <text>` — one-line Inbox capture (ADR-061).
//!
//! A **spark** is the pre-task — the raw operator intent that appears in a
//! head and must land somewhere typed without ceremony. `cs spark "réunion
//! demain : revoir le pitch"` drops a `MoleculeKind::Idea` into the fleet's
//! `.cosmon/`, tagged `temp:hot`, with the sparker's identity attached.
//!
//! The demo criterion: a user on a phone via Blink Shell SSH → types one
//! line → the operator sees it at the top of the `temp:hot` pile on the
//! next `cs peek` refresh. No Claude Code in the chain, no Matrix, no
//! invitation ceremony.
//!
//! # Shape
//!
//! One verb, one argument, one line in the Inbox:
//!
//! ```text
//! cs spark "réunion demain : revoir le pitch"
//! ```
//!
//! The command is a thin wrapper over [`super::nucleate::run`]: it fills
//! a [`super::nucleate::Args`] with `formula = "spark"`, `kind = "idea"`,
//! `tag = temp:hot`, `--var topic=<text>`, and a derived `nucleon_id`.
//! The remaining nucleation machinery (prompt-seal, event emission,
//! symmetric link maintenance) is inherited unchanged.
//!
//! # `nucleon_id` derivation
//!
//! ADR-061 reserves a typed `nucleon_id` field on `MoleculeData` for after
//! the ADR is accepted. Until then, the sparker identity rides on the
//! molecule's `variables` map (key `nucleon_id`), written verbatim into
//! `prompt.md`'s frontmatter by the existing `write_prompt` path.
//!
//! Derivation order:
//! 1. Explicit `--nucleon <id>` — operator override.
//! 2. `git config user.email` — SSH-identity proxy.
//! 3. `$USER@$(hostname)` — last-resort fallback.
//!
//! # Hard sacrifices (documented per Jobs §3)
//!
//! - **Offline.** operator-demo without network fails. OK — she is on wifi.
//! - **Fine auth.** `nucleon_id` ≡ whoever has SSH access. No mTLS, no
//!   magic-link (Godin's anti-pattern list).
//! - **Latency.** No push. The pilot sees the line on the next refresh.
//! - **iPhone UX.** Raw Blink Shell terminal. No form.
//! - **Bidirectional.** Pilot does not reply from the Inbox in v1.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use super::Context;

/// Arguments for the `spark` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// The spark text itself — what appeared in the operator's head.
    ///
    /// Captured verbatim into the molecule's `topic` variable and thus
    /// into `prompt.md` (sealed by the usual nucleate path). Quote the
    /// argument if it contains spaces.
    pub text: String,

    /// Override the molecule kind. Defaults to `idea` (💡) — the Jobs §2
    /// shape. Accepts `idea`, `task`, `issue`, or any other
    /// [`cosmon_core::kind::MoleculeKind`] string the operator cares to
    /// pass; the actual validation happens in `nucleate`.
    #[arg(long, value_name = "KIND", default_value = "idea")]
    pub kind: String,

    /// Tag to attach (repeatable). When no `--tag` is supplied the
    /// spark lands with `temp:hot` so it surfaces immediately in
    /// `cs inbox` (HOT bucket) and `cs ensemble --tag temp:hot`.
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,

    /// Fleet to nucleate into. Defaults to `default`.
    #[arg(long, default_value = "default")]
    pub fleet: String,

    /// Override the auto-derived `nucleon_id` (sparker identity).
    ///
    /// Normally derived from `git config user.email` with a
    /// `$USER@$(hostname)` fallback. Pass this when scripting a test
    /// demo or when the git email is not the identity you want to
    /// record.
    #[arg(long)]
    pub nucleon: Option<String>,

    /// Currently-open pilot-session molecule id (ADR-061 §`SparkedBy`).
    ///
    /// Recorded as a variable only in v1 — the `SparkedBy` typed link
    /// is reserved until ADR-061 is marked `accepted`. Passed verbatim
    /// into `prompt.md` so later migrations can recover the edge.
    #[arg(long = "sparked-by", value_name = "SESSION_ID")]
    pub sparked_by: Option<String>,

    /// Override the formula name (defaults to `spark`). Exists for
    /// tests and for exotic deployments that vendor their own capture
    /// formula; normal callers leave this unset.
    #[arg(long, default_value = "spark")]
    pub formula: String,

    /// Path to the formulas directory (defaults to walk-up discovery).
    #[arg(long, value_name = "DIR")]
    pub formulas_dir: Option<PathBuf>,

    /// Path to the state store root (defaults to walk-up discovery).
    #[arg(long, value_name = "DIR")]
    pub store_dir: Option<PathBuf>,
}

/// Execute the `spark` command.
///
/// # Errors
/// Propagates any error from [`super::nucleate::run`] — malformed tags,
/// missing formula, disk failure on the nucleate path, etc.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let tags = if args.tags.is_empty() {
        vec!["temp:hot".to_owned()]
    } else {
        args.tags.clone()
    };

    let nucleon_id = args
        .nucleon
        .clone()
        .unwrap_or_else(|| derive_nucleon_id(&EnvReaderReal));

    let mut variables: HashMap<&str, String> = HashMap::new();
    variables.insert("topic", args.text.clone());
    variables.insert("nucleon_id", nucleon_id);
    if let Some(ref sb) = args.sparked_by {
        variables.insert("sparked_by", sb.clone());
    }
    let vars_strs: Vec<String> = variables.iter().map(|(k, v)| format!("{k}={v}")).collect();

    let mut na = super::nucleate::Args::for_formula(&args.formula);
    na.fleet.clone_from(&args.fleet);
    na.kind = Some(args.kind.clone());
    na.tags = tags;
    na.vars = vars_strs;
    na.formulas_dir.clone_from(&args.formulas_dir);
    na.store_dir.clone_from(&args.store_dir);
    // Sparks are orphan top-level intents by default; the env-driven
    // auto-parent contract would attach a spurious DecayedFrom edge
    // when a worker happens to shell out `cs spark` from inside its
    // own worktree. Suppress it — a spark is the operator's voice,
    // not a worker's subtask.
    na.no_parent = true;

    // STREAM half (delib-20260509-18df §D-B): record the spark on
    // events.jsonl so a future operator-attention-patrol can derive
    // spark→verdict latency. spark_id is a content-addressed digest
    // of the spark text (stable across re-runs) so a deduplicated
    // analysis can join across days.
    {
        let state_dir = cosmon_filestore::resolve_state_dir(args.store_dir.as_deref());
        let spark_id = spark_id_from_text(&args.text);
        let content_hash = blake3_hex_prefix(&args.text);
        crate::operator_event::emit_operator_spark(
            &state_dir,
            &spark_id,
            "cli",
            &content_hash,
            None,
            None,
        );
    }

    super::nucleate::run(ctx, &na)
}

/// BLAKE3 hex prefix (16 chars) of a string — used for the
/// `spark_id` and `content_hash` fields of `operator.spark` events.
/// Sixteen hex chars (64 bits) is enough to disambiguate sparks
/// within a single operator's lifetime without bloating the line.
fn blake3_hex_prefix(s: &str) -> String {
    let hex = cosmon_hash::Hash::of_bytes(s.as_bytes()).to_hex();
    hex[..16].to_owned()
}

/// Derive a stable `spark_id` from the spark text. Same content → same
/// id, so a re-run of `cs spark "same text"` joins on the same id and
/// the operator-attention-patrol can detect repeated nudges.
fn spark_id_from_text(text: &str) -> String {
    format!("spark-{}", blake3_hex_prefix(text))
}

/// Environment reader trait — abstracts `env::var` + `Command` lookups
/// so the unit test for [`derive_nucleon_id`] can stub identity
/// discovery without poisoning the process env or spawning git.
pub(crate) trait EnvReader {
    /// Return the git-config user email, if set.
    fn git_user_email(&self) -> Option<String>;
    /// Return the `$USER` env var.
    fn user(&self) -> Option<String>;
    /// Return the system hostname.
    fn hostname(&self) -> Option<String>;
}

struct EnvReaderReal;

impl EnvReader for EnvReaderReal {
    fn git_user_email(&self) -> Option<String> {
        let out = Command::new("git")
            .args(["config", "--get", "user.email"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }

    fn user(&self) -> Option<String> {
        std::env::var("USER").ok().filter(|s| !s.is_empty())
    }

    fn hostname(&self) -> Option<String> {
        let out = Command::new("hostname").output().ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

/// Derive the sparker identity. Tries in order: git email → `$USER@$(hostname)`
/// → the literal `unknown@unknown` (so the field is never empty).
pub(crate) fn derive_nucleon_id(env: &dyn EnvReader) -> String {
    if let Some(email) = env.git_user_email() {
        return email;
    }
    let user = env.user().unwrap_or_else(|| "unknown".to_owned());
    let host = env.hostname().unwrap_or_else(|| "unknown".to_owned());
    format!("{user}@{host}")
}

#[cfg(test)]
pub(crate) fn real_repo_formula_path() -> PathBuf {
    // CARGO_MANIFEST_DIR points at crates/cosmon-cli; the repo's shared
    // formulas live at <repo>/.cosmon/formulas. Walk up two parents.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join(".cosmon/formulas/spark.formula.toml"))
        .expect("spark.formula.toml locatable from manifest")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Deterministic env stub for the identity-derivation tests.
    struct StubEnv {
        git_email: Option<String>,
        user: Option<String>,
        host: Option<String>,
    }

    impl EnvReader for StubEnv {
        fn git_user_email(&self) -> Option<String> {
            self.git_email.clone()
        }
        fn user(&self) -> Option<String> {
            self.user.clone()
        }
        fn hostname(&self) -> Option<String> {
            self.host.clone()
        }
    }

    #[test]
    fn derive_nucleon_id_prefers_git_email() {
        let env = StubEnv {
            git_email: Some("operator-demo@democorp.example".to_owned()),
            user: Some("you".to_owned()),
            host: Some("macbook".to_owned()),
        };
        assert_eq!(derive_nucleon_id(&env), "operator-demo@democorp.example");
    }

    #[test]
    fn derive_nucleon_id_falls_back_to_user_at_host() {
        let env = StubEnv {
            git_email: None,
            user: Some("operator-demo".to_owned()),
            host: Some("iphone".to_owned()),
        };
        assert_eq!(derive_nucleon_id(&env), "operator-demo@iphone");
    }

    #[test]
    fn derive_nucleon_id_handles_missing_user_and_host() {
        let env = StubEnv {
            git_email: None,
            user: None,
            host: None,
        };
        assert_eq!(derive_nucleon_id(&env), "unknown@unknown");
    }

    /// End-to-end test: `cs spark` produces a molecule state.json with
    /// the right kind, `temp:hot` tag, and a `nucleon_id` variable.
    ///
    /// Drives `run(ctx, args)` directly against a tempdir-backed
    /// state + formulas directory, reads the JSON file the nucleate
    /// path writes, and asserts on its shape. No subprocess spawn.
    #[test]
    fn spark_produces_molecule_with_right_kind_tag_and_nucleon_id() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();

        // Copy the repo's spark formula into the tempdir so the test
        // exercises the real formula rather than an ad-hoc fixture.
        let src_formula = super::real_repo_formula_path();
        let dst_formula = formulas_dir.join("spark.formula.toml");
        fs::copy(&src_formula, &dst_formula).unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = Args {
            text: "réunion demain : revoir le pitch".to_owned(),
            kind: "idea".to_owned(),
            tags: vec![],
            fleet: "default".to_owned(),
            nucleon: Some("operator-demo@demo.example".to_owned()),
            sparked_by: None,
            formula: "spark".to_owned(),
            formulas_dir: Some(formulas_dir.clone()),
            store_dir: Some(state_dir.clone()),
        };

        run(&ctx, &args).unwrap();

        // Discover the single molecule directory the nucleate path wrote.
        let mol_root = state_dir.join("fleets").join("default").join("molecules");
        let mut dirs = fs::read_dir(&mol_root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect::<Vec<_>>();
        dirs.retain(|p| p.is_dir());
        assert_eq!(dirs.len(), 1, "expected exactly one molecule to be created");
        let mol_dir = &dirs[0];

        let state_json = fs::read_to_string(mol_dir.join("state.json")).unwrap();
        let state: serde_json::Value = serde_json::from_str(&state_json).unwrap();

        assert_eq!(state["kind"].as_str(), Some("idea"));
        let tags: Vec<&str> = state["tags"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(
            tags.contains(&"temp:hot"),
            "default tag should be temp:hot, got {tags:?}"
        );
        assert_eq!(
            state["variables"]["nucleon_id"].as_str(),
            Some("operator-demo@demo.example"),
            "nucleon_id variable must round-trip unchanged"
        );
        assert_eq!(
            state["variables"]["topic"].as_str(),
            Some("réunion demain : revoir le pitch"),
            "topic variable must hold the raw spark text"
        );

        // Molecule id prefix comes from the formula's `id_prefix` field.
        let id = state["id"].as_str().unwrap();
        assert!(
            id.starts_with("spark-"),
            "molecule id should carry the spark-* prefix, got {id}"
        );

        // prompt.md must exist and mention the topic verbatim — this is
        // the surface operator-demo's spark lands on when the operator opens it
        // from the Inbox.
        let prompt = fs::read_to_string(mol_dir.join("prompt.md")).unwrap();
        assert!(prompt.contains("réunion demain : revoir le pitch"));
        assert!(prompt.contains("operator-demo@demo.example"));
    }

    #[test]
    fn spark_respects_explicit_tag_overrides() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::copy(
            super::real_repo_formula_path(),
            formulas_dir.join("spark.formula.toml"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = Args {
            text: "cold idea".to_owned(),
            kind: "idea".to_owned(),
            tags: vec!["temp:cold".to_owned()],
            fleet: "default".to_owned(),
            nucleon: Some("op@host".to_owned()),
            sparked_by: None,
            formula: "spark".to_owned(),
            formulas_dir: Some(formulas_dir.clone()),
            store_dir: Some(state_dir.clone()),
        };
        run(&ctx, &args).unwrap();

        let mol_root = state_dir.join("fleets").join("default").join("molecules");
        let mol_dir = fs::read_dir(&mol_root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .unwrap();
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();
        let tags: Vec<&str> = state["tags"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(tags, vec!["temp:cold"]);
    }
}
