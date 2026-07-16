// SPDX-License-Identifier: AGPL-3.0-only

//! `cs topology` — structural topology maps for the workspace.
//!
//! Thin façade over the in-tree [`topon_core`] crate, re-exposing its three
//! views — `map`, `outline`, and `symbols` — under the `cs` umbrella so
//! workers and humans can introspect the codebase without leaving the
//! cosmon vocabulary.
//!
//! `topon_core` does the heavy lifting (tree-sitter parsing, symbol
//! extraction, `PageRank`). This module just parses arguments, calls the
//! library, and renders results — markdown by default, JSON when the
//! global `--json` flag is set.
//!
//! ## Architectural fit
//!
//! - **Stateless**: read-only filesystem walk, no `.cosmon/` state touched.
//! - **Idempotent**: pure projection, twice = once.
//! - **Regime-agnostic**: introspection only — no molecules created or
//!   advanced. Safe to call from any regime (Inert / Propelled / Autonomous).
//! - **Single perimeter**: code topology only. Does not duplicate
//!   `cs status` (project pulse) or `cs ensemble` (fleet view).

use std::path::{Path, PathBuf};

use topon_core::project::{map_project, outline_file, search_symbols};

use super::Context;

/// Arguments for the `cs topology` subcommand group.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: TopologyCommand,
}

/// Subcommands mirroring the `topon` CLI surface.
#[derive(clap::Subcommand)]
pub enum TopologyCommand {
    /// PageRank-ranked structural map of a Rust project.
    Map(MapArgs),

    /// Symbol outline of a single Rust file.
    Outline(OutlineArgs),

    /// Search symbols by name across a Rust project.
    Symbols(SymbolsArgs),
}

/// Arguments for `cs topology map`.
#[derive(clap::Args)]
pub struct MapArgs {
    /// Path to the project root (defaults to current directory).
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Maximum symbols per module (0 = unlimited).
    #[arg(long, default_value_t = 0)]
    pub max_symbols: usize,
}

/// Arguments for `cs topology outline`.
#[derive(clap::Args)]
pub struct OutlineArgs {
    /// Path to the `.rs` file.
    pub file: PathBuf,
}

/// Arguments for `cs topology symbols`.
///
/// Mirrors `topon symbols <PATH> <QUERY>`: both arguments are positional
/// and required, in that order, so the cosmon wrapper does not silently
/// re-order what users learned from `topon --help`.
#[derive(clap::Args)]
pub struct SymbolsArgs {
    /// Path to the project root.
    pub path: PathBuf,

    /// Search query (case-insensitive substring match).
    pub query: String,
}

/// Execute the `cs topology` command.
///
/// Dispatches to the matching `topon_core` entry point and renders the
/// result. Honors the global `--json` flag for the `map` view (the only
/// view with a structured representation rich enough to warrant JSON);
/// `outline` and `symbols` always return text since they are already
/// human-oriented.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        TopologyCommand::Map(map_args) => run_map(ctx, map_args),
        TopologyCommand::Outline(outline_args) => run_outline(outline_args),
        TopologyCommand::Symbols(symbols_args) => run_symbols(ctx, symbols_args),
    }
}

#[allow(clippy::similar_names)]
fn run_map(ctx: &Context, args: &MapArgs) -> anyhow::Result<()> {
    let max = if args.max_symbols == 0 {
        None
    } else {
        Some(args.max_symbols)
    };
    let map = map_project(&args.path, max)
        .map_err(|e| anyhow::anyhow!("topon map failed: {e}"))
        .with_path(&args.path)?;

    if ctx.json {
        let json = map
            .to_json()
            .map_err(|e| anyhow::anyhow!("failed to serialize map as JSON: {e}"))?;
        println!("{json}");
    } else {
        // 0 = render every symbol the library kept after our `max` filter.
        print!("{}", map.to_markdown(0));
    }
    Ok(())
}

fn run_outline(args: &OutlineArgs) -> anyhow::Result<()> {
    let outline = outline_file(&args.file)
        .map_err(|e| anyhow::anyhow!("topon outline failed: {e}"))
        .with_path(&args.file)?;
    print!("{outline}");
    Ok(())
}

fn run_symbols(ctx: &Context, args: &SymbolsArgs) -> anyhow::Result<()> {
    let symbols = search_symbols(&args.path, &args.query)
        .map_err(|e| anyhow::anyhow!("topon symbols failed: {e}"))
        .with_path(&args.path)?;

    if ctx.json {
        let json = serde_json::to_string_pretty(&symbols)?;
        println!("{json}");
        return Ok(());
    }

    if symbols.is_empty() {
        println!("(no symbols matching {:?})", args.query);
        return Ok(());
    }

    for sym in &symbols {
        let vis = if sym.is_public { "pub " } else { "" };
        println!(
            "  {vis}{} {} ({}:L{}–L{})",
            sym.kind,
            sym.name,
            sym.file.display(),
            sym.span.start_line + 1,
            sym.span.end_line + 1,
        );
    }
    Ok(())
}

