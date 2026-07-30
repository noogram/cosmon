// SPDX-License-Identifier: AGPL-3.0-only

//! A config field with no production reader is a defect — this test finds it.
//!
//! # The class this retires
//!
//! A config field is declared, parsed, documented — and read by no production
//! code. It reads as a control and is decoration. Every instance is invisible
//! until somebody sets it and nothing happens: `[project] trunk_branch` named
//! the galaxy's trunk without governing the merge destination; `[spore.fleet]
//! concurrency_cap` declared a bound no scheduler enforces. Both were found by
//! a human noticing, once, after the damage. This test is the detector so the
//! next one is found by CI.
//!
//! # What counts as a reader
//!
//! A **reader** is a field-access expression `.<field>` — not immediately
//! followed by `(`, which would make it a method call on a same-named method —
//! appearing in **production Rust source** under `crates/*/src/`, where
//! production means, in order of subtraction:
//!
//! * not inside a `#[cfg(test)]` block (a field read only by its own parse test
//!   has no reader — that is the single most common shape of this defect);
//! * not inside a comment or a string literal (a doc comment that *mentions*
//!   `[spore.astra].emit` is documentation, not a reader);
//! * not a same-name copy-through in a struct literal — `concurrency_cap:
//!   f.concurrency_cap,` is serde round-tripping the value from the `Raw*`
//!   deserialization struct into the typed one, which moves the byte without
//!   ever consulting it.
//!
//! Integration tests under `crates/*/tests/`, benches, and examples are outside
//! the scan entirely, for the same reason as `#[cfg(test)]`.
//!
//! # Where the field set comes from
//!
//! From the types, never from a list. [`ROOTS`] names two config *root* types;
//! the test parses their declaring modules, walks the type graph from each root
//! through every nested config struct, and checks every field it reaches. A
//! field added to `ProjectConfig` tomorrow is covered without anyone
//! remembering this file exists — which is the whole point, since a
//! hand-maintained list drifts and then passes because the new field was never
//! added to it.
//!
//! # Waivers
//!
//! Some fields legitimately have no in-tree reader: a value consumed only by an
//! external tool, or a slot deliberately reserved. Waive one with an inline
//! marker on the field's own declaration line, carrying a reason:
//!
//! ```ignore
//! pub some_field: String, // config-knob: allow — consumed by <x>, never by cosmon
//! ```
//!
//! Per line, never per file or per section — same discipline as `publish.sh`'s
//! `publish: allow` marker. A whole-file exclusion is a blind spot nobody sees
//! again; a marker is a sentence someone had to write and a reviewer reads in
//! the diff.
//!
//! # What this test is not
//!
//! It is a **lower bound**, and says so rather than pretending otherwise. The
//! match is textual, so a field whose name collides with a same-named field on
//! an unrelated type (`profile`, `output`, `enabled`) is reported as read when
//! the collision is somewhere else entirely. Such a field slips through. The
//! test therefore never *misses in the safe direction*: everything it flags is
//! genuinely unread, and a green run means "no *detectable* dead knob", not
//! "no dead knob". Narrowing the collision window needs type resolution, which
//! is a compiler, not a test.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The config root types, each with the module that declares it. Every struct
/// reachable from a root through a field type is checked.
const ROOTS: &[(&str, &str)] = &[
    ("crates/cosmon-core/src/config.rs", "ProjectConfig"),
    ("crates/cosmon-core/src/spore/mod.rs", "Spore"),
];

/// The inline waiver marker. Must carry a reason after it.
const WAIVER: &str = "config-knob: allow";

/// One declared field of a config struct.
struct Field {
    owner: String,
    name: String,
    ty: String,
    file: String,
    line: usize,
    waiver: Option<String>,
}

/// Workspace root, derived from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("cosmon-core lives at <workspace>/crates/cosmon-core")
        .to_path_buf()
}

