// SPDX-License-Identifier: AGPL-3.0-only

//! `xtask gen-api-ref` — project the §8p surface canon into the
//! smithy API reference (avatar-surface A3, delib-20260610-9a0c
//! Q2 / shannon M3).
//!
//! The drift this closes was already live when the panel measured it:
//! the canon carried 30 routes (`GET /v1/molecules/{id}/result` landed
//! 2026-06-05) while the hand-maintained table in
//! `docs/specs/cosmon-rpp-api-reference.md` (smithy) still said 29.
//! *Delete the copy, don't add a checker*: the tables become generated
//! blocks; the surrounding prose (conventions, error semantics, curl
//! examples) stays hand-written in the same file.
//!
//! Mechanics: the target markdown carries marker-delimited blocks
//!
//! ```text
//! <!-- gen:begin routes-v1 … -->
//! (generated table)
//! <!-- gen:end routes-v1 -->
//! ```
//!
//! `gen-api-ref <target.md>` re-renders every known block in place;
//! `--check` exits non-zero when a re-render would change the file —
//! the « git diff vide après re-gen » gate, recomputable by anyone
//! (shannon K8: a gate is a re-computation, never a worker claim).
//! Every count in the generated blocks is computed from the canon —
//! no literal to hand-bump (wheeler I-ADDITIVE-COUNTERS).
//!
//! A cosmon-side golden (`xtask/tests/golden.rs`) pins the rendered
//! blocks, so a canon append fails cosmon CI until the smithy doc is
//! regenerated and the golden re-blessed — the cross-repo tripwire.

#![forbid(unsafe_code)]

use cosmon_surface_canon::{effect_annotation, parse_canon, CanonEvent, Exposure};

/// Canonical on-disk location of the canon, relative to the workspace
/// root (this crate lives at `<workspace>/xtask`).
pub const CANON_RELATIVE: &str = "crates/cosmon-rpp-adapter/data/surface_events.txt";

/// Route family used to group the reference tables. Derived
/// mechanically from `(method, path)`; an unclassifiable path is a
/// hard error so a future canon append forces a conscious choice here
/// instead of silently landing in a wrong bucket.
///
/// # Errors
///
/// Returns the offending path when no rule matches.
pub fn family(path: &str) -> Result<&'static str, String> {
    match path {
        "/v1/avatar/converse" | "/v1/avatar/perceive" => return Ok("avatar-canal"),
        "/v1/auth/me"
        | "/v1/events"
        | "/v1/quota"
        | "/v1/noyaux"
        | "/v1/workers"
        | "/v1/molecules/{id}/logs" => return Ok("observ."),
        _ => {}
    }
    if path.starts_with("/v1/auth/claude/") {
        return Ok("auth-claude");
    }
    if path.starts_with("/v1/avatar/") {
        return Ok("avatar-life");
    }
    if path.starts_with("/v1/molecules") && path.contains("/artifacts") {
        return Ok("artifact");
    }
    if path.starts_with("/v1/molecules") {
        return Ok("molecule");
    }
    if path.starts_with("/v1/admin/") {
        return Ok("admin");
    }
    Err(format!(
        "no family rule for canon path {path:?} — classify it in xtask/src/lib.rs::family"
    ))
}

/// Display order of the families in the generated tables (the order
/// the reference's prose sections follow).
const FAMILY_ORDER: &[&str] = &[
    "molecule",
    "artifact",
    "auth-claude",
    "observ.",
    "avatar-canal",
    "avatar-life",
    "admin",
];

fn method_path(ev: &CanonEvent) -> (&str, &str) {
    ev.method_path
        .split_once(' ')
        .expect("parse_canon guarantees `METHOD PATH` shape")
}

/// Markdown rendering of one scope expression, matching the
/// reference's established style: `—` for auth-level routes, and
/// `` `a` **ET** `b` `` for compound (AND) scopes.
fn scope_md(scope: &str) -> String {
    if scope == "-" {
        return "—".to_owned();
    }
    scope
        .split('+')
        .map(|s| format!("`{s}`"))
        .collect::<Vec<_>>()
        .join(" **ET** ")
}

