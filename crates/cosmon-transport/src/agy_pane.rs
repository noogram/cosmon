// SPDX-License-Identifier: AGPL-3.0-only

//! The agy pane driver: an interactive, readable agy worker that keeps its
//! `stream-json` receipt (issue #240).
//!
//! # Why a driver, and not `agy -i`
//!
//! agy's interactive TUI (`-i`) is readable and accepts typed input, but it
//! records the session's token usage only inside its own conversation store,
//! as unnamed protobuf fields, and nowhere a cosmon reader can rely on. The
//! receipt `<mol>/agy-stream.jsonl` is what `cs peek` reads for usage and for a
//! quota stop (#241), so trading it for the TUI would lose both.
//!
//! agy's print mode has a documented multi-turn form instead:
//! `--input-format stream-json` reads one NDJSON message per line on stdin and
//! runs a turn for each, emitting the same `stream-json` frames as the
//! one-shot launch. Measured on agy 1.5.19: one `init`, then one `result` per
//! turn whose `usage` is cumulative over the session, and the process exits
//! when stdin closes or a turn fails (a quota refusal exits 3). So the receipt
//! keeps its meaning turn after turn, and the pane still dies when agy does.
//!
//! This driver is the small program that sits in the pane between the
//! terminal and that mode:
//!
//! - the brief is the first message; each line typed into the pane, or pasted
//!   by `cs whisper`, becomes the next one (a bracketed paste stays one
//!   message, blank lines are dropped because agy rejects an empty message by
//!   ending the session);
//! - every line agy writes, stdout and stderr, is appended verbatim to the
//!   receipt and shown in the pane through [`render_frame`], so the pane is
//!   readable while the receipt stays raw;
//! - agy's exit status is written to `agy-exit.code` and returned, so the
//!   pane's exit status is agy's.
//!
//! The renderer is display only. Nothing reads the pane to learn usage or
//! status; those come from the receipt.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde_json::Value;

/// File name of the append-only `stream-json` receipt in the molecule
/// directory. Shared with the readers of #241.
pub const RECEIPT_FILE: &str = "agy-stream.jsonl";

/// File name of agy's recorded exit status in the molecule directory.
pub const EXIT_CODE_FILE: &str = "agy-exit.code";

/// Prefix of the line the driver prints when agy's `init` frame arrives.
///
/// It is the readiness evidence `cs tackle` waits for: the line exists only
/// once agy itself has started a session, never from the pane merely existing.
pub const READY_PREFIX: &str = "agy session ";

/// Inputs of one driven agy session.
#[derive(Debug, Clone)]
pub struct AgyPaneConfig {
    /// Canonical molecule directory, which holds the receipt and the exit
    /// code. It outlives the worktree.
    pub molecule_dir: PathBuf,
    /// The brief, sent as the session's first message.
    pub prompt: String,
    /// The agy executable.
    pub program: String,
    /// agy's arguments, verbatim.
    pub arguments: Vec<String>,
}

/// Errors that stop the driver before agy's exit status is known.
#[derive(Debug, thiserror::Error)]
pub enum AgyPaneError {
    /// The receipt or the exit-code file could not be written.
    #[error("molecule directory: {0}")]
    MoleculeDir(String),
    /// agy could not be started.
    #[error("cannot start agy: {0}")]
    Spawn(String),
    /// Waiting for agy failed.
    #[error("waiting for agy: {0}")]
    Wait(String),
}

/// Encode one user message as agy's `stream-json` input line, newline
/// included. `None` for a message that is blank once trimmed: agy answers an
/// empty message with an error that ends the session, and `cs whisper` presses
/// Enter on an empty composer more than once.
#[must_use]
pub fn user_message_line(text: &str) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    let frame = serde_json::json!({ "event": "user", "message": { "content": text } });
    Some(format!("{frame}\n"))
}

/// Start of a bracketed paste, as the terminal sends it once the driver has
/// enabled bracketed-paste mode.
const PASTE_START: &str = "\u{1b}[200~";
/// End of a bracketed paste.
const PASTE_END: &str = "\u{1b}[201~";

/// Groups terminal lines into messages.
///
/// A plain line is one message. A bracketed paste (`cs whisper` pastes with
/// tmux's `-p`) may span several lines; it becomes one message, with its line
/// breaks kept, so a multi-line whisper is one turn and not one turn per line.
#[derive(Debug, Default)]
pub struct MessageAssembler {
    paste: Option<String>,
}