/// Replace every comment and string-literal body with spaces, preserving byte
/// offsets so line numbers stay exact.
///
/// Blanking rather than deleting is deliberate: a reader search over the result
/// can still report the original line, and a `.field` that only ever appears
/// inside prose or a formatted error message is correctly not a reader.
fn blank_comments_and_strings(src: &str) -> String {
    let b: Vec<char> = src.chars().collect();
    let mut out: Vec<char> = b.clone();
    let mut i = 0;
    while i < b.len() {
        // Line comment — blank to end of line.
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '/' {
            while i < b.len() && b[i] != '\n' {
                out[i] = ' ';
                i += 1;
            }
            continue;
        }
        // Block comment — blank to the closing delimiter (non-nesting is fine
        // here; a nested `/*` inside one only blanks more, never less).
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            while i < b.len() {
                let end = b[i] == '*' && i + 1 < b.len() && b[i + 1] == '/';
                if b[i] != '\n' {
                    out[i] = ' ';
                }
                i += 1;
                if end {
                    if i < b.len() && b[i] != '\n' {
                        out[i] = ' ';
                    }
                    i += 1;
                    break;
                }
            }
            continue;
        }
        // Raw string — `r`, then N hashes, then the body up to `"` + N hashes.
        if b[i] == 'r' && !prev_is_ident_char(&b, i) {
            let mut j = i + 1;
            let mut hashes = 0;
            while j < b.len() && b[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < b.len() && b[j] == '"' {
                j += 1;
                while j < b.len() {
                    if b[j] == '"' && b[j + 1..].iter().take(hashes).all(|c| *c == '#') {
                        j += 1 + hashes;
                        break;
                    }
                    if b[j] != '\n' {
                        out[j] = ' ';
                    }
                    j += 1;
                }
                i = j;
                continue;
            }
        }
        // Ordinary string literal.
        if b[i] == '"' {
            let mut j = i + 1;
            while j < b.len() {
                if b[j] == '\\' {
                    if b[j] != '\n' {
                        out[j] = ' ';
                    }
                    j += 1;
                    if j < b.len() && b[j] != '\n' {
                        out[j] = ' ';
                    }
                    j += 1;
                    continue;
                }
                if b[j] == '"' {
                    j += 1;
                    break;
                }
                if b[j] != '\n' {
                    out[j] = ' ';
                }
                j += 1;
            }
            i = j;
            continue;
        }
        i += 1;
    }
    out.into_iter().collect()
}

/// `true` when the character before `i` can be part of an identifier.
fn prev_is_ident_char(b: &[char], i: usize) -> bool {
    i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '_')
}

/// Blank out every `#[cfg(test)]`-gated item by brace matching.
///
/// Line numbers are preserved (newlines survive) so a reported reader still
/// points at the right line.
fn blank_cfg_test_blocks(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut lines = src.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            out.push('\n');
            let mut depth: i32 = 0;
            let mut opened = false;
            for inner in lines.by_ref() {
                depth += i32::try_from(inner.matches('{').count()).unwrap_or(0);
                if inner.contains('{') {
                    opened = true;
                }
                depth -= i32::try_from(inner.matches('}').count()).unwrap_or(0);
                out.push('\n');
                if opened && depth <= 0 {
                    break;
                }
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Parse `struct <Name> { … }` declarations and their fields out of a module.
///
/// Deliberately textual, not a Rust parser: the alternative is a `syn`
/// dev-dependency for a job that rustfmt's own formatting already makes
/// unambiguous. A field declaration that does not fit the shape (a type broken
/// across lines) is skipped, which under-reports and never over-reports.
fn parse_structs(src: &str) -> BTreeMap<String, Vec<(String, String, usize, Option<String>)>> {
    let mut structs = BTreeMap::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let Some(name) = struct_name(lines[i]) else {
            i += 1;
            continue;
        };
        let mut fields = Vec::new();
        let mut j = i + 1;
        while j < lines.len() && lines[j] != "}" {
            if let Some((fname, fty, waiver)) = field_decl(lines[j]) {
                fields.push((fname, fty, j + 1, waiver));
            }
            j += 1;
        }
        structs.insert(name, fields);
        i = j + 1;
    }
    structs
}

/// `Some(name)` when the line opens a top-level struct declaration.
fn struct_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("pub struct ")
        .or_else(|| line.strip_prefix("struct "))?;
    let name = rest.strip_suffix(" {")?;
    name.chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        .then(|| name.to_owned())
}

/// `Some((name, type, waiver))` when the line declares a struct field.
fn field_decl(line: &str) -> Option<(String, String, Option<String>)> {
    let body = line.strip_prefix("    ")?;
    if body.starts_with(' ') || body.starts_with('#') || body.starts_with("//") {
        return None;
    }
    let (decl, waiver) = match body.find("//") {
        Some(at) => {
            let comment = body[at + 2..].trim();
            let reason = comment
                .strip_prefix(WAIVER)
                .map(|r| r.trim_start_matches([' ', '—', '-', ':']).trim().to_owned());
            (body[..at].trim_end(), reason)
        }
        None => (body, None),
    };
    let decl = decl.strip_suffix(',')?;
    let decl = decl.strip_prefix("pub ").unwrap_or(decl);
    let (name, ty) = decl.split_once(": ")?;
    name.chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        .then(|| (name.to_owned(), ty.to_owned(), waiver))
}

