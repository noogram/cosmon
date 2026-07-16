// SPDX-License-Identifier: AGPL-3.0-only

//! `cs listen [--seconds N]` — voice → whisper.cpp → `cs nucleate spark` (MVP).
//!
//! Pipe an operator utterance into the Inbox without leaving the terminal:
//! record from the default microphone, transcribe on-device via
//! `whisper.cpp`, then hand the transcript to [`super::spark::run`] exactly
//! as if the operator had typed `cs spark "<text>"`.
//!
//! # Why this exists
//!
//! The real problem this solves is the latency between the thought and a
//! landed `cs nucleate`
//! when the laptop is not at arm's length. One evening, one model, one
//! channel, one verb — no multiplex architecture until we have one week of
//! usage data to justify it.
//!
//! # What the command does and does not do
//!
//! - **Does**: record N seconds via `ffmpeg` (`AVFoundation` on macOS),
//!   transcribe via `whisper-cli` from `whisper.cpp`, print the text,
//!   and call [`super::spark::run`] with the text as `topic`.
//! - **Does not**: run a wake-word, stream partials, do continuous
//!   transcription, speak back, or call any cloud ASR. Local-first is a
//!   hard constraint of the v0 — zero network traffic, zero Anthropic
//!   API hit on the audio path.
//!
//! # Invariants respected
//!
//! - **Zero core mutation.** `cs listen` is a new verb but only
//!   delegates to existing verbs ([`super::spark::run`] →
//!   [`super::nucleate::run`]). No new state, no new store, no new
//!   transport.
//! - **§8l CLI/UX capability parity.** A matching UI control can be
//!   added later (e.g. "tap-to-talk" in mac-pilot) without changing
//!   the CLI surface.
//! - **§8j ingress binding (minimal form).** The utterance flows
//!   through ASR → text → `cs nucleate spark`. No parallel path.
//!
//! # Testability
//!
//! The external side-effects (microphone, ffmpeg, whisper-cli) sit
//! behind the [`ListenAdapter`] trait so unit tests can exercise the
//! orchestrator against deterministic stubs, without a microphone or
//! the 488 MB whisper model on disk. The `--transcript <text>` flag
//! offers the same bypass to operators who want to dry-run the spark
//! pipeline without audio.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use super::Context;

/// Arguments for the `listen` subcommand.
#[derive(clap::Args, Debug, Clone)]
pub struct Args {
    /// Seconds to record before cutting off (no VAD in v0).
    ///
    /// Ignored when `--transcript` or `--audio` is supplied. Keep the
    /// value short: whisper-cli on CPU runs near 1× realtime on the
    /// `small` model, so 15 s of speech costs ~15 s of wall-clock.
    #[arg(long, default_value_t = 10)]
    pub seconds: u32,

    /// Skip recording + transcription and use this text directly.
    ///
    /// Useful for scripting (end-to-end dry-runs without a microphone)
    /// and for isolating the spark pipeline from the audio stack when
    /// debugging. Incompatible with `--audio`.
    #[arg(long, value_name = "TEXT", conflicts_with = "audio")]
    pub transcript: Option<String>,

    /// Transcribe an existing audio file instead of recording.
    ///
    /// Accepted formats are whatever the configured `whisper-cli` was
    /// built with (WAV/FLAC/MP3 on the default homebrew build).
    /// Incompatible with `--transcript`.
    #[arg(long, value_name = "PATH")]
    pub audio: Option<PathBuf>,

    /// Path to the whisper-cli binary (defaults to `whisper-cli` in $PATH).
    ///
    /// This is the `whisper.cpp` CLI (`brew install whisper-cpp`), not
    /// the `OpenAI` Python package — the latter is slower and violates
    /// the local-first constraint (it bundles `PyTorch`).
    #[arg(long, default_value = "whisper-cli", value_name = "PATH")]
    pub whisper_bin: String,

    /// Path to the whisper model (e.g. `ggml-small.bin`).
    ///
    /// Can also be set via the `COSMON_WHISPER_MODEL` env var. Required
    /// for actual transcription — if absent and `--transcript` was not
    /// supplied the command fails loudly instead of silently producing
    /// an empty spark.
    #[arg(long, env = "COSMON_WHISPER_MODEL", value_name = "PATH")]
    pub model: Option<PathBuf>,

