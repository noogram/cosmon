// SPDX-License-Identifier: AGPL-3.0-only

//! The verdict-door mapping must be readable in ONE direction only.
//!
//! # The defect this pins
//!
//! `cmb-verify`'s door — `{confirmed | refuted | inconclusive}` — is defined as
//! *"the stated mechanism holds / does not hold"*. That is a **relative**
//! verdict: whether "holds" is good news depends on what the stated mechanism
//! CLAIMED, which the formula receives as a variable. Two polarities exist:
//!
//! - `defect` (bug intake): `confirmed` = the defect reproduces = FINDINGS;
//! - `fix` (a committee seat auditing a shipped fix): `confirmed` = the fix
//!   holds = CLEAN.
//!
//! Until 2026-07-28 three formula files stated the *fix* row unconditionally,
//! as `confirmed → CLEAN / nothing found`, twenty-five lines below a definition
//! whose own example (`confirmed` hands off to `bug-closure`) is the *defect*
//! row. A seat that had REPRODUCED A DEFECT would have been filed as CLEAN.
//! Rounds 1–3 of the clean-room convergence escaped it by luck: every seat
//! emitted `refuted`.
//!
//! # Why the assertion is on the prose
//!
//! There is no code reader for these verdicts — the reader is a worker reading
//! the formula. So the artefact that can be wrong is the prose, and the prose is
//! what this test reads. It does not grade writing: it asserts the single
//! structural property whose absence caused the inversion — that no *site* that
//! states the CLEAN correspondence does so without naming the polarity it is
//! relative to, and that the file which DEFINES the door carries both rows.
//!
//! # Why the assertion is PER SITE
//!
//! The first version of this falsifier asked `body.contains("polarity")` over
//! the WHOLE FILE flattened into one string. That question — *does the word
//! appear anywhere in this file?* — is not the question its own docstring
//! claimed to ask, and it cannot bind a statement to its condition. Measured
//! 2026-07-28: reverting `converge-clean-room.formula.toml` to its exact
//! pre-fix content and appending one bare comment line `# polarity` left this
//! test reporting `ok` on the fully-reproduced defect. A test that passes on
//! the exact defect it is named for is decoration.
//!
//! So the file is split into the **sites** a reader actually consumes —
//! paragraphs, and list items within them — and the condition is required in
//! the same site as the statement. That is also the true shape of the original
//! bug: the qualifier existed, twenty-five lines away, and the reader did not
//! carry it down.

use std::path::{Path, PathBuf};

/// The `.cosmon/formulas/` directory of the repository under test.
fn formulas_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cosmon/formulas")
}

