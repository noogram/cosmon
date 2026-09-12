// SPDX-License-Identifier: AGPL-3.0-only

//! Per-step **harness settings** — the opaque `key = value` map cosmon carries
//! from a formula step (or a `cs tackle --harness` flag) to the adapter's own
//! native override channel (ADR-177, issue #65).
//!
//! # Why this module exists
//!
//! cosmon can pin *which model* a step runs on. It could not pin *how* that
//! model runs: codex's `model_reasoning_effort` stayed at whatever the
//! machine's `~/.codex/config.toml` said, and Claude Code's own flags were
//! unreachable. A spore shipped to a recipient therefore depended on the
//! recipient's machine-wide harness config — the one thing a spore exists to
//! stop depending on.
//!
//! # The contract, in three sentences (ADR-177 Decisions 1, 2 and 4)
//!
//! 1. **Opaque.** cosmon recognises **zero** keys. The map is carried verbatim
//!    to the adapter's native channel and logged verbatim as sent; cosmon never
//!    normalises a key, rewrites a value, or maintains an allowlist. An unknown
//!    key fails in the *harness's* own parser, at launch, loudly — which is
//!    where the knowledge about that harness's keys actually lives and stays
//!    current. A recognised key would become public API carried in files on
//!    other people's disks, and spores are not in `cargo semver-checks`' view.
//! 2. **Per-key merge.** Precedence is `flag > step pin > adapter default`,
//!    resolved **key by key**, never wholesale. A step that raises effort must
//!    not thereby drop a budget cap set at another level.
//! 3. **Dispatched, not ran.** Everything here is *ex-ante*: it records what
//!    cosmon asked for, minted before the process exists. The strongest
//!    sentence an acceptance run may print is *"cosmon requested E through
//!    channel C; the harness's own log reported E"* — a two-party agreement,
//!    not a proof of behaviour.
//!
//! The floor — rank 3, the adapter's own default — is not a cosmon surface at
//! all. It is *silence*: cosmon passing no key for `k` is the only way the
//! harness's own default can apply, exactly as [`crate::event_v2::ModelSelectionSource::Default`]
//! resolves to `None` rather than to a named model.
//!
//! # Zero-I/O
//!
//! Resolution and rendering are pure functions of already-read values. Probing
//! a harness's version, spawning it, and reading its echo are the caller's job,
//! in the shell.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The opaque harness map as it appears on a formula step or a flag: ordered
/// `key → value`, both carried verbatim.
///
/// `BTreeMap` rather than `HashMap` because the rendered argv must be
/// **deterministic**: a command line that reorders between two runs cannot be
/// diffed against the harness's echo, which is the whole point of Decision 1's
/// verbatim rule.
pub type HarnessMap = BTreeMap<String, String>;

/// Where one resolved harness key came from — the harness sibling of
/// [`crate::event_v2::ModelSelectionSource`], one level shorter.
///
/// There is no `Default` variant on purpose: rank 3 of ADR-177's table is the
/// harness's own config, which cosmon does not read and does not pass. A key
/// nobody set simply has no entry in the resolved map, and so has no source to
/// record — silence, not a named floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum HarnessSelectionSource {
    /// Rank 1 — `cs tackle --harness <key>=<value>`, the operator's
    /// in-the-moment choice. Always wins, **per key**.
    Flag {
        /// The flag text as the operator typed it (`"key=value"`), kept
        /// verbatim so the receipt names the origin and not a reconstruction.
        raw: String,
    },
    /// Rank 2 — a `[steps.harness]` pin on the executing formula step: the
    /// per-workflow override a spore carries.
    FormulaPin {
        /// Formula the pin was read from.
        formula: String,
        /// Id of the step whose `[steps.harness]` table carried the key.
        step: String,
    },
}