    /// Whisper language hint: `auto`, `fr`, `en`, …
    ///
    /// The whisper-cpp flag accepts ISO 639-1 codes. `auto` asks whisper
    /// to detect the language itself.
    #[arg(long, default_value = "auto", value_name = "LANG")]
    pub language: String,

    /// Path to the ffmpeg binary used for recording.
    #[arg(long, default_value = "ffmpeg", value_name = "PATH")]
    pub ffmpeg_bin: String,

    /// ffmpeg `AVFoundation` input device specifier.
    ///
    /// On macOS the default microphone is `":0"`. Run
    /// `ffmpeg -f avfoundation -list_devices true -i ""` to enumerate.
    #[arg(long, default_value = ":0", value_name = "SPEC")]
    pub device: String,

    /// Skip nucleation — print the transcript only.
    ///
    /// Use this when validating the audio path on a fresh machine so
    /// you do not pollute the Inbox with test utterances.
    #[arg(long)]
    pub dry_run: bool,

    /// Molecule kind for the resulting spark (delegated to `cs spark`).
    #[arg(long, default_value = "idea")]
    pub kind: String,

    /// Tag to attach (repeatable, defaults to `temp:hot`).
    #[arg(long = "tag", value_name = "TAG")]
    pub tags: Vec<String>,

    /// Fleet to nucleate into.
    #[arg(long, default_value = "default")]
    pub fleet: String,

    /// Override the auto-derived `nucleon_id` (sparker identity).
    #[arg(long)]
    pub nucleon: Option<String>,

    /// Path to the formulas directory (defaults to walk-up discovery).
    #[arg(long, value_name = "DIR")]
    pub formulas_dir: Option<PathBuf>,

    /// Path to the state store root (defaults to walk-up discovery).
    #[arg(long, value_name = "DIR")]
    pub store_dir: Option<PathBuf>,
}

/// Minimum meaningful utterance length — anything shorter is almost
/// certainly silence, a cough, or a mis-triggered recording, and
/// producing a spark from it would just pollute the Inbox.
const MIN_TRANSCRIPT_CHARS: usize = 2;

/// Abstraction over the audio stack so the orchestrator is testable
/// without a microphone.
///
/// Both methods may return `anyhow::Error` on failure; the orchestrator
/// propagates the error untouched so the operator sees the underlying
/// cause (ffmpeg missing, whisper-cli missing, model path invalid, …).
pub trait ListenAdapter {
    /// Record `seconds` seconds of microphone audio to a file and
    /// return its path. The caller takes ownership of the file and
    /// is responsible for cleanup; implementations typically write
    /// to a tempdir that is dropped with the returned handle.
    fn record(&self, seconds: u32) -> anyhow::Result<RecordedAudio>;

    /// Transcribe the given audio file and return the final text.
    fn transcribe(&self, audio: &std::path::Path) -> anyhow::Result<String>;
}

/// Handle over a recorded audio file. Drops the tempdir (if any) on
/// scope exit so repeated `cs listen` invocations do not leak `/tmp`.
pub struct RecordedAudio {
    path: PathBuf,
    #[allow(dead_code)]
    tempdir: Option<tempfile::TempDir>,
}

impl RecordedAudio {
    /// Wrap an existing file path without tempdir ownership. Used when
    /// the operator supplies `--audio <path>`.
    #[must_use]
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            path,
            tempdir: None,
        }
    }

    /// Path to the recorded audio file.
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

/// Real adapter that shells out to ffmpeg and whisper-cli.
struct RealAdapter<'a> {
    ffmpeg_bin: &'a str,
    device: &'a str,
    whisper_bin: &'a str,
    model: Option<&'a std::path::Path>,
    language: &'a str,
}