impl MessageAssembler {
    /// Feed one terminal line (without its line terminator). Returns a
    /// complete message when the line ends one.
    pub fn push(&mut self, line: &str) -> Option<String> {
        let line = line.trim_end_matches('\r');
        let (mut buffer, rest) = match self.paste.take() {
            Some(buffer) => (buffer, line),
            None => match line.find(PASTE_START) {
                Some(at) => (line[..at].to_owned(), &line[at + PASTE_START.len()..]),
                None => return Some(line.to_owned()),
            },
        };
        if let Some(end) = rest.find(PASTE_END) {
            buffer.push_str(&rest[..end]);
            buffer.push_str(&rest[end + PASTE_END.len()..]);
            return Some(buffer);
        }
        buffer.push_str(rest);
        buffer.push('\n');
        self.paste = Some(buffer);
        None
    }
}

/// What the terminal produced: a finished line, or the end of input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalInput {
    /// A line, without its terminator. Bracketed-paste markers are kept for
    /// [`MessageAssembler`].
    Line(String),
    /// Ctrl-D on an empty line: the operator ends the session.
    End,
}

/// Assembles terminal bytes into lines, with the little editing a prompt
/// needs, when the driver has taken the terminal out of canonical mode.
///
/// Canonical mode would do this itself, but it caps one input line at the
/// system's `MAX_CANON` (1024 bytes on macOS); a longer typed or whispered line
/// never reached agy at all (measured). So the driver reads bytes and edits
/// here, with no length limit: printable bytes are kept and echoed, Backspace
/// removes the last character, Ctrl-U clears the line (`cs whisper` sends it
/// before pasting), Ctrl-D on an empty line ends input, Enter ends the line.
/// The bracketed-paste markers are kept, other escape sequences (arrow keys)
/// and other control bytes are dropped.
#[derive(Debug, Default)]
pub struct LineEditor {
    line: Vec<u8>,
    escape: Option<Vec<u8>>,
}

impl LineEditor {
    /// Feed one byte. Echo for the terminal is appended to `echo`.
    pub fn push(&mut self, byte: u8, echo: &mut Vec<u8>) -> Option<TerminalInput> {
        if let Some(mut sequence) = self.escape.take() {
            sequence.push(byte);
            let complete = match sequence.as_slice() {
                [_, b'['] => false,
                [_, _] => true,
                [_, b'[', .., last] => (0x40..=0x7e).contains(last),
                _ => false,
            };
            if !complete {
                self.escape = Some(sequence);
            } else if sequence == PASTE_START.as_bytes() || sequence == PASTE_END.as_bytes() {
                self.line.extend_from_slice(&sequence);
            }
            return None;
        }
        match byte {
            0x1b => self.escape = Some(vec![byte]),
            b'\r' | b'\n' => {
                echo.extend_from_slice(b"\r\n");
                let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                return Some(TerminalInput::Line(line));
            }
            0x7f | 0x08 => {
                while let Some(last) = self.line.pop() {
                    if last & 0xc0 != 0x80 {
                        echo.extend_from_slice(b"\x08 \x08");
                        break;
                    }
                }
            }
            0x15 => {
                if !self.line.is_empty() {
                    self.line.clear();
                    echo.extend_from_slice(b"\r\x1b[K");
                }
            }
            0x04 if self.line.is_empty() => return Some(TerminalInput::End),
            b'\t' => {
                self.line.push(byte);
                echo.push(byte);
            }
            byte if byte < 0x20 => {}
            byte => {
                self.line.push(byte);
                echo.push(byte);
            }
        }
        None
    }
}

/// Display state carried between frames: whether the cursor sits after
/// streamed agent text that has not yet ended its line.
#[derive(Debug, Default)]
pub struct RenderState {
    mid_line: bool,
}

/// Longest tool parameter or output excerpt shown on one pane line.
const EXCERPT_CHARS: usize = 120;

fn excerpt(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default().trim();
    let mut out: String = first.chars().take(EXCERPT_CHARS).collect();
    if first.chars().count() > EXCERPT_CHARS || text.trim().lines().nth(1).is_some() {
        out.push('…');
    }
    out
}

