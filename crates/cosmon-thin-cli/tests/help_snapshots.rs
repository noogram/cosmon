// SPDX-License-Identifier: Apache-2.0

//! Snapshot tests for `cs-thin help` and friends.
//!
//! The structured help is the
//! first thing an external auditor (operator-demo, tenant-demo) sees when they
//! type `cs-thin help`. We pin the output with `insta` so that an
//! accidental rewording, dropped section, or off-by-one indent in the
//! help renderer is caught at CI time rather than in a partner
//! screenshot.
//!
//! These snapshots are **stable** by design — they capture compile-time
//! data (the link-time verb registry + the `OPERATOR_ONLY` static list).
//! They do not contact the network and do not depend on the rpp-adapter.
//! When the help text intentionally changes, run `cargo insta review`
//! to accept the new snapshot in the same PR.

use cosmon_thin_cli::cli::{run_with, Cli, Command, HelpArgs, VerbsArgs};
use insta::assert_snapshot;

fn drive(cli: Cli) -> String {
    let mut out = Vec::new();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(async {
        run_with(cli, &mut out)
            .await
            .expect("help dispatch should succeed");
    });
    String::from_utf8(out).expect("help output is UTF-8")
}

#[test]
fn help_root() {
    let cli = Cli {
        base_url: None,
        jwt_from_env: None,
        jwt_file: None,
        coverage_report: false,
        json: false,
        command: Some(Command::Help(HelpArgs { command: None })),
    };
    let body = drive(cli);
    assert_snapshot!("help_root", body);
}

#[test]
fn help_root_via_render_function() {
    // Direct call to the renderer — same bytes as the CLI dispatch
    // path, but lets us assert the renderer is the source of truth
    // (no per-call drift, no surprise prefix from clap).
    let body = cosmon_thin_cli::help::render_root_help();
    assert_snapshot!("help_root_render", body);
}

#[test]
fn verbs_no_check_lists_link_time_slice() {
    // `cs-thin verbs` (no flag) — original "list every registered
    // route" affordance, useful for shell-grep against the link-time
    // slice. Pinned so a refactor of the registry order or formatting
    // is caught.
    let cli = Cli {
        base_url: None,
        jwt_from_env: None,
        jwt_file: None,
        coverage_report: false,
        json: false,
        command: Some(Command::Verbs(VerbsArgs {
            check: false,
            json: false,
        })),
    };
    let body = drive(cli);
    assert_snapshot!("verbs_no_check", body);
}

#[test]
fn verbs_check_renders_coverage_report() {
    // `cs-thin verbs --check` — the screenshot form. Pinned so the
    // ✓ / ⚠ glyphs, column widths, and ADR pointer text stay byte
    // stable across releases.
    let cli = Cli {
        base_url: None,
        jwt_from_env: None,
        jwt_file: None,
        coverage_report: false,
        json: false,
        command: Some(Command::Verbs(VerbsArgs {
            check: true,
            json: false,
        })),
    };
    let body = drive(cli);
    assert_snapshot!("verbs_check", body);
}