impl ListenAdapter for RealAdapter<'_> {
    fn record(&self, seconds: u32) -> anyhow::Result<RecordedAudio> {
        let tmp = tempfile::tempdir()?;
        let wav = tmp.path().join("listen.wav");

        // 16 kHz mono s16 — exactly what whisper.cpp wants at its
        // native input rate, so the internal resampler does nothing.
        let status = Command::new(self.ffmpeg_bin)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "avfoundation",
                "-i",
                self.device,
                "-t",
                &seconds.to_string(),
                "-ac",
                "1",
                "-ar",
                "16000",
                "-y",
                wav.to_str().ok_or_else(|| {
                    anyhow::anyhow!("tempdir path is not valid utf-8: {}", wav.display())
                })?,
            ])
            .status()
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to spawn ffmpeg ({}): {e}. Install via `brew install ffmpeg` and grant Terminal microphone permission.",
                    self.ffmpeg_bin,
                )
            })?;

        if !status.success() {
            anyhow::bail!(
                "ffmpeg exited with {status:?}; check that `{}` has microphone permission and that device `{}` exists (list with `ffmpeg -f avfoundation -list_devices true -i \"\"`)",
                self.ffmpeg_bin,
                self.device,
            );
        }

        Ok(RecordedAudio {
            path: wav,
            tempdir: Some(tmp),
        })
    }

    fn transcribe(&self, audio: &std::path::Path) -> anyhow::Result<String> {
        let model = self.model.ok_or_else(|| {
            anyhow::anyhow!(
                "no whisper model supplied: pass --model <path> or set COSMON_WHISPER_MODEL (e.g. to ~/tmp/audio-stack-check/models/ggml-small.bin)"
            )
        })?;

        let out = Command::new(self.whisper_bin)
            .args([
                "-m",
                model
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("model path not utf-8: {}", model.display()))?,
                "-l",
                self.language,
                "-f",
                audio
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("audio path not utf-8: {}", audio.display()))?,
                "-nt", // no timestamps — we want plain text.
                "-np", // no progress — keep stdout clean.
            ])
            .output()
            .map_err(|e| {
                anyhow::anyhow!(
                    "failed to spawn whisper-cli ({}): {e}. Install via `brew install whisper-cpp`.",
                    self.whisper_bin,
                )
            })?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "whisper-cli exited with {:?}:\n{}",
                out.status,
                stderr.trim()
            );
        }

        // whisper-cli with -nt -np emits the transcript on stdout, one
        // segment per line. Trim surrounding whitespace and join.
        let stdout = String::from_utf8(out.stdout)
            .map_err(|e| anyhow::anyhow!("whisper-cli stdout is not utf-8: {e}"))?;
        Ok(normalize_transcript(&stdout))
    }
}

/// Collapse whisper's segment-per-line output into a single utterance.
///
/// Extracted so `--transcript` and the real transcription path apply
/// the exact same cleanup (trim, collapse blank lines, join with
/// single spaces). Empty segments are dropped.
pub(crate) fn normalize_transcript(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_owned()
}

/// Execute the `listen` command.
///
/// # Errors
/// Propagates any error from the recording, transcription, or
/// [`super::spark::run`] path. In particular, a whisper-cli failure
/// or a missing model produces a loud error rather than a silent
/// empty spark.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    let model = args.model.as_deref();
    let adapter = RealAdapter {
        ffmpeg_bin: &args.ffmpeg_bin,
        device: &args.device,
        whisper_bin: &args.whisper_bin,
        model,
        language: &args.language,
    };
    run_with_adapter(ctx, args, &adapter)
}

/// Orchestrator split out for testability.
///
/// The surface side-effects (recording, transcription) go through the
/// supplied adapter; the spark-nucleation side-effect always goes
/// through [`super::spark::run`] (unit-testing that path is already
/// covered by `spark::tests`).
pub fn run_with_adapter(
    ctx: &Context,
    args: &Args,
    adapter: &dyn ListenAdapter,
) -> anyhow::Result<()> {
    let (transcript, timings) = capture_transcript(args, adapter)?;

    if transcript.chars().count() < MIN_TRANSCRIPT_CHARS {
        anyhow::bail!(
            "transcript too short ({} chars) — refusing to nucleate a blank spark. Check the microphone level and whisper model.",
            transcript.chars().count()
        );
    }

    emit_transcript(ctx, &transcript, &timings);

    if args.dry_run {
        return Ok(());
    }

    let spark_args = super::spark::Args {
        text: transcript,
        kind: args.kind.clone(),
        tags: args.tags.clone(),
        fleet: args.fleet.clone(),
        nucleon: args.nucleon.clone(),
        sparked_by: None,
        formula: "spark".to_owned(),
        formulas_dir: args.formulas_dir.clone(),
        store_dir: args.store_dir.clone(),
    };
    super::spark::run(ctx, &spark_args)
}

/// Outcome timings produced by [`capture_transcript`] so the operator
/// can see where the wall-clock went.
#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct Timings {
    record_ms: Option<u128>,
    transcribe_ms: Option<u128>,
}