fn count(usage: &Value, key: &str) -> String {
    usage
        .get(key)
        .and_then(Value::as_u64)
        .map_or_else(|| "-".to_owned(), |n| n.to_string())
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// Turn one line of agy output into the text the pane shows for it.
///
/// Agent text streams as it arrives, tool calls show one line each, every
/// turn ends with its status and cumulative token counts, and a line that is
/// not a JSON frame (agy's stderr) is shown as written. Frames with nothing
/// for a reader (step bookkeeping, system messages) show nothing.
#[must_use]
pub fn render_frame(line: &str, state: &mut RenderState) -> String {
    let Ok(frame) = serde_json::from_str::<Value>(line) else {
        return block(state, line);
    };
    match frame.get("event").and_then(Value::as_str) {
        Some("init") => {
            let init = &frame["init"];
            let model = init
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("default");
            block(
                state,
                &format!(
                    "{READY_PREFIX}{} · model {model}",
                    str_field(&frame, "conversation_id")
                ),
            )
        }
        Some("step_update") => render_step(&frame["step_update"], state),
        Some("result") => {
            let result = &frame["result"];
            let usage = &result["usage"];
            let mut text = format!(
                "── turn {} {}",
                result
                    .get("num_turns")
                    .and_then(Value::as_u64)
                    .map_or_else(|| "?".to_owned(), |n| n.to_string()),
                str_field(result, "status")
            );
            let error = str_field(result, "error");
            if !error.is_empty() {
                text.push_str(": ");
                text.push_str(error);
            }
            let _ = write!(
                text,
                " · tokens in {} cache {} out {} thinking {}",
                count(usage, "input_tokens"),
                count(usage, "cache_read_tokens"),
                count(usage, "output_tokens"),
                count(usage, "thinking_tokens"),
            );
            block(state, &text)
        }
        _ => String::new(),
    }
}

fn render_step(step: &Value, state: &mut RenderState) -> String {
    let status = str_field(step, "state");
    match str_field(step, "step_type") {
        "agent_response" => {
            let delta = str_field(step, "text_delta");
            let mut out = delta.to_owned();
            if !delta.is_empty() {
                state.mid_line = !delta.ends_with('\n');
            }
            if status == "DONE" && state.mid_line {
                out.push('\n');
                state.mid_line = false;
            }
            out
        }
        "tool" => {
            let name = step
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or_else(|| str_field(&step["tool_info"], "name"));
            let info = &step["tool_info"];
            match status {
                "ACTIVE" => {
                    let parameter = info
                        .get("parameters")
                        .and_then(Value::as_object)
                        .and_then(|p| p.values().find_map(Value::as_str))
                        .map(excerpt)
                        .unwrap_or_default();
                    block(state, format!("→ {name} {parameter}").trim_end())
                }
                "DONE" => {
                    let output = excerpt(str_field(info, "output"));
                    block(state, format!("  ✓ {name} {output}").trim_end())
                }
                "ERROR" => block(state, &format!("  ✗ {name}")),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// A line of its own, after ending any streamed text still on the cursor line.
fn block(state: &mut RenderState, text: &str) -> String {
    let lead = if state.mid_line { "\n" } else { "" };
    state.mid_line = false;
    format!("{lead}{text}\n")
}

/// Take the terminal `fd` (the pane: stdin) out of canonical mode and its
/// own echo, so the driver edits lines itself with no length limit
/// ([`LineEditor`]). Signals (Ctrl-C) stay with the terminal. Returns the
/// settings to restore, or `None` when `fd` is not a terminal or refuses the
/// change.
#[cfg(unix)]
#[must_use]
pub fn take_terminal(fd: impl std::os::fd::AsFd) -> Option<nix::sys::termios::Termios> {
    use nix::sys::termios::{tcgetattr, tcsetattr, LocalFlags, SetArg, SpecialCharacterIndices};
    let saved = tcgetattr(&fd).ok()?;
    let mut edited = saved.clone();
    edited
        .local_flags
        .remove(LocalFlags::ICANON | LocalFlags::ECHO);
    edited.control_chars[SpecialCharacterIndices::VMIN as usize] = 1;
    edited.control_chars[SpecialCharacterIndices::VTIME as usize] = 0;
    tcsetattr(&fd, SetArg::TCSANOW, &edited).ok()?;
    Some(saved)
}

/// Put back on `fd` the terminal settings [`take_terminal`] saved, so a
/// terminal the driver ran in is left as it was found.
#[cfg(unix)]
pub fn restore_terminal(fd: impl std::os::fd::AsFd, saved: &nix::sys::termios::Termios) {
    let _ = nix::sys::termios::tcsetattr(fd, nix::sys::termios::SetArg::TCSANOW, saved);
}

/// Exit status of a finished child, as a shell would report it: the code, or
/// 128 plus the signal number when a signal ended it.
fn exit_status_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status.code().unwrap_or(1)
}

/// Run one agy session in the current terminal and return agy's exit status.
///
/// `input` is the terminal (the pane), `output` the pane's display, and
/// `echo` receives the typed characters when the caller has turned the
/// terminal's own echo off (see [`LineEditor`]). The session stays open after
/// the brief's turn: it ends when `input` reaches
/// end of file, which closes agy's stdin, or when agy exits on its own (a
/// failed turn). `input` is read on a thread that is abandoned when agy
/// exits, so a caller should exit the process with the returned status rather
/// than wait on the terminal.
///
/// # Errors
///
/// Returns [`AgyPaneError`] when the receipt cannot be opened, agy cannot be
/// started or waited for, or the exit code cannot be recorded.
pub fn run(
    config: &AgyPaneConfig,
    input: impl Read + Send + 'static,
    output: impl Write + Send + 'static,
    echo: Option<Box<dyn Write + Send>>,
) -> Result<i32, AgyPaneError> {
    let exit_file = config.molecule_dir.join(EXIT_CODE_FILE);
    match std::fs::remove_file(&exit_file) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(AgyPaneError::MoleculeDir(e.to_string())),
    }
    let receipt = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.molecule_dir.join(RECEIPT_FILE))
        .map_err(|e| AgyPaneError::MoleculeDir(e.to_string()))?;

    let mut child = Command::new(&config.program)
        .args(&config.arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| AgyPaneError::Spawn(format!("{}: {e}", config.program)))?;

    let sink = Arc::new(Mutex::new(Sink {
        receipt,
        output: Box::new(output),
        state: RenderState::default(),
    }));
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let sink = Arc::clone(&sink);
        std::thread::spawn(move || forward(stream, &sink))
    })
    .collect();

    if let Some(mut stdin) = child.stdin.take() {
        let first = user_message_line(&config.prompt);
        let sent = first.is_none_or(|line| stdin.write_all(line.as_bytes()).is_ok());
        if sent {
            std::thread::spawn(move || feed(input, stdin, echo));
        }
    }

    let status = child
        .wait()
        .map_err(|e| AgyPaneError::Wait(e.to_string()))?;
    for reader in readers {
        let _ = reader.join();
    }
    let code = exit_status_code(status);
    std::fs::write(&exit_file, format!("{code}\n"))
        .map_err(|e| AgyPaneError::MoleculeDir(e.to_string()))?;
    Ok(code)
}