/// The `routes-v1` block: the full frozen-surface catalogue, family
/// order, with the §8p exposure and the scope-derived effect marker
/// (godel C5) as columns. All counts computed.
///
/// # Errors
///
/// Propagates a family-classification failure.
pub fn render_routes_block(events: &[CanonEvent]) -> Result<String, String> {
    let mut out = String::new();
    let total = events.len();
    out.push_str(&format!(
        "**{total} routes `/v1/` gelées** — recomptées depuis le canon à chaque génération \
         (`{CANON_RELATIVE}`, cosmon). La colonne *Effet* est dérivée du scope requis \
         (godel C5 : un scope distinct par effet coûteux ou irréversible, ADR-080 §6.5) — \
         jamais éditée à la main.\n\n",
    ));
    out.push_str("| # | Famille | Méthode | Path | Scope requis | §8p | Effet |\n");
    out.push_str("|---|---|---|---|---|---|---|\n");

    let mut counts: Vec<(&str, usize)> = Vec::new();
    let mut i = 0usize;
    for fam in FAMILY_ORDER {
        let mut fam_count = 0usize;
        for ev in events {
            let (method, path) = method_path(ev);
            if family(path)? != *fam {
                continue;
            }
            i += 1;
            fam_count += 1;
            let effect = effect_annotation(&ev.scope)
                .map(|m| format!("`{m}`"))
                .unwrap_or_default();
            out.push_str(&format!(
                "| {i} | {fam} | {method} | `{path}` | {} | {} | {effect} |\n",
                scope_md(&ev.scope),
                ev.exposure.as_token(),
            ));
        }
        counts.push((fam, fam_count));
    }
    if i != total {
        return Err(format!(
            "family partition lost routes: placed {i}, canon has {total}"
        ));
    }

    let breakdown = counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(fam, n)| format!("**{n}** {fam}"))
        .collect::<Vec<_>>()
        .join(" + ");
    out.push_str(&format!("\nDécoupage : {breakdown} = **{total}**.\n"));
    Ok(out)
}

/// The `bijection-8p` block: liée/exempte status per route, derived
/// from the canon's exposure column (the two hand-maintained copies
/// `is_adapter_only()` + `forbidden` died in task-20260610-06a4).
///
/// # Errors
///
/// Propagates a family-classification failure.
pub fn render_bijection_block(events: &[CanonEvent]) -> Result<String, String> {
    let mut out = String::new();
    out.push_str(
        "| Route | Statut bijection (§8p) |\n\
         |---|---|\n",
    );
    let mut linked = 0usize;
    let mut exempt = 0usize;
    let mut linked_by_family: Vec<(&str, usize)> = Vec::new();
    for fam in FAMILY_ORDER {
        let mut fam_linked = 0usize;
        for ev in events {
            let (method, path) = method_path(ev);
            if family(path)? != *fam {
                continue;
            }
            let status = match ev.exposure {
                Exposure::TenantVerb => {
                    linked += 1;
                    fam_linked += 1;
                    "✅ liée (verbe tenant, bijection testée)"
                }
                Exposure::AdapterOnly => {
                    exempt += 1;
                    "⊘ exempte (adapter-only)"
                }
                Exposure::OperatorOnly => {
                    return Err(format!(
                        "operator-only route on the frozen surface: {method} {path}"
                    ));
                }
            };
            out.push_str(&format!("| `{method} {path}` | {status} |\n"));
        }
        if fam_linked > 0 {
            linked_by_family.push((fam, fam_linked));
        }
    }
    let linked_breakdown = linked_by_family
        .iter()
        .map(|(fam, n)| format!("{n} {fam}"))
        .collect::<Vec<_>>()
        .join(" + ");
    out.push_str(&format!(
        "\nBijection liée : **{linked}** ({linked_breakdown}). Exemptes : **{exempt}**. \
         Total : **{}** — recompté depuis le canon (colonne `exposure`) à chaque génération.\n",
        linked + exempt,
    ));
    Ok(out)
}

/// All generated blocks, keyed by marker name.
///
/// # Errors
///
/// Propagates rendering failures.
pub fn render_blocks(events: &[CanonEvent]) -> Result<Vec<(&'static str, String)>, String> {
    Ok(vec![
        ("routes-v1", render_routes_block(events)?),
        ("bijection-8p", render_bijection_block(events)?),
    ])
}

fn begin_marker(name: &str) -> String {
    format!(
        "<!-- gen:begin {name} — bloc généré par `cargo xtask gen-api-ref` (repo cosmon) \
         depuis {CANON_RELATIVE} ; NE PAS éditer à la main -->"
    )
}

fn end_marker(name: &str) -> String {
    format!("<!-- gen:end {name} -->")
}