/// Decide which transcript to use based on the mutually-exclusive
/// `--transcript` / `--audio` / recording modes, and return the text
/// plus timings.
fn capture_transcript(
    args: &Args,
    adapter: &dyn ListenAdapter,
) -> anyhow::Result<(String, Timings)> {
    if let Some(ref text) = args.transcript {
        return Ok((normalize_transcript(text), Timings::default()));
    }

    let mut timings = Timings::default();

    let recording = if let Some(ref path) = args.audio {
        if !path.exists() {
            anyhow::bail!("--audio path does not exist: {}", path.display());
        }
        RecordedAudio::from_path(path.clone())
    } else {
        let t0 = Instant::now();
        let rec = adapter.record(args.seconds)?;
        timings.record_ms = Some(t0.elapsed().as_millis());
        rec
    };

    let t0 = Instant::now();
    let text = adapter.transcribe(recording.path())?;
    timings.transcribe_ms = Some(t0.elapsed().as_millis());

    Ok((normalize_transcript(&text), timings))
}

/// Print the transcript (and timings) to stderr so the operator can
/// verify what is about to be nucleated. Stderr keeps stdout reserved
/// for the `cs spark` → `cs nucleate` JSON tail under `--json`.
fn emit_transcript(ctx: &Context, transcript: &str, timings: &Timings) {
    if ctx.json {
        let payload = serde_json::json!({
            "event": "listen.transcript",
            "text": transcript,
            "record_ms": timings.record_ms,
            "transcribe_ms": timings.transcribe_ms,
        });
        eprintln!("{payload}");
    } else {
        eprintln!("cs listen: transcript = {transcript:?}");
        if let Some(ms) = timings.record_ms {
            eprintln!(
                "cs listen: record {} / transcribe {}",
                human_duration(ms),
                timings
                    .transcribe_ms
                    .map_or_else(|| "?".to_owned(), human_duration)
            );
        }
    }
}