struct Sink {
    receipt: std::fs::File,
    output: Box<dyn Write + Send>,
    state: RenderState,
}

/// Copy one of agy's output streams, line by line, into the receipt and the
/// pane. A line is written to the receipt whole, under the lock, so stdout and
/// stderr interleave by line and never inside one.
fn forward(stream: Box<dyn Read + Send>, sink: &Mutex<Sink>) {
    let mut reader = BufReader::new(stream);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if !raw.ends_with(b"\n") {
            raw.push(b'\n');
        }
        let Ok(mut sink) = sink.lock() else { return };
        let _ = sink.receipt.write_all(&raw);
        let line = String::from_utf8_lossy(&raw);
        let shown = {
            let Sink { state, .. } = &mut *sink;
            render_frame(line.trim_end_matches(['\n', '\r']), state)
        };
        let _ = sink.output.write_all(shown.as_bytes());
        let _ = sink.output.flush();
    }
}

/// Forward terminal lines to agy as messages until the terminal closes or the
/// operator presses Ctrl-D, then close agy's stdin so the session ends after
/// its current turn.
fn feed(
    input: impl Read,
    mut stdin: std::process::ChildStdin,
    mut echo: Option<Box<dyn Write + Send>>,
) {
    let mut editor = LineEditor::default();
    let mut assembler = MessageAssembler::default();
    let mut buffer = [0_u8; 4096];
    let mut input = input;
    let mut shown = Vec::new();
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for &byte in &buffer[..read] {
            let event = editor.push(byte, &mut shown);
            if let Some(echo) = echo.as_mut() {
                if !shown.is_empty() {
                    let _ = echo.write_all(&shown);
                    let _ = echo.flush();
                }
            }
            shown.clear();
            let line = match event {
                None => continue,
                Some(TerminalInput::End) => return,
                Some(TerminalInput::Line(line)) => line,
            };
            let Some(message) = assembler.push(&line) else {
                continue;
            };
            let Some(encoded) = user_message_line(&message) else {
                continue;
            };
            // `ChildStdin` is unbuffered: a written message is already agy's.
            if stdin.write_all(encoded.as_bytes()).is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_agys_stream_json_user_frame() {
        let line = user_message_line("say \"hi\"\nthen \\ stop").expect("non-blank");
        assert!(line.ends_with('\n'));
        let frame: Value = serde_json::from_str(line.trim_end()).expect("one JSON line");
        assert_eq!(frame["event"], "user");
        assert_eq!(frame["message"]["content"], "say \"hi\"\nthen \\ stop");
    }

    #[test]
    fn a_blank_message_is_never_sent() {
        assert_eq!(user_message_line(""), None);
        assert_eq!(user_message_line("  \t "), None);
    }

    #[test]
    fn a_plain_line_is_one_message() {
        let mut a = MessageAssembler::default();
        assert_eq!(a.push("hello\r").as_deref(), Some("hello"));
        assert_eq!(a.push("").as_deref(), Some(""));
    }

    #[test]
    fn a_bracketed_paste_over_several_lines_is_one_message() {
        let mut a = MessageAssembler::default();
        assert_eq!(a.push("\u{1b}[200~first line"), None);
        assert_eq!(a.push("second line"), None);
        assert_eq!(
            a.push("third\u{1b}[201~").as_deref(),
            Some("first line\nsecond line\nthird")
        );
        assert_eq!(a.push("after").as_deref(), Some("after"));
    }

    #[test]
    fn a_one_line_bracketed_paste_keeps_text_around_the_markers() {
        let mut a = MessageAssembler::default();
        assert_eq!(
            a.push("typed \u{1b}[200~pasted\u{1b}[201~ more").as_deref(),
            Some("typed pasted more")
        );
    }

    fn type_bytes(bytes: &[u8]) -> (Vec<TerminalInput>, Vec<u8>) {
        let mut editor = LineEditor::default();
        let mut echo = Vec::new();
        let inputs = bytes
            .iter()
            .filter_map(|b| editor.push(*b, &mut echo))
            .collect();
        (inputs, echo)
    }

    fn line(text: &str) -> TerminalInput {
        TerminalInput::Line(text.to_owned())
    }

    #[test]
    fn a_line_longer_than_the_canonical_limit_is_kept_whole() {
        let mut long = vec![b'a'; 5000];
        long.push(b'\r');
        let (inputs, echo) = type_bytes(&long);
        assert_eq!(inputs, vec![line(&"a".repeat(5000))]);
        assert!(echo.ends_with(b"a\r\n"));
    }

    #[test]
    fn backspace_removes_one_character_even_a_multibyte_one() {
        let (inputs, echo) = type_bytes("abé\x7f\x7fc\x08d\n".as_bytes());
        assert_eq!(inputs, vec![line("ad")]);
        assert_eq!(echo.windows(3).filter(|w| w == b"\x08 \x08").count(), 3);
        let (empty, quiet) = type_bytes(b"\x7f\n");
        assert_eq!(empty, vec![line("")]);
        assert_eq!(quiet, b"\r\n");
    }

    #[test]
    fn ctrl_u_clears_the_line_and_ctrl_d_ends_only_an_empty_line() {
        let (inputs, echo) = type_bytes(b"junk\x15ok\r\x15x\x04\x7f\x04");
        assert_eq!(inputs, vec![line("ok"), TerminalInput::End]);
        // Only the first Ctrl-U had something to clear.
        assert_eq!(echo.windows(4).filter(|w| w == b"\r\x1b[K").count(), 1);
    }

    #[test]
    fn paste_markers_are_kept_and_other_escapes_and_controls_dropped() {
        let (inputs, echo) = type_bytes(b"\x1b[200~one\rtwo\x1b[201~\r\x1b[Aup\x1bx\x07\tz\r");
        assert_eq!(
            inputs,
            vec![line("\x1b[200~one"), line("two\x1b[201~"), line("up\tz")]
        );
        assert!(!echo.contains(&0x1b), "escape sequences are not echoed");
        let mut assembler = MessageAssembler::default();
        let messages: Vec<String> = inputs
            .iter()
            .filter_map(|i| match i {
                TerminalInput::Line(l) => assembler.push(l),
                TerminalInput::End => None,
            })
            .collect();
        assert_eq!(messages, ["one\ntwo", "up\tz"]);
    }

    fn render_all(lines: &[&str]) -> String {
        let mut state = RenderState::default();
        lines.iter().map(|l| render_frame(l, &mut state)).collect()
    }

    #[test]
    fn init_shows_the_session_and_model_as_readiness_evidence() {
        let out = render_all(&[
            r#"{"event":"init","conversation_id":"c-1","init":{"model":"m-low","tools":["a","b"]}}"#,
        ]);
        assert_eq!(out, "agy session c-1 · model m-low\n");
        assert!(out.starts_with(READY_PREFIX));
    }

    #[test]
    fn init_without_a_model_says_default() {
        let out = render_all(&[r#"{"event":"init","conversation_id":"c-2","init":{}}"#]);
        assert_eq!(out, "agy session c-2 · model default\n");
    }

    #[test]
    fn streamed_text_joins_and_ends_its_line_before_the_next_block() {
        let out = render_all(&[
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"Hel"}}"#,
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"lo"}}"#,
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","parameters":{"Path":"/w/a.rs"}}}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"tool","tool_name":"read_file","tool_info":{"name":"read_file","output":"3 lines"}}}"#,
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"Done"}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"agent_response","text_delta":""}}"#,
        ]);
        assert_eq!(
            out,
            "Hello\n→ read_file /w/a.rs\n  ✓ read_file 3 lines\nDone\n"
        );
    }

    #[test]
    fn a_text_delta_ending_in_a_newline_needs_no_extra_line() {
        let out = render_all(&[
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"OK"}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"agent_response","text_delta":"\n"}}"#,
        ]);
        assert_eq!(out, "OK\n");
    }

    #[test]
    fn a_failed_tool_and_bookkeeping_frames() {
        let out = render_all(&[
            r#"{"event":"step_update","step_update":{"state":"ERROR","step_type":"tool","tool_info":{"name":"run_command"}}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"user_input"}}"#,
            r#"{"event":"step_update","step_update":{"state":"DONE","step_type":"system_message"}}"#,
            r#"{"event":"step_update","step_update":{"state":"PENDING","step_type":"tool","tool_name":"x"}}"#,
            r#"{"event":"other"}"#,
        ]);
        assert_eq!(out, "  ✗ run_command\n");
    }

    #[test]
    fn a_long_or_multiline_excerpt_is_cut_and_marked() {
        assert_eq!(excerpt("one\ntwo"), "one…");
        assert_eq!(excerpt("short"), "short");
        let long = "x".repeat(EXCERPT_CHARS + 5);
        let cut = excerpt(&long);
        assert_eq!(cut.chars().count(), EXCERPT_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(
            excerpt(&"y".repeat(EXCERPT_CHARS)),
            "y".repeat(EXCERPT_CHARS)
        );
    }

    #[test]
    fn a_result_shows_status_error_and_cumulative_counts() {
        let out = render_all(&[
            r#"{"event":"result","result":{"status":"ERROR","error":"Individual quota reached. Resets in 1h.","num_turns":2,"usage":{"input_tokens":10,"output_tokens":3,"thinking_tokens":0,"cache_read_tokens":7}}}"#,
        ]);
        assert_eq!(
            out,
            "── turn 2 ERROR: Individual quota reached. Resets in 1h. · tokens in 10 cache 7 out 3 thinking 0\n"
        );
        let unknown = render_all(&[r#"{"event":"result","result":{"status":"SUCCESS"}}"#]);
        assert_eq!(
            unknown,
            "── turn ? SUCCESS · tokens in - cache - out - thinking -\n"
        );
    }

    #[test]
    fn a_non_json_line_is_shown_as_written_on_its_own_line() {
        let out = render_all(&[
            r#"{"event":"step_update","step_update":{"state":"ACTIVE","step_type":"agent_response","text_delta":"partial"}}"#,
            "error: Individual quota reached.",
        ]);
        assert_eq!(out, "partial\nerror: Individual quota reached.\n");
    }

    /// A writer the test can read back after the driver is done with it.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Shared {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// A fake agy speaking the multi-turn print mode: init, one turn per
    /// stdin line (recorded), a stderr line when stdin closes, then `exit`.
    fn fake_agy(dir: &std::path::Path, exit: &str) -> AgyPaneConfig {
        let script = dir.join("agy");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 printf '%s\\n' '{{\"event\":\"init\",\"conversation_id\":\"c\",\"init\":{{}}}}'\n\
                 n=0\n\
                 while IFS= read -r line; do n=$((n+1)); printf '%s\\n' \"$line\" >> '{d}/stdin'\n\
                 printf '{{\"event\":\"result\",\"result\":{{\"status\":\"SUCCESS\",\"num_turns\":%d,\"usage\":{{\"input_tokens\":%d}}}}}}\\n' $n $n\n\
                 done\n\
                 echo 'closed' >&2\n\
                 {exit}\n",
                d = dir.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        AgyPaneConfig {
            molecule_dir: dir.to_path_buf(),
            prompt: "the brief".into(),
            program: script.to_string_lossy().into_owned(),
            arguments: vec![],
        }
    }

    fn sent(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("stdin"))
            .unwrap_or_default()
            .lines()
            .map(|l| {
                let v: Value = serde_json::from_str(l).unwrap();
                v["message"]["content"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    #[test]
    fn run_sends_brief_then_lines_and_records_receipt_display_and_exit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(EXIT_CODE_FILE), "stale\n").unwrap();
        std::fs::write(dir.path().join(RECEIPT_FILE), "earlier\n").unwrap();
        let config = fake_agy(dir.path(), "exit 6");
        let (out, echo) = (Shared::default(), Shared::default());
        let input = std::io::Cursor::new(b"nexz\x7ft\r\n\r\x1b[200~a\rb\x1b[201~\r".to_vec());
        let code = run(&config, input, out.clone(), Some(Box::new(echo.clone()))).unwrap();

        assert_eq!(code, 6);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(EXIT_CODE_FILE)).unwrap(),
            "6\n"
        );
        assert_eq!(sent(dir.path()), ["the brief", "next", "a\nb"]);
        let receipt = std::fs::read_to_string(dir.path().join(RECEIPT_FILE)).unwrap();
        assert!(
            receipt.starts_with("earlier\n{\"event\":\"init\""),
            "{receipt}"
        );
        assert_eq!(receipt.matches("\"event\":\"result\"").count(), 3);
        assert!(receipt.contains("closed\n"), "stderr is kept: {receipt}");
        let shown = out.text();
        assert!(shown.contains("agy session c · model default\n"), "{shown}");
        assert!(shown.contains("── turn 3 SUCCESS · tokens in 3 cache - out - thinking -\n"));
        assert!(!shown.contains("{\"event\""), "{shown}");
        assert!(echo.text().contains("nexz\x08 \x08t"), "{:?}", echo.text());
    }

    #[test]
    fn run_ends_when_agy_ends_though_the_terminal_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        // agy reads one message and fails, as on a quota refusal.
        let mut config = fake_agy(dir.path(), "exit 0");
        std::fs::write(
            &config.program,
            "#!/bin/sh\nread -r line\nprintf 'error: quota' >&2\nexit 3\n",
        )
        .unwrap();
        config.prompt = "brief".into();
        let (terminal, kept_open) = std::os::unix::net::UnixStream::pair().unwrap();
        let out = Shared::default();
        // A deadline of its own (generous for a loaded host; success returns at
        // once): if the brief never reached agy, agy would
        // wait for it forever while the terminal stays open.
        let (done, outcome) = std::sync::mpsc::channel();
        let (cfg, shown) = (config.clone(), out.clone());
        std::thread::spawn(move || {
            let _ = done.send(run(&cfg, terminal, shown, None));
        });
        let code = outcome
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("run must end when agy ends")
            .unwrap();
        drop(kept_open);
        assert_eq!(code, 3);
        assert!(out.text().contains("error: quota\n"));
        // A last line without its newline is still a whole receipt line.
        let receipt = std::fs::read_to_string(dir.path().join(RECEIPT_FILE)).unwrap();
        assert!(receipt.ends_with("error: quota\n"), "{receipt:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(EXIT_CODE_FILE)).unwrap(),
            "3\n"
        );
    }

    #[test]
    fn run_without_a_brief_waits_for_the_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fake_agy(dir.path(), "exit 0");
        config.prompt = "  ".into();
        let code = run(
            &config,
            std::io::Cursor::new(b"only line\n".to_vec()),
            Shared::default(),
            None,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(sent(dir.path()), ["only line"]);
    }

    #[test]
    fn run_stops_feeding_after_ctrl_d() {
        let dir = tempfile::tempdir().unwrap();
        let config = fake_agy(dir.path(), "exit 0");
        let code = run(
            &config,
            std::io::Cursor::new(b"one\n\x04two\n".to_vec()),
            Shared::default(),
            None,
        )
        .unwrap();
        assert_eq!(code, 0);
        assert_eq!(sent(dir.path()), ["the brief", "one"]);
    }

    #[test]
    fn a_taken_terminal_is_unbuffered_and_silent_and_is_restored() {
        use nix::sys::termios::{tcgetattr, LocalFlags, SpecialCharacterIndices};
        let pty = nix::pty::openpty(None, None).unwrap();
        let before = tcgetattr(&pty.slave).unwrap();
        assert!(before
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ECHO));

        let saved = take_terminal(&pty.slave).expect("a pty is a terminal");
        let taken = tcgetattr(&pty.slave).unwrap();
        assert!(!taken.local_flags.contains(LocalFlags::ICANON));
        assert!(!taken.local_flags.contains(LocalFlags::ECHO));
        assert!(
            taken.local_flags.contains(LocalFlags::ISIG),
            "Ctrl-C still signals"
        );
        assert_eq!(
            taken.control_chars[SpecialCharacterIndices::VMIN as usize],
            1
        );
        assert_eq!(
            taken.control_chars[SpecialCharacterIndices::VTIME as usize],
            0
        );

        restore_terminal(&pty.slave, &saved);
        let after = tcgetattr(&pty.slave).unwrap();
        // The kernel may add PENDIN when canonical mode changes; the flags the
        // driver took are what must come back.
        assert!(after
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ECHO));
    }

    #[test]
    fn a_file_is_not_a_terminal_to_take() {
        let file = tempfile::tempfile().unwrap();
        assert!(take_terminal(&file).is_none());
    }

    #[test]
    fn a_stale_exit_code_that_cannot_be_removed_stops_before_agy_starts() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the exit-code file goes cannot be removed as a file.
        std::fs::create_dir(dir.path().join(EXIT_CODE_FILE)).unwrap();
        let config = fake_agy(dir.path(), "exit 0");
        let err = run(
            &config,
            std::io::Cursor::new(b"line\n".to_vec()),
            Shared::default(),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, AgyPaneError::MoleculeDir(_)), "{err}");
        assert!(
            !dir.path().join("stdin").exists(),
            "agy must not run when its previous exit code cannot be cleared"
        );
    }

    #[test]
    fn run_reports_an_agy_that_cannot_start() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fake_agy(dir.path(), "exit 0");
        config.program = dir.path().join("missing").to_string_lossy().into_owned();
        let err = run(&config, std::io::empty(), Shared::default(), None).unwrap_err();
        assert!(
            matches!(err, AgyPaneError::Spawn(ref m) if m.contains("missing")),
            "{err}"
        );
        assert!(!dir.path().join(EXIT_CODE_FILE).exists());
    }

    #[test]
    fn run_refuses_a_molecule_dir_it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = fake_agy(dir.path(), "exit 0");
        config.molecule_dir = dir.path().join("absent");
        let err = run(&config, std::io::empty(), Shared::default(), None).unwrap_err();
        assert!(matches!(err, AgyPaneError::MoleculeDir(_)), "{err}");
    }

    #[test]
    fn exit_status_is_the_code_or_128_plus_the_signal() {
        let ok = Command::new("sh").args(["-c", "exit 3"]).status().unwrap();
        assert_eq!(exit_status_code(ok), 3);
        let killed = Command::new("sh")
            .args(["-c", "kill -TERM $$"])
            .status()
            .unwrap();
        assert_eq!(exit_status_code(killed), 128 + 15);
    }
}
