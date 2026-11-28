// SPDX-License-Identifier: AGPL-3.0-only

//! `cs agy-pane` — internal pane driver of an agy worker (issue #240).
//!
//! `cs tackle --adapter agy` runs this in the worker's tmux pane; nobody types
//! it. It keeps agy's session open in the pane, forwards each line typed or
//! whispered into the pane as the next turn, appends agy's raw `stream-json`
//! output to `<mol>/agy-stream.jsonl` and shows it readably, then exits with
//! agy's status. The behaviour lives in [`cosmon_transport::agy_pane`].

use std::io::IsTerminal as _;
use std::path::PathBuf;

/// Launch inputs, built by `cosmon_transport::agy::build_agy_command`.
#[derive(clap::Args)]
pub struct Args {
    /// Canonical molecule directory: receives the receipt and the exit code.
    #[arg(long)]
    pub molecule_dir: PathBuf,
    /// The brief, sent as the session's first message.
    #[arg(long)]
    pub prompt: Option<String>,
    /// The agy executable followed by its arguments, after `--`.
    #[arg(last = true, required = true)]
    pub command: Vec<String>,
}

/// Bracketed-paste mode on: a whisper pasted with tmux's `-p` then arrives
/// between markers and stays one message, line breaks included.
const BRACKETED_PASTE_ON: &str = "\u{1b}[?2004h";

/// Run the session and exit with agy's status.
///
/// The process exits here, rather than returning, because the thread reading
/// the terminal is still blocked on it when agy ends.
pub fn run(args: &Args) -> anyhow::Result<()> {
    let (program, arguments) = args
        .command
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("agy-pane: no agy command after `--`"))?;
    let config = cosmon_transport::agy_pane::AgyPaneConfig {
        molecule_dir: args.molecule_dir.clone(),
        prompt: args.prompt.clone().unwrap_or_default(),
        program: program.clone(),
        arguments: arguments.to_vec(),
    };
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        print!("{BRACKETED_PASTE_ON}");
    }
    let code = cosmon_transport::agy_pane::run(&config, stdin, std::io::stdout())
        .map_err(|e| anyhow::anyhow!("agy-pane: {e}"))?;
    std::process::exit(code)
}