fn human_duration(ms: u128) -> String {
    let d = Duration::from_millis(u64::try_from(ms).unwrap_or(u64::MAX));
    if d.as_secs() >= 1 {
        format!("{:.1}s", d.as_secs_f32())
    } else {
        format!("{}ms", d.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::path::Path;

    /// Deterministic adapter for orchestrator-level tests.
    struct StubAdapter {
        recorded: Cell<u32>,
        transcribed: Cell<u32>,
        transcript: String,
    }

    impl StubAdapter {
        fn new(transcript: &str) -> Self {
            Self {
                recorded: Cell::new(0),
                transcribed: Cell::new(0),
                transcript: transcript.to_owned(),
            }
        }
    }

    impl ListenAdapter for StubAdapter {
        fn record(&self, _seconds: u32) -> anyhow::Result<RecordedAudio> {
            self.recorded.set(self.recorded.get() + 1);
            // Write a sentinel file so `--audio` contract-like inputs
            // can be exercised without actually producing audio.
            let tmp = tempfile::tempdir()?;
            let path = tmp.path().join("stub.wav");
            fs::write(&path, b"stub")?;
            Ok(RecordedAudio {
                path,
                tempdir: Some(tmp),
            })
        }

        fn transcribe(&self, _audio: &Path) -> anyhow::Result<String> {
            self.transcribed.set(self.transcribed.get() + 1);
            Ok(self.transcript.clone())
        }
    }

    fn real_repo_formula_path() -> PathBuf {
        // Mirror spark::real_repo_formula_path — walk up from the
        // manifest dir to the repo root and pick up the shared
        // spark.formula.toml the test relies on.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join(".cosmon/formulas/spark.formula.toml"))
            .expect("spark.formula.toml locatable from manifest")
    }

    fn base_args() -> Args {
        Args {
            seconds: 0,
            transcript: None,
            audio: None,
            whisper_bin: "whisper-cli".into(),
            model: None,
            language: "auto".into(),
            ffmpeg_bin: "ffmpeg".into(),
            device: ":0".into(),
            dry_run: false,
            kind: "idea".into(),
            tags: vec![],
            fleet: "default".into(),
            nucleon: Some("listen-test@demo.example".into()),
            formulas_dir: None,
            store_dir: None,
        }
    }

    #[test]
    fn normalize_trims_and_collapses_whisper_segments() {
        let raw = "  \n\nHello, Cosmon.\n\n   This is a sparky idea.   \n";
        assert_eq!(
            normalize_transcript(raw),
            "Hello, Cosmon. This is a sparky idea."
        );
    }

    #[test]
    fn transcript_flag_bypasses_audio_stack() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::copy(
            real_repo_formula_path(),
            formulas_dir.join("spark.formula.toml"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let mut args = base_args();
        args.transcript = Some("ceci est une idée vocale".into());
        args.formulas_dir = Some(formulas_dir.clone());
        args.store_dir = Some(state_dir.clone());

        let adapter = StubAdapter::new("unused — --transcript is set");
        run_with_adapter(&ctx, &args, &adapter).unwrap();

        // The adapter must not have been touched at all.
        assert_eq!(adapter.recorded.get(), 0);
        assert_eq!(adapter.transcribed.get(), 0);

        let mol_root = state_dir.join("fleets").join("default").join("molecules");
        let mol_dir = fs::read_dir(&mol_root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .expect("exactly one molecule");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(
            state["variables"]["topic"].as_str(),
            Some("ceci est une idée vocale"),
            "topic must round-trip from --transcript to the spark variable"
        );
        assert_eq!(state["kind"].as_str(), Some("idea"));
    }

    #[test]
    fn record_plus_transcribe_produces_spark_with_right_topic() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::copy(
            real_repo_formula_path(),
            formulas_dir.join("spark.formula.toml"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let mut args = base_args();
        args.seconds = 3;
        args.formulas_dir = Some(formulas_dir.clone());
        args.store_dir = Some(state_dir.clone());

        let adapter = StubAdapter::new("réunion demain : revoir le pitch\n");
        run_with_adapter(&ctx, &args, &adapter).unwrap();

        assert_eq!(adapter.recorded.get(), 1, "must record once");
        assert_eq!(adapter.transcribed.get(), 1, "must transcribe once");

        let mol_root = state_dir.join("fleets").join("default").join("molecules");
        let mol_dir = fs::read_dir(&mol_root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .find(|p| p.is_dir())
            .expect("exactly one molecule");
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(mol_dir.join("state.json")).unwrap()).unwrap();
        assert_eq!(
            state["variables"]["topic"].as_str(),
            Some("réunion demain : revoir le pitch"),
        );
    }

    #[test]
    fn dry_run_prints_transcript_without_nucleating() {
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join("state");
        let formulas_dir = tmp.path().join("formulas");
        fs::create_dir_all(&state_dir).unwrap();
        fs::create_dir_all(&formulas_dir).unwrap();
        fs::copy(
            real_repo_formula_path(),
            formulas_dir.join("spark.formula.toml"),
        )
        .unwrap();

        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let mut args = base_args();
        args.dry_run = true;
        args.transcript = Some("don't nucleate me".into());
        args.formulas_dir = Some(formulas_dir.clone());
        args.store_dir = Some(state_dir.clone());

        let adapter = StubAdapter::new("ignored");
        run_with_adapter(&ctx, &args, &adapter).unwrap();

        // No molecule dir should have been created — the state store
        // layout writes fleets/<name>/molecules/ only when a nucleate
        // actually happened.
        let mol_root = state_dir.join("fleets").join("default").join("molecules");
        let existed = mol_root.exists()
            && fs::read_dir(&mol_root)
                .map(|it| it.filter_map(Result::ok).any(|e| e.path().is_dir()))
                .unwrap_or(false);
        assert!(!existed, "dry-run must not create a molecule");
    }

    #[test]
    fn refuses_blank_transcript_with_explicit_error() {
        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let mut args = base_args();
        args.transcript = Some("   \n  ".into());

        let adapter = StubAdapter::new("ignored");
        let err = run_with_adapter(&ctx, &args, &adapter).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("too short") || msg.contains("blank"),
            "expected explicit refusal, got: {msg}"
        );
    }

    #[test]
    fn missing_audio_file_fails_loudly() {
        let ctx = Context {
            verbose: false,
            json: false,
            config: None,
        };
        let mut args = base_args();
        args.audio = Some(PathBuf::from("/nonexistent/listen-test.wav"));

        let adapter = StubAdapter::new("unused");
        let err = run_with_adapter(&ctx, &args, &adapter).unwrap_err();
        assert!(
            format!("{err}").contains("does not exist"),
            "expected explicit error, got: {err}"
        );
    }
}