/// Every `crates/*/src/**/*.rs` file, as `(repo-relative path, source)`.
fn production_sources(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let crates = root.join("crates");
    let mut dirs: Vec<PathBuf> = fs::read_dir(&crates)
        .expect("crates/ is readable")
        .filter_map(|e| e.ok().map(|e| e.path().join("src")))
        .filter(|p| p.is_dir())
        .collect();
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let Ok(src) = fs::read_to_string(&path) else {
                    continue;
                };
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .display()
                    .to_string();
                out.push((
                    rel,
                    blank_comments_and_strings(&blank_cfg_test_blocks(&src)),
                ));
            }
        }
    }
    out.sort();
    out
}

/// Every identifier read as a field, mapped to the first `(file, line)` that
/// reads it.
///
/// One pass over the tree, indexed by identifier — not one pass per field, which
/// would be quadratic in a workspace this size.
type ReaderIndex = BTreeMap<String, (String, usize)>;

/// Index every production field access in `sources`.
fn index_readers(sources: &[(String, String)]) -> ReaderIndex {
    let mut index = ReaderIndex::new();
    for (path, src) in sources {
        for (ident, line) in field_accesses(src) {
            index.entry(ident).or_insert_with(|| (path.clone(), line));
        }
    }
    index
}

/// Every `(identifier, line)` appearing as a field access in one source file.
fn field_accesses(src: &str) -> Vec<(String, usize)> {
    let mut out = Vec::new();
    for (n, line) in src.lines().enumerate() {
        let b: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if !(b[i].is_alphabetic() || b[i] == '_') || prev_is_ident_char(&b, i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_') {
                i += 1;
            }
            // `.field(` is a method call on a same-named method, not a read.
            if b.get(i).copied() == Some('(') {
                continue;
            }
            // Receiver must be a `.`; rustfmt may put it at the head of a
            // continuation line, so an ident opening a line counts too.
            let mut k = start;
            while k > 0 && b[k - 1].is_whitespace() {
                k -= 1;
            }
            if k == 0 || b[k - 1] != '.' {
                continue;
            }
            let ident: String = b[start..i].iter().collect();
            // `concurrency_cap: f.concurrency_cap,` is serde round-tripping the
            // value from the `Raw*` struct into the typed one — the byte moves
            // without anyone consulting it.
            if line.trim_start().starts_with(&format!("{ident}:")) {
                continue;
            }
            out.push((ident, n + 1));
        }
    }
    out
}