fn read_formula(name: &str) -> String {
    let path = formulas_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Collapse the TOML's line-continuation backslashes and whitespace so a
/// sentence that wraps across lines still reads as one string.
fn flatten(raw: &str) -> String {
    raw.replace("\\\n", " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One site a reader consumes as a unit, with the line it starts on.
struct Site {
    line: usize,
    text: String,
}

/// Whether a line opens a new list item — `1.`, `- `, `* `, `(a)`. A numbered
/// instruction is read as its own unit, which is why an instruction may not
/// borrow its condition from a sibling item.
fn opens_list_item(line: &str) -> bool {
    let t = line.trim_start();
    if t.starts_with("- ") || t.starts_with("* ") {
        return true;
    }
    let digits: String = t.chars().take_while(char::is_ascii_digit).collect();
    !digits.is_empty() && t[digits.len()..].starts_with(". ")
}

/// Split a formula into the sites a reader consumes: blank-line-separated
/// paragraphs, further split at list-item boundaries.
///
/// Line continuations are joined WITHIN a site, never across one, so a sentence
/// that wraps still reads whole while a statement in one paragraph cannot reach
/// a qualifier in the next.
fn sites(raw: &str) -> Vec<Site> {
    let mut out: Vec<Site> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let flush = |buf: &mut Vec<&str>, start: usize, out: &mut Vec<Site>| {
        if !buf.is_empty() {
            out.push(Site {
                line: start,
                text: flatten(&buf.join("\n")),
            });
            buf.clear();
        }
    };
    for (idx, line) in raw.lines().enumerate() {
        let no = idx + 1;
        if line.trim().is_empty() {
            flush(&mut current, start, &mut out);
            continue;
        }
        if opens_list_item(line) {
            flush(&mut current, start, &mut out);
        }
        if current.is_empty() {
            start = no;
        }
        current.push(line);
    }
    flush(&mut current, start, &mut out);
    out
}

/// Remove every `{a | b | c}` **door enumeration** — a brace group naming two
/// or more alternatives of one vocabulary.
///
/// Naming a door's alternatives as a SET is not asserting a correspondence
/// between two doors: `cmb-verify speaks {confirmed | refuted | inconclusive}
/// while this contract speaks {CLEAN | FINDINGS | INCONCLUSIVE}` introduces two
/// vocabularies and maps nothing. Requiring a polarity there would force a word
/// into prose that is not wrong — decoration, which is what this rewrite exists
/// to remove. The exclusion is structural rather than a phrase allowlist, and a
/// bare mapping cannot hide inside it: `confirmed -> CLEAN` is not a
/// three-alternative enumeration.
fn strip_door_enumerations(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(end) = (i + 1..chars.len()).find(|&j| chars[j] == '}') {
                let pipes = chars[i + 1..end].iter().filter(|&&c| c == '|').count();
                if pipes >= 2 {
                    i = end + 1;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Whether `token` occurs in `text` as a whole word — letters, digits, `_` and
/// `-` all count as word characters.
///
/// Without this, `UNCONFIRMED` (a seat whose endpoint was never observed) reads
/// as `confirmed`, and `NOT-CLEAN` reads as `CLEAN` — two tokens that mean the
/// OPPOSITE of the ones this test is looking for.
fn contains_word(text: &str, token: &str) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(rel) = text[from..].find(token) {
        let at = from + rel;
        let before_ok = at == 0 || !text[..at].chars().next_back().is_some_and(is_word);
        let after = at + token.len();
        let after_ok = after >= bytes.len() || !text[after..].chars().next().is_some_and(is_word);
        if before_ok && after_ok {
            return true;
        }
        from = at + token.len();
    }
    false
}

/// Whether this site STATES the correspondence — names `confirmed` and `CLEAN`
/// together outside a door enumeration.
fn states_the_correspondence(site: &str) -> bool {
    let stripped = strip_door_enumerations(site);
    contains_word(&stripped, "confirmed") && contains_word(&stripped, "CLEAN")
}

/// Whether this site names the condition the correspondence is relative to.
/// Any spelling counts — `polarity`, `POLARITY`, `mechanism_polarity` — because
/// the requirement is that the reader meets the condition, not that an author
/// used one word.
fn names_the_polarity(site: &str) -> bool {
    site.to_ascii_lowercase().contains("polarity")
}

/// Every formula file that speaks the two vocabularies at once. The definition
/// is first; the rest consume it.
const SPEAKERS: [&str; 3] = [
    "cmb-verify.formula.toml",
    "converge-clean-room.formula.toml",
    "cross-provider-committee.formula.toml",
];

/// **The falsifier.** No SITE may state the `confirmed ↔ CLEAN` correspondence
/// without naming the polarity that correspondence is relative to. Stating it
/// bare is what made a reproduced defect readable as clean — and stating it
/// bare in an executable instruction, three paragraphs below a header that
/// states it correctly, is the same defect with a longer walk to it.
#[test]
fn no_formula_states_the_clean_correspondence_without_its_polarity() {
    let mut bare: Vec<String> = Vec::new();
    for name in SPEAKERS {
        for site in sites(&read_formula(name)) {
            if states_the_correspondence(&site.text) && !names_the_polarity(&site.text) {
                let excerpt: String = site.text.chars().take(180).collect();
                bare.push(format!("{name}:{} — {excerpt}…", site.line));
            }
        }
    }
    assert!(
        bare.is_empty(),
        "{} site(s) state the confirmed/CLEAN correspondence without naming the \
         POLARITY it is relative to. `confirmed` means 'the stated mechanism \
         holds', which is CLEAN only when the stated mechanism claimed a FIX; for \
         a claimed DEFECT it means the defect reproduces, which is FINDINGS. A \
         reader given the bare correspondence files a reproduced defect as clean, \
         and a reader executing a pseudocode body never scrolls back up to the \
         header that qualified it:\n  {}",
        bare.len(),
        bare.join("\n  "),
    );
}

/// The definition file must carry BOTH rows. One row plus an unstated
/// assumption is the same defect wearing a qualifier.
#[test]
fn the_definition_file_carries_both_polarity_rows() {
    let body = flatten(&read_formula("cmb-verify.formula.toml"));
    assert!(
        body.contains("defect confirmed FINDINGS"),
        "cmb-verify.formula.toml must state the DEFECT row explicitly — \
         `defect + confirmed -> FINDINGS`, the defect reproduces. It is the row \
         the file's own `bug-closure` hand-off implies and the one that was \
         missing from every table."
    );
    assert!(
        body.contains("fix confirmed CLEAN"),
        "cmb-verify.formula.toml must also state the FIX row — `fix + confirmed \
         -> CLEAN` — so a committee seat still has its mapping. Deleting the row \
         that was there would move the defect rather than close it."
    );
}

/// A consumer may not present itself as the definition. Two files that each
/// look authoritative is how they were allowed to drift apart for three rounds.
#[test]
fn the_consumers_name_the_definition_and_defer_to_it() {
    for name in [
        "converge-clean-room.formula.toml",
        "cross-provider-committee.formula.toml",
    ] {
        let body = flatten(&read_formula(name));
        assert!(
            body.contains("cmb-verify.formula.toml"),
            "{name} consumes the verdict door but never names \
             `cmb-verify.formula.toml` as where it is DEFINED. A reader who finds \
             a mapping here has no way to know it is a copy, and a copy is what \
             drifts."
        );
    }
}

/// The residual the fail-closed both-files rule does NOT close, named where a
/// reader will meet it: a seat emitting `confirmed` and `VERDICT: CLEAN`
/// together still passes that rule while its polarity says the opposite.
#[test]
fn the_converge_contract_names_the_agreeing_but_wrong_pair() {
    let body = flatten(&read_formula("converge-clean-room.formula.toml"));
    assert!(
        body.contains("Two files agreeing is not two files being right"),
        "converge-clean-room.formula.toml must name the residual its both-files \
         rule leaves open — a seat emitting `confirmed` AND `VERDICT: CLEAN` \
         while its polarity is `defect` satisfies 'affirmative CLEAN in both' and \
         is still wrong. An unnamed residual is one nobody checks."
    );
}

/// The declaration must be RESOLVED by something, not merely asserted.
///
/// The header mitigates the residual with *"a seat convened by this loop is
/// ALWAYS `polarity: fix`"*. On its own that is a claim about seats that no
/// reader enforces — the shape this whole lineage exists to refuse: a gate that
/// still passes when the constrained party lies, or is simply absent. So the
/// executable body must hand the triple to the reader that actually refuses it,
/// and that reader must exist in the workspace.
#[test]
fn the_polarity_rule_names_the_gate_that_enforces_it() {
    let body = flatten(&read_formula("converge-clean-room.formula.toml"));
    assert!(
        body.contains("cs reconcile --check"),
        "converge-clean-room.formula.toml states that a missing or inconsistent \
         `mechanism_polarity` is NOT-CLEAN but never sends the reader to a gate \
         that REFUSES one. `cs reconcile --check` runs the lint \
         (`check_seat_verdict_polarity`); name it in the body, or the rule is a \
         declaration nothing resolves."
    );

    let reader = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../cosmon-core/src/committee.rs"),
    )
    .expect("read cosmon-core/src/committee.rs");
    assert!(
        reader.contains("pub fn read_seat_emission"),
        "`cosmon_core::committee::read_seat_emission` is the code that maps a \
         seat's (polarity, verdict, VERDICT) triple and refuses a missing field \
         or an off-table row. Without it the polarity is a field nothing reads, \
         which is how the mitigation was allowed to be prose."
    );
}