/// Tiny extension trait that attaches the offending path to topon errors,
/// since [`topon_core::error::CfsError`] does not always carry it itself.
trait WithPath<T> {
    fn with_path(self, path: &Path) -> anyhow::Result<T>;
}

impl<T> WithPath<T> for anyhow::Result<T> {
    fn with_path(self, path: &Path) -> anyhow::Result<T> {
        self.map_err(|e| e.context(format!("path: {}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tempfile::TempDir;

    /// Build a tiny self-contained Rust project for testing topon-core
    /// behavior end-to-end through our wrapper.
    fn make_project() -> TempDir {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("lib.rs"),
            r"
//! Toy crate for tests.

/// A toy struct used by the topology smoke test.
pub struct Widget;

impl Widget {
    /// Construct a new widget.
    pub fn new() -> Self {
        Self
    }
}

/// A free function that calls Widget::new.
pub fn make_widget() -> Widget {
    Widget::new()
}
",
        )
        .unwrap();
        dir
    }

    fn ctx(json: bool) -> Context {
        Context {
            verbose: false,
            json,
            config: None,
        }
    }

    #[test]
    fn map_renders_markdown_for_real_project() {
        let dir = make_project();
        let args = MapArgs {
            path: dir.path().to_path_buf(),
            max_symbols: 0,
        };
        // Should succeed and not panic.
        run_map(&ctx(false), &args).expect("map should succeed");
    }

    #[test]
    fn map_emits_json_when_flag_set() {
        let dir = make_project();
        let args = MapArgs {
            path: dir.path().to_path_buf(),
            max_symbols: 5,
        };
        run_map(&ctx(true), &args).expect("json map should succeed");
    }

    #[test]
    fn outline_reads_a_single_file() {
        let dir = make_project();
        let file = dir.path().join("src").join("lib.rs");
        let args = OutlineArgs { file };
        run_outline(&args).expect("outline should succeed");
    }

    #[test]
    fn symbols_finds_known_name() {
        let dir = make_project();
        let args = SymbolsArgs {
            path: dir.path().to_path_buf(),
            query: "Widget".to_owned(),
        };
        run_symbols(&ctx(false), &args).expect("symbols should succeed");
    }

    #[test]
    fn symbols_handles_empty_match_set() {
        let dir = make_project();
        let args = SymbolsArgs {
            path: dir.path().to_path_buf(),
            query: "ZZNoSuchSymbol".to_owned(),
        };
        run_symbols(&ctx(false), &args).expect("empty match should not error");
    }

    #[test]
    fn outline_propagates_error_for_missing_file() {
        let args = OutlineArgs {
            file: PathBuf::from("/nonexistent/cosmon/topology-test.rs"),
        };
        let err = run_outline(&args).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("topon outline failed") || msg.contains("path:"));
    }

    /// Args parsing: `map` accepts a path with default `.` and a
    /// `--max-symbols` flag.
    #[test]
    fn map_args_parse() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrapper {
            #[command(subcommand)]
            cmd: TopologyCommand,
        }

        let parsed = Wrapper::try_parse_from(["topology", "map"]).unwrap();
        match parsed.cmd {
            TopologyCommand::Map(m) => {
                assert_eq!(m.path, PathBuf::from("."));
                assert_eq!(m.max_symbols, 0);
            }
            _ => panic!("expected map"),
        }

        let parsed =
            Wrapper::try_parse_from(["topology", "map", "crates/cosmon-cli", "--max-symbols", "5"])
                .unwrap();
        match parsed.cmd {
            TopologyCommand::Map(m) => {
                assert_eq!(m.path, PathBuf::from("crates/cosmon-cli"));
                assert_eq!(m.max_symbols, 5);
            }
            _ => panic!("expected map"),
        }
    }

    /// `outline` requires a file argument; `symbols` requires both
    /// path and query (mirrors topon).
    #[test]
    fn outline_and_symbols_args() {
        use clap::Parser;

        #[derive(clap::Parser)]
        struct Wrapper {
            #[command(subcommand)]
            cmd: TopologyCommand,
        }

        let parsed = Wrapper::try_parse_from(["topology", "outline", "src/main.rs"]).unwrap();
        match parsed.cmd {
            TopologyCommand::Outline(o) => assert_eq!(o.file, PathBuf::from("src/main.rs")),
            _ => panic!("expected outline"),
        }

        // Symbols requires BOTH PATH and QUERY (mirrors topon).
        assert!(
            Wrapper::try_parse_from(["topology", "symbols", "evolve"]).is_err(),
            "symbols requires both PATH and QUERY"
        );

        let parsed =
            Wrapper::try_parse_from(["topology", "symbols", "crates/cosmon-core", "evolve"])
                .unwrap();
        match parsed.cmd {
            TopologyCommand::Symbols(s) => {
                assert_eq!(s.path, PathBuf::from("crates/cosmon-core"));
                assert_eq!(s.query, "evolve");
            }
            _ => panic!("expected symbols"),
        }
    }
}