/// Re-render every known block inside `document`. Hand-written prose
/// outside the markers is preserved byte-for-byte. Every known block
/// must be present exactly once; an unterminated or unknown
/// `gen:begin` is an error.
///
/// # Errors
///
/// Returns a message naming the structural problem (missing block,
/// missing end marker, unknown block name).
pub fn inject(document: &str, blocks: &[(&'static str, String)]) -> Result<String, String> {
    let mut out = String::with_capacity(document.len());
    let mut seen: Vec<&str> = Vec::new();
    let mut lines = document.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.trim_start().strip_prefix("<!-- gen:begin ") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            let Some((_, content)) = blocks.iter().find(|(n, _)| *n == name) else {
                return Err(format!(
                    "unknown generated block {name:?} in the target document \
                     (known: {:?})",
                    blocks.iter().map(|(n, _)| *n).collect::<Vec<_>>()
                ));
            };
            seen.push(
                blocks
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(n, _)| *n)
                    .unwrap_or(name),
            );
            // Skip the stale body up to (and including) the end marker.
            let end = end_marker(name);
            let mut terminated = false;
            for inner in lines.by_ref() {
                if inner.trim() == end {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(format!("block {name:?} has no `{end}` marker"));
            }
            out.push_str(&begin_marker(name));
            out.push('\n');
            out.push_str(content);
            out.push_str(&end);
            out.push('\n');
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    for (name, _) in blocks {
        if !seen.contains(name) {
            return Err(format!(
                "generated block {name:?} not found in the target document — \
                 add the `gen:begin {name}` / `gen:end {name}` markers"
            ));
        }
    }
    Ok(out)
}

/// Parse the canon text and render the final document.
///
/// # Errors
///
/// Propagates canon-parse, rendering and injection failures.
pub fn regenerate(canon_text: &str, document: &str) -> Result<String, String> {
    let events = parse_canon(canon_text, CANON_RELATIVE)?;
    let blocks = render_blocks(&events)?;
    inject(document, &blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(line: &str) -> Vec<CanonEvent> {
        parse_canon(line, "test").unwrap()
    }

    #[test]
    fn family_classifies_the_live_canon_shapes() {
        assert_eq!(family("/v1/molecules/{id}/result").unwrap(), "molecule");
        assert_eq!(family("/v1/molecules/{id}/artifacts").unwrap(), "artifact");
        assert_eq!(family("/v1/auth/claude/start").unwrap(), "auth-claude");
        assert_eq!(family("/v1/auth/me").unwrap(), "observ.");
        assert_eq!(family("/v1/molecules/{id}/logs").unwrap(), "observ.");
        assert_eq!(family("/v1/avatar/converse").unwrap(), "avatar-canal");
        assert_eq!(family("/v1/admin/habilitations").unwrap(), "admin");
        assert_eq!(family("/v1/admin/reload").unwrap(), "admin");
        assert_eq!(
            family("/v1/avatar/{instance_id}/status").unwrap(),
            "avatar-life"
        );
        assert!(family("/v2/surprise").is_err());
    }

    #[test]
    fn routes_block_counts_are_computed() {
        let events = ev(
            "GET /v1/molecules | m | 2026-06-10 | tenant | cosmon:molecule:read | tenant-verb | List\n\
             POST /v1/molecules/{id}/tackle | m | 2026-06-10 | tenant | cosmon:molecule:write+cosmon:worker:spawn | tenant-verb | Tackle",
        );
        let block = render_routes_block(&events).unwrap();
        assert!(block.contains("**2 routes `/v1/` gelées**"), "{block}");
        assert!(block.contains("= **2**"), "{block}");
        assert!(block.contains("`[coûteux]`"), "{block}");
    }

    #[test]
    fn bijection_block_derives_from_exposure() {
        let events = ev(
            "GET /v1/molecules | m | 2026-06-10 | tenant | cosmon:molecule:read | tenant-verb | List\n\
             GET /v1/quota | m | 2026-06-10 | tenant | cosmon:molecule:read | adapter-only | Quota",
        );
        let block = render_bijection_block(&events).unwrap();
        assert!(block.contains("✅ liée"), "{block}");
        assert!(block.contains("⊘ exempte"), "{block}");
        assert!(block.contains("Bijection liée : **1**"), "{block}");
        assert!(block.contains("Exemptes : **1**"), "{block}");
    }

    #[test]
    fn inject_replaces_only_marked_regions() {
        let doc = format!(
            "prose avant\n{}\nstale\n{}\nprose après\n{}\nstale2\n{}\nfin\n",
            begin_marker("routes-v1"),
            end_marker("routes-v1"),
            begin_marker("bijection-8p"),
            end_marker("bijection-8p"),
        );
        let blocks = vec![
            ("routes-v1", "NOUVEAU-A\n".to_owned()),
            ("bijection-8p", "NOUVEAU-B\n".to_owned()),
        ];
        let out = inject(&doc, &blocks).unwrap();
        assert!(out.contains("prose avant\n"));
        assert!(out.contains("NOUVEAU-A\n"));
        assert!(out.contains("NOUVEAU-B\n"));
        assert!(out.contains("prose après\n"));
        assert!(!out.contains("stale"));
        // Idempotent: re-injecting yields the same bytes.
        assert_eq!(inject(&out, &blocks).unwrap(), out);
    }

    #[test]
    fn inject_refuses_a_missing_block() {
        let doc = "no markers here\n";
        let blocks = vec![("routes-v1", String::new())];
        assert!(inject(doc, &blocks).unwrap_err().contains("routes-v1"));
    }
}
