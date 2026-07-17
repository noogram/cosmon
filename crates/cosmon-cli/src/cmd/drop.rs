// SPDX-License-Identifier: AGPL-3.0-only

//! `cs drop <text>` — universal Inbox drop gesture.
//!
//! A **drop** is the operator's raw thought dropped into the Inbox from
//! *any* surface — the macOS hotkey sheet, the zsh widget, the mac-pilot
//! menubar, an SSH wrapper for an iPhone Shortcut. Every entry-point
//! funnels into this one verb so there is exactly one place where the
//! spark molecule is shaped and tagged.
//!
//! # Relationship to `cs spark`
//!
//! `cs drop` is a thin operator-surface over [`super::spark::run`]:
//!
//! - same `spark` formula (molecule id prefix: `spark-YYYYMMDD-xxxx`);
//! - same `prompt.md` seal, same nucleate event, same symmetric links;
//! - identity derivation (`nucleon_id`) unchanged;
//!
//! — with three additions specific to the drop surface:
//!
//! 1. **stdin fallback.** When `TEXT` is empty on the command line, the
//!    text is read from stdin. Supports `cs drop < file` and
//!    `echo "..." | cs drop`.
//! 2. **`--galaxy <name>`.** Resolves a galaxy by name via
//!    [`cosmon_registry::TomlGalaxyIndex`] and points the nucleation at
//!    that galaxy's `.cosmon/state/` store. Without the flag, the
//!    current walk-up discovery decides (same behaviour as `cs spark`).
//! 3. **Default tags `temp:hot` + `source:drop`.** The `source:drop`
//!    tag separates chord-originated sparks from other surfaces
//!    (`source:shortcut`, `source:session-note`, …) for later triage.
//!
//! # Wedge
//!
//! The macOS hotkey (`⌃⌥D`) fires a `SwiftUI` sheet that shells out to
//! `cs drop`. The zsh widget (`Ctrl-G`) passes `$BUFFER` to `cs drop`.
//! The mac-pilot menubar "Drop…" entry invokes the same sheet. None of
//! these callers need to know *how* a spark is nucleated — they only
//! need this one verb.

use std::io::Read;
use std::path::PathBuf;

use cosmon_registry::{GalaxyIndex, TomlGalaxyIndex};

use super::Context;

/// Arguments for the `drop` subcommand.
#[derive(clap::Args)]
pub struct Args {
    /// The drop text itself. When absent or empty, the text is read
    /// from stdin — supports `cs drop < file` and pipe chains.
    ///
    /// Multiple words are joined with single spaces so callers can
    /// pass raw command-line tokens (`cs drop hello there`) without
    /// quoting. Leading/trailing whitespace is trimmed.
    #[arg(value_name = "TEXT")]
    pub text: Vec<String>,

    /// Galaxy name to drop into. Resolved via
    /// `~/.config/cosmon/galaxies.toml`
    /// ([`TomlGalaxyIndex`]).
    ///
    /// When set, the resolved `Galaxy.path` becomes the store root
    /// (`<path>/.cosmon/state/`) for this nucleation. Without it,
    /// cosmon's usual walk-up discovery picks the galaxy from the
    /// current working directory.
    #[arg(long, value_name = "NAME")]
    pub galaxy: Option<String>,

    /// Molecule kind override. Defaults to `idea` — same as `cs spark`.
    ///
    /// The briefing names `spark | idea | task` as common choices;
    /// any valid [`cosmon_core::kind::MoleculeKind`] token is accepted
    /// (validated in `nucleate`).
    #[arg(long, value_name = "KIND", default_value = "idea")]
    pub kind: String,

    /// Additional tag to attach (repeatable). `temp:hot` and
    /// `source:drop` are always added; `--tag` extends the list.
    ///
    /// Callers use this to stamp the drop's origin surface (e.g.
    /// `--tag source:shortcut` from the iPhone SSH wrapper, which
    /// supplements rather than replaces `source:drop`).
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,

    /// Fleet to nucleate into. Defaults to `default`.
    #[arg(long, default_value = "default")]
    pub fleet: String,

    /// Override the auto-derived `nucleon_id` (drop author identity).
    /// Same semantics as `cs spark --nucleon`.
    #[arg(long)]
    pub nucleon: Option<String>,