impl HarnessSelectionSource {
    /// The wire tag for this source (`"flag"` / `"formula"`), matching the
    /// vocabulary ADR-177 Decision 5 names for the ex-ante receipt.
    #[must_use]
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Flag { .. } => "flag",
            Self::FormulaPin { .. } => "formula",
        }
    }
}

/// One harness key after the per-key merge: the value that won, and the level
/// it won from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedHarnessSetting {
    /// The value, carried verbatim from its source.
    pub value: String,
    /// Which level of ADR-177 Decision 2's table supplied it.
    pub selection_source: HarnessSelectionSource,
}

/// The resolved map: `key → (value, source)`, ordered for a deterministic argv.
pub type ResolvedHarness = BTreeMap<String, ResolvedHarnessSetting>;

/// Merge the formula-step pin and the CLI flag **per key**, flag winning.
///
/// This is the whole of ADR-177 Decision 2 that cosmon can express: rank 3 is
/// the harness's own default and is reached by *not emitting the key*.
///
/// Per-key merge is the load-bearing half. Wholesale replacement would make
/// every level a complete restatement of every other, which is how a two-key
/// map silently loses a key: a step raising `model_reasoning_effort` would drop
/// a `model_max_output_tokens` cap the operator set on the same step.
///
/// `formula_step` is `(map, formula_name, step_id)` for the currently executing
/// step, or `None` when no step pin applies.
#[must_use]
pub fn resolve_harness_settings(
    flag: &HarnessMap,
    formula_step: Option<(&HarnessMap, &str, &str)>,
) -> ResolvedHarness {
    let mut resolved = ResolvedHarness::new();
    if let Some((pins, formula, step)) = formula_step {
        for (key, value) in pins {
            resolved.insert(
                key.clone(),
                ResolvedHarnessSetting {
                    value: value.clone(),
                    selection_source: HarnessSelectionSource::FormulaPin {
                        formula: formula.to_owned(),
                        step: step.to_owned(),
                    },
                },
            );
        }
    }
    for (key, value) in flag {
        // Per key: this overwrites only `key`, leaving every sibling the step
        // pinned exactly where it was.
        resolved.insert(
            key.clone(),
            ResolvedHarnessSetting {
                value: value.clone(),
                selection_source: HarnessSelectionSource::Flag {
                    raw: format!("{key}={value}"),
                },
            },
        );
    }
    resolved
}

/// A malformed `--harness` flag value.
///
/// The grammar is one line long — `key=value`, split at the **first** `=` so a
/// value may itself contain one — and both halves must be non-empty. Refusing
/// here, at parse time, is what keeps a typo from being carried verbatim to a
/// harness that would reject it far less legibly.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HarnessFlagError {
    /// The argument carried no `=` at all.
    #[error(
        "--harness expects `key=value` (got `{raw}`). cosmon recognises no \
         harness keys and carries the pair verbatim to the adapter; the key is \
         the harness's own, e.g. `--harness model_reasoning_effort=high`."
    )]
    NotAPair {
        /// The argument as typed.
        raw: String,
    },
    /// The key half was empty (`=value`).
    #[error("--harness key is empty in `{raw}`; expected `key=value`")]
    EmptyKey {
        /// The argument as typed.
        raw: String,
    },
    /// The same key was passed twice on one command line. Refused rather than
    /// silently last-wins: two flags naming one key is a mistake the operator
    /// wants to hear about, and cosmon has no basis to pick between them.
    #[error(
        "--harness {key}=… was passed more than once; a key may appear at most \
         once per invocation"
    )]
    DuplicateKey {
        /// The repeated key.
        key: String,
    },
}