/// Collect every field reachable from the configured roots.
fn reachable_fields(root: &Path) -> Vec<Field> {
    let mut declared: BTreeMap<String, (String, Vec<(String, String, usize, Option<String>)>)> =
        BTreeMap::new();
    for (rel, _) in ROOTS {
        let src = fs::read_to_string(root.join(rel)).expect("config module is readable");
        for (name, fields) in parse_structs(&src) {
            declared.insert(name, ((*rel).to_owned(), fields));
        }
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = ROOTS.iter().map(|(_, t)| (*t).to_owned()).collect();
    let mut out = Vec::new();
    while let Some(ty) = stack.pop() {
        if !seen.insert(ty.clone()) {
            continue;
        }
        let Some((file, fields)) = declared.get(&ty) else {
            continue;
        };
        for (name, fty, line, waiver) in fields {
            out.push(Field {
                owner: ty.clone(),
                name: name.clone(),
                ty: fty.clone(),
                file: file.clone(),
                line: *line,
                waiver: waiver.clone(),
            });
            for word in fty.split(|c: char| !c.is_alphanumeric() && c != '_') {
                if declared.contains_key(word) {
                    stack.push(word.to_owned());
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
    out
}

/// Every config field reachable from a root type must be read by production
/// code, or carry an inline waiver naming why it is not.
#[test]
fn every_config_field_has_a_production_reader() {
    let root = workspace_root();
    let fields = reachable_fields(&root);
    assert!(
        fields.len() > 50,
        "field enumeration collapsed to {} fields — the parser stopped matching \
         the config modules, so this gate would pass vacuously",
        fields.len()
    );
    let sources = production_sources(&root);
    assert!(
        sources.len() > 100,
        "production source scan found only {} files — the walk broke",
        sources.len()
    );
    let readers = index_readers(&sources);

    let mut dead = Vec::new();
    for field in &fields {
        if field.waiver.is_some() {
            continue;
        }
        if !readers.contains_key(&field.name) {
            dead.push(field);
        }
    }

    assert!(
        dead.is_empty(),
        "{} config field(s) are declared, parsed and documented but read by no \
         production code — a knob with no power:\n{}\n\nFix each one: wire it to \
         a real reader (not a reader that logs it), delete it, or waive it with \
         an inline `// {WAIVER} — <reason>` on its declaration line.",
        dead.len(),
        dead.iter()
            .map(|f| format!(
                "  - {}.{}: {} ({}:{})",
                f.owner, f.name, f.ty, f.file, f.line
            ))
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

/// A waiver must carry a reason — the marker alone is a whole-file exclusion in
/// disguise, which is the blind spot this shape exists to prevent.
#[test]
fn every_waiver_carries_a_reason() {
    let root = workspace_root();
    let empty: Vec<String> = reachable_fields(&root)
        .iter()
        .filter(|f| f.waiver.as_ref().is_some_and(|r| r.len() < 10))
        .map(|f| format!("{}.{} ({}:{})", f.owner, f.name, f.file, f.line))
        .collect();
    assert!(
        empty.is_empty(),
        "waiver marker without a usable reason on: {}",
        empty.join(", ")
    );
}

/// The detector must be able to see a dead knob. A synthetic field name that
/// appears nowhere in the tree must come back with no reader — otherwise the
/// reader search is matching something other than field access and the whole
/// gate is measuring the property next door.
#[test]
fn detector_reports_no_reader_for_a_field_nobody_reads() {
    let readers = index_readers(&production_sources(&workspace_root()));
    assert!(
        !readers.contains_key("knob_that_nothing_reads_zqx"),
        "a field name absent from the tree must have no reader"
    );
    assert!(
        readers.contains_key("project_id"),
        "`project_id` is read all over the tree — a detector that cannot see it \
         is not looking at field accesses"
    );
}

/// The waiver marker is recognised, carries its reason, and is not confused
/// with an ordinary trailing comment.
///
/// No field in tree is waived today, so `every_waiver_carries_a_reason` passes
/// vacuously and proves nothing about the mechanism. This test is what proves
/// the escape hatch works before somebody needs it under pressure.
#[test]
fn waiver_marker_is_parsed_with_its_reason() {
    let plain = field_decl("    pub knob: String,").expect("plain field parses");
    assert_eq!(plain.0, "knob");
    assert_eq!(plain.2, None, "a field with no comment is not waived");

    let commented =
        field_decl("    pub knob: String, // just a note").expect("commented field parses");
    assert_eq!(
        commented.2, None,
        "an ordinary trailing comment is not a waiver"
    );

    let waived = field_decl(
        "    pub knob: String, // config-knob: allow — read by the packaging script, never by cosmon",
    )
    .expect("waived field parses");
    assert_eq!(waived.0, "knob");
    assert_eq!(
        waived.2.as_deref(),
        Some("read by the packaging script, never by cosmon")
    );
}

/// The three subtractions that define "production reader" each have teeth.
#[test]
fn reader_definition_excludes_tests_comments_and_copy_through() {
    let src = "\
#[cfg(test)]
mod tests {
    fn t() { let _ = cfg.only_in_tests; }
}
/// Docs mentioning `.only_in_docs` are not a reader.
fn build(raw: Raw) -> Typed {
    Typed {
        only_copied: raw.only_copied,
    }
}
fn shout() -> &'static str { \"set cfg.only_in_a_string to enable\" }
fn real(c: &Cfg) -> bool { c.genuinely_read }
";
    let blanked = blank_comments_and_strings(&blank_cfg_test_blocks(src));
    let readers = index_readers(&[("synthetic.rs".to_owned(), blanked)]);
    for dead in [
        "only_in_tests",
        "only_in_docs",
        "only_copied",
        "only_in_a_string",
    ] {
        assert!(
            !readers.contains_key(dead),
            "`{dead}` must not count as read"
        );
    }
    assert!(
        readers.contains_key("genuinely_read"),
        "a plain field access is a reader"
    );
}