    /// Currently-open pilot-session molecule id
    /// (ADR-061 §`SparkedBy`). Same semantics as
    /// `cs spark --sparked-by`.
    #[arg(long = "sparked-by", value_name = "SESSION_ID")]
    pub sparked_by: Option<String>,

    /// Override the formula name (defaults to `spark`). Exists for
    /// tests and exotic deployments; normal callers leave this unset.
    #[arg(long, default_value = "spark")]
    pub formula: String,

    /// Path to the formulas directory (default: walk-up discovery).
    #[arg(long, value_name = "DIR")]
    pub formulas_dir: Option<PathBuf>,

    /// Path to the state store root (default: walk-up discovery, or
    /// the galaxy-registry lookup when `--galaxy` is set).
    #[arg(long, value_name = "DIR")]
    pub store_dir: Option<PathBuf>,
}

/// Execute the `drop` command.
///
/// # Errors
/// Propagates any error from [`super::spark::run`] — malformed tags,
/// missing formula, disk failure on the nucleate path, etc. Also
/// returns an error if `--galaxy <name>` is supplied but the name is
/// not in the registry, or if the drop text is empty (both stdin and
/// positional empty).
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let text = resolve_text(&args.text, &mut std::io::stdin().lock())?;
    let store_dir = resolve_store_dir(args.galaxy.as_deref(), args.store_dir.clone())?;

    // Auto-tags: temp:hot (Inbox HOT bucket) + source:drop (origin surface).
    // Caller-supplied --tag flags are appended and deduplicated later by
    // the nucleate path's Tag parser.
    let mut tags: Vec<String> = vec!["temp:hot".to_owned(), "source:drop".to_owned()];
    for t in &args.tags {
        if !tags.contains(t) {
            tags.push(t.clone());
        }
    }

    let spark_args = super::spark::Args {
        text,
        kind: args.kind.clone(),
        tags,
        fleet: args.fleet.clone(),
        nucleon: args.nucleon.clone(),
        sparked_by: args.sparked_by.clone(),
        formula: args.formula.clone(),
        formulas_dir: args.formulas_dir.clone(),
        store_dir,
    };

    super::spark::run(ctx, &spark_args)
}

/// Decide the drop's text: positional `TEXT...` wins, otherwise read
/// stdin. Positional tokens are joined with single spaces so
/// `cs drop hello world` is equivalent to `cs drop "hello world"`.
///
/// Stdin is only consulted when `text` is empty — a non-empty positional
/// payload short-circuits, even when stdin also has data (matches the
/// scripting intuition: explicit args always win).
fn resolve_text(text: &[String], stdin: &mut dyn Read) -> anyhow::Result<String> {
    let joined = text.join(" ");
    let joined = joined.trim().to_owned();
    if !joined.is_empty() {
        return Ok(joined);
    }
    let mut buf = String::new();
    stdin
        .read_to_string(&mut buf)
        .map_err(|e| anyhow::anyhow!("failed to read stdin: {e}"))?;
    let trimmed = buf.trim().to_owned();
    if trimmed.is_empty() {
        anyhow::bail!("drop: empty text (no TEXT args, no stdin)");
    }
    Ok(trimmed)
}