/// Parse the repeated `cs tackle --harness key=value` flag into a
/// [`HarnessMap`].
///
/// An empty *value* is accepted: `--harness foo=` is a legitimate way to hand a
/// harness an empty string, and cosmon does not judge values.
///
/// # Errors
///
/// [`HarnessFlagError`] when an argument is not a `key=value` pair, carries an
/// empty key, or repeats a key already given.
pub fn parse_harness_flags<S: AsRef<str>>(args: &[S]) -> Result<HarnessMap, HarnessFlagError> {
    let mut map = HarnessMap::new();
    for arg in args {
        let raw = arg.as_ref();
        let Some((key, value)) = raw.split_once('=') else {
            return Err(HarnessFlagError::NotAPair {
                raw: raw.to_owned(),
            });
        };
        if key.trim().is_empty() {
            return Err(HarnessFlagError::EmptyKey {
                raw: raw.to_owned(),
            });
        }
        if map.contains_key(key) {
            return Err(HarnessFlagError::DuplicateKey {
                key: key.to_owned(),
            });
        }
        map.insert(key.to_owned(), value.to_owned());
    }
    Ok(map)
}

/// *Which* override surface carried a key to its harness.
///
/// Recorded on every ex-ante receipt because two keys of the same map may
/// legitimately leave through different channels, and a reader must not have to
/// infer which (ADR-177 Decision 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarnessChannel {
    /// codex's generic config override: one `-c key=value` per entry. It
    /// accepts any dotted key and TOML-parses the value, which is why an
    /// allowlist for codex would be enumerating a set the harness does not
    /// have.
    CodexConfigOverride,
    /// Claude Code's own flags: `--<key> <value>`, with **no** allowlist. An
    /// unknown key is rejected by Claude Code's parser at launch, naming
    /// itself; cosmon names the adapter alongside it.
    ClaudeFlag,
}

impl HarnessChannel {
    /// The wire token for this channel, stable across releases because it is
    /// what an acceptance run greps for.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CodexConfigOverride => "codex:-c",
            Self::ClaudeFlag => "claude:flag",
        }
    }
}

/// One harness key, rendered for a concrete adapter's command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessArg {
    /// The key, verbatim.
    pub key: String,
    /// The value, verbatim.
    pub value: String,
    /// Which level of the precedence table supplied it.
    pub selection_source: HarnessSelectionSource,
    /// Which override surface carries it.
    pub channel: HarnessChannel,
    /// The argv tokens appended to the command line for this key, in order —
    /// `["-c", "model_reasoning_effort=high"]` or `["--effort", "high"]`.
    ///
    /// This is the field Decision 5 calls *the realized argv fragment*: the
    /// only one that can later be diffed against the harness's own echo. A
    /// normalised record would prove that cosmon's normaliser ran, and nothing
    /// else.
    pub argv: Vec<String>,
}

impl HarnessArg {
    /// The argv fragment as one displayable string, for the event receipt and
    /// the readiness trace. Tokens are joined by a single space; they are
    /// **not** shell-quoted here, because this is a record of the argv vector,
    /// not a command to re-run.
    #[must_use]
    pub fn argv_fragment(&self) -> String {
        self.argv.join(" ")
    }
}

/// An adapter was handed a non-empty harness map and has no channel to carry it.
///
/// Fail closed at launch, naming the adapter — the same posture as the illegal
/// adapter/model pair. A setting is never silently dropped (ADR-177
/// Decision 5); the alternative would be a spore whose `[steps.harness]` looks
/// honoured on every adapter and is honoured on two.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "adapter '{adapter}' has no harness-settings channel, but {count} harness \
     key(s) were resolved for this dispatch ({keys}). cosmon never silently \
     drops a setting: re-dispatch without `--harness` / without the step's \
     `[steps.harness]` table, or pick an adapter that carries them (codex, \
     claude)."
)]
pub struct UnsupportedHarnessCarrier {
    /// The adapter that cannot carry the map.
    pub adapter: String,
    /// How many keys were resolved.
    pub count: usize,
    /// The resolved keys, comma-separated, so the operator sees what to remove.
    pub keys: String,
}

/// Render a resolved harness map onto `adapter`'s native override channel.
///
/// Each adapter maps the **whole** map — cosmon does not filter, because it
/// recognises no keys:
///
/// - `codex` → one `-c key=value` per entry ([`HarnessChannel::CodexConfigOverride`]).
/// - `claude` → `--<key> <value>` per entry, no allowlist
///   ([`HarnessChannel::ClaudeFlag`]); Claude Code's own parser rejects an
///   unknown flag at launch, loudly.
/// - anything else → [`UnsupportedHarnessCarrier`], **only** when the map is
///   non-empty. An empty map renders to an empty vec on every adapter, so a
///   dispatch that pins nothing is byte-identical to the pre-#65 shape on every
///   arm.
///
/// # Errors
///
/// [`UnsupportedHarnessCarrier`] when `adapter` has no channel and the map is
/// non-empty.
pub fn render_harness_args(
    adapter: &str,
    resolved: &ResolvedHarness,
) -> Result<Vec<HarnessArg>, UnsupportedHarnessCarrier> {
    if resolved.is_empty() {
        return Ok(Vec::new());
    }
    let channel = match adapter {
        "codex" => HarnessChannel::CodexConfigOverride,
        "claude" => HarnessChannel::ClaudeFlag,
        other => {
            return Err(UnsupportedHarnessCarrier {
                adapter: other.to_owned(),
                count: resolved.len(),
                keys: resolved.keys().cloned().collect::<Vec<_>>().join(", "),
            });
        }
    };
    Ok(resolved
        .iter()
        .map(|(key, setting)| {
            let argv = match channel {
                HarnessChannel::CodexConfigOverride => {
                    vec!["-c".to_owned(), format!("{key}={}", setting.value)]
                }
                HarnessChannel::ClaudeFlag => {
                    vec![format!("--{key}"), setting.value.clone()]
                }
            };
            HarnessArg {
                key: key.clone(),
                value: setting.value.clone(),
                selection_source: setting.selection_source.clone(),
                channel,
                argv,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> HarnessMap {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn an_absent_pin_and_an_absent_flag_resolve_to_silence() {
        // Rank 3 is the harness's own config, reached by emitting nothing.
        let resolved = resolve_harness_settings(&HarnessMap::new(), None);
        assert!(
            resolved.is_empty(),
            "no level set a key → no key is carried"
        );
    }

    #[test]
    fn a_step_pin_survives_a_flag_that_names_another_key() {
        // ADR-177 Decision 2: the merge is per key, never wholesale. This is
        // falsifier 2's second half — a step pinning two keys, a flag
        // overriding one, and the other surviving untouched.
        let step = map(&[
            ("model_reasoning_effort", "high"),
            ("model_max_output_tokens", "4096"),
        ]);
        let flag = map(&[("model_reasoning_effort", "low")]);
        let resolved = resolve_harness_settings(&flag, Some((&step, "task-work", "implement")));

        assert_eq!(resolved.len(), 2, "the untouched key must survive");
        let effort = &resolved["model_reasoning_effort"];
        assert_eq!(effort.value, "low", "the flag wins for the key it names");
        assert_eq!(effort.selection_source.tag(), "flag");
        let tokens = &resolved["model_max_output_tokens"];
        assert_eq!(tokens.value, "4096", "the sibling key is not replaced");
        assert_eq!(tokens.selection_source.tag(), "formula");
    }

    #[test]
    fn a_step_pin_records_its_formula_and_step() {
        let step = map(&[("model_reasoning_effort", "high")]);
        let resolved =
            resolve_harness_settings(&HarnessMap::new(), Some((&step, "task-work", "implement")));
        assert_eq!(
            resolved["model_reasoning_effort"].selection_source,
            HarnessSelectionSource::FormulaPin {
                formula: "task-work".to_owned(),
                step: "implement".to_owned(),
            }
        );
    }

    #[test]
    fn codex_renders_one_dash_c_per_key_in_a_stable_order() {
        let step = map(&[("model_reasoning_effort", "high"), ("a_key", "1")]);
        let resolved = resolve_harness_settings(&HarnessMap::new(), Some((&step, "f", "s")));
        let args = render_harness_args("codex", &resolved).expect("codex carries the map");
        assert_eq!(
            args.iter()
                .map(HarnessArg::argv_fragment)
                .collect::<Vec<_>>(),
            vec!["-c a_key=1", "-c model_reasoning_effort=high"],
            "BTreeMap order makes the argv diffable against the echo"
        );
        assert_eq!(args[0].channel, HarnessChannel::CodexConfigOverride);
    }

    #[test]
    fn claude_renders_a_flag_per_key_with_no_allowlist() {
        // ADR-177 Decision 1: the `claude` allowlist proposed by #65 is
        // dropped. An unrecognisable key still renders; Claude Code's own
        // parser is what rejects it, at launch.
        let flag = map(&[("effort", "xhigh"), ("not-a-real-flag", "x")]);
        let resolved = resolve_harness_settings(&flag, None);
        let args = render_harness_args("claude", &resolved).expect("claude carries the map");
        assert_eq!(
            args.iter()
                .map(HarnessArg::argv_fragment)
                .collect::<Vec<_>>(),
            vec!["--effort xhigh", "--not-a-real-flag x"]
        );
    }

    #[test]
    fn an_adapter_with_no_carrier_refuses_a_non_empty_map() {
        // Falsifier 6. opencode is deliberately out of #65's scope; a map
        // reaching it must fail at launch naming the adapter, never be dropped.
        let flag = map(&[("model_reasoning_effort", "high")]);
        let resolved = resolve_harness_settings(&flag, None);
        let err = render_harness_args("opencode", &resolved)
            .expect_err("opencode has no harness channel");
        assert_eq!(err.adapter, "opencode");
        assert!(
            err.to_string().contains("opencode"),
            "the refusal must name the adapter: {err}"
        );
        assert!(err.to_string().contains("model_reasoning_effort"));
    }

    #[test]
    fn an_empty_map_is_carried_by_every_adapter() {
        // The absence-default: a dispatch that pins nothing must be
        // byte-identical to the pre-#65 shape on every arm, including the ones
        // with no channel.
        for adapter in ["opencode", "aider", "local", "openai", "codex", "claude"] {
            assert_eq!(
                render_harness_args(adapter, &ResolvedHarness::new()),
                Ok(Vec::new()),
                "{adapter} must accept an empty map"
            );
        }
    }

    #[test]
    fn flag_parsing_splits_at_the_first_equals_only() {
        let parsed = parse_harness_flags(&["k=a=b", "empty="]).expect("both are well-formed pairs");
        assert_eq!(parsed["k"], "a=b", "a value may contain `=`");
        assert_eq!(parsed["empty"], "", "an empty value is a legitimate value");
    }

    #[test]
    fn flag_parsing_refuses_a_non_pair_an_empty_key_and_a_repeat() {
        assert_eq!(
            parse_harness_flags(&["noequals"]),
            Err(HarnessFlagError::NotAPair {
                raw: "noequals".to_owned()
            })
        );
        assert_eq!(
            parse_harness_flags(&["=v"]),
            Err(HarnessFlagError::EmptyKey {
                raw: "=v".to_owned()
            })
        );
        assert_eq!(
            parse_harness_flags(&["k=1", "k=2"]),
            Err(HarnessFlagError::DuplicateKey {
                key: "k".to_owned()
            })
        );
    }

    #[test]
    fn a_value_is_never_normalised() {
        // Decision 1's verbatim rule, made a test: whitespace, case and quoting
        // survive to the argv fragment untouched.
        let flag = map(&[("k", "  High \"x\"  ")]);
        let resolved = resolve_harness_settings(&flag, None);
        let args = render_harness_args("codex", &resolved).expect("codex carries the map");
        assert_eq!(args[0].argv_fragment(), r#"-c k=  High "x"  "#);
    }
}