/// Resolve the state-store override, combining `--galaxy` and
/// `--store-dir`.
///
/// Rules:
/// - `--store-dir` alone → use verbatim (mirrors `cs spark --store-dir`).
/// - `--galaxy <name>` alone → look up the galaxy in the TOML registry
///   and resolve to `<galaxy.path>/.cosmon/state/`.
/// - Both set → `--store-dir` wins (explicit beats derived — the usual
///   clap precedence).
/// - Neither set → return `None` and let walk-up discovery decide.
///
/// The galaxy name is case-sensitive, matching
/// [`GalaxyIndex::resolve`]. Unknown names are a hard error — dropping
/// into the wrong galaxy is worse than failing loudly.
fn resolve_store_dir(
    galaxy: Option<&str>,
    store_dir: Option<PathBuf>,
) -> anyhow::Result<Option<PathBuf>> {
    if store_dir.is_some() {
        return Ok(store_dir);
    }
    let Some(name) = galaxy else {
        return Ok(None);
    };
    let idx = TomlGalaxyIndex::load_default()
        .map_err(|e| anyhow::anyhow!("failed to load galaxy registry: {e}"))?;
    let g = idx.resolve(name).ok_or_else(|| {
        anyhow::anyhow!("unknown galaxy '{name}' (check ~/.config/cosmon/galaxies.toml)")
    })?;
    Ok(Some(g.path.join(".cosmon").join("state")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn resolve_text_prefers_positional() {
        let mut stdin = Cursor::new(b"from stdin".to_vec());
        let out = resolve_text(&["hello".into(), "world".into()], &mut stdin).unwrap();
        assert_eq!(out, "hello world");
    }

    #[test]
    fn resolve_text_falls_back_to_stdin_when_positional_empty() {
        let mut stdin = Cursor::new(b"  piped line\n".to_vec());
        let out = resolve_text(&[], &mut stdin).unwrap();
        assert_eq!(out, "piped line");
    }

    #[test]
    fn resolve_text_joins_positional_tokens_with_spaces() {
        let mut stdin = Cursor::new(Vec::new());
        let out = resolve_text(&["one".into(), "two".into(), "three".into()], &mut stdin).unwrap();
        assert_eq!(out, "one two three");
    }

    #[test]
    fn resolve_text_errors_when_both_empty() {
        let mut stdin = Cursor::new(Vec::new());
        let err = resolve_text(&[], &mut stdin).unwrap_err();
        assert!(
            err.to_string().contains("empty text"),
            "message should mention empty text, got: {err}"
        );
    }

    #[test]
    fn resolve_store_dir_explicit_store_dir_wins_over_galaxy() {
        let explicit = PathBuf::from("/tmp/explicit");
        let out = resolve_store_dir(Some("nonexistent"), Some(explicit.clone())).unwrap();
        assert_eq!(out, Some(explicit));
    }

    #[test]
    fn resolve_store_dir_none_without_overrides() {
        let out = resolve_store_dir(None, None).unwrap();
        assert!(out.is_none());
    }

    /// Full pipeline test: `cs drop "text"` nucleates a spark molecule
    /// with tags `temp:hot`, `source:drop`.
    ///
    /// Exercises the same tempdir pattern as `spark`'s end-to-end test;
    /// asserts on tag set and id prefix.
    #[test]
    fn drop_produces_spark_with_temp_hot_and_source_drop() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::copy(
            super::super::spark::real_repo_formula_path(),
            formulas_dir.join("spark.formula.toml"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = Args {
            text: vec!["a".into(), "dropped".into(), "thought".into()],
            galaxy: None,
            kind: "idea".to_owned(),
            tags: vec![],
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
            .expect("one molecule directory");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();

        let tags: Vec<&str> = state["tags"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(tags.contains(&"temp:hot"), "missing temp:hot: {tags:?}");
        assert!(
            tags.contains(&"source:drop"),
            "missing source:drop: {tags:?}"
        );

        let id = state["id"].as_str().unwrap();
        assert!(
            id.starts_with("spark-"),
            "id must carry spark- prefix: {id}"
        );

        assert_eq!(
            state["variables"]["topic"].as_str(),
            Some("a dropped thought"),
            "topic must be the joined positional text"
        );
    }

    #[test]
    fn drop_preserves_caller_tags_and_dedupes_autotags() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::copy(
            super::super::spark::real_repo_formula_path(),
            formulas_dir.join("spark.formula.toml"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let args = Args {
            text: vec!["x".into()],
            galaxy: None,
            kind: "idea".to_owned(),
            // Caller re-supplies temp:hot explicitly + adds source:shortcut;
            // the auto-tag must dedupe (no duplicate temp:hot), the extra
            // tag must be preserved verbatim.
            tags: vec!["temp:hot".to_owned(), "source:shortcut".to_owned()],
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

        // Each expected tag appears exactly once.
        let count = |needle: &str| tags.iter().filter(|t| **t == needle).count();
        assert_eq!(count("temp:hot"), 1, "temp:hot dedup failed: {tags:?}");
        assert_eq!(count("source:drop"), 1, "source:drop must be auto-applied");
        assert_eq!(count("source:shortcut"), 1, "caller tag missing");
    }
}
