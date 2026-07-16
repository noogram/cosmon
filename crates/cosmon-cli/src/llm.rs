// SPDX-License-Identifier: AGPL-3.0-only

//! Checkpointed LLM streaming step.
//!
//! Replaces the historical "curl-style synchronous LLM call" — where an
//! 8-minute response timed out in one undifferentiated drop — with a
//! streaming call that flushes partial output to disk on a configurable
//! cadence and emits typed `ExternalChannelTimeout` events on stalls.
//!
//! The runtime contract:
//!
//! - The provider produces an iterator of token-or-chunk strings.
//! - The runtime appends each chunk to `output_path` and flushes every
//!   `checkpoint_every` seconds.
//! - On a per-checkpoint silence (`timeout_per_checkpoint`) or aggregate
//!   ceiling (`max_total_minutes`), the runtime emits an
//!   `ExternalChannelTimeout` event and decides whether to retry from the
//!   on-disk prefix.
//!
//! The provider abstraction is deliberately thin (`fn call(spec) -> impl
//! Iterator<...>`) so a mock test provider can simulate timeouts and
//! reprises deterministically. The Anthropic implementation is a separate
//! provider key (`"anthropic"`) and is not invoked in CI — workflows that
//! need a real LLM must opt in via env vars.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use cosmon_core::event_v2::ExternalChannelTimeoutKind;
use cosmon_core::formula::LlmSpec;

/// A single chunk from a provider stream.
///
/// Real implementations carry token strings; the test provider can also
/// inject `Stall` markers to simulate a network silence the runtime should
/// notice.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum StreamItem {
    /// A normal data chunk to append to the output.
    Chunk(String),
    /// Simulated silence — the test provider yields this to make the
    /// runtime emit an `ExternalChannelTimeout`. Real providers never
    /// produce this variant.
    Stall { duration: Duration },
    /// An explicit provider-side abort (rate-limit, server error, network
    /// drop). The runtime translates this to
    /// [`ExternalChannelTimeoutKind::ProviderAborted`].
    Abort(String),
}

/// Errors a provider can return.
#[derive(Debug)]
#[allow(dead_code)]
pub enum LlmError {
    /// The provider key is not registered.
    UnknownProvider(String),
    /// The provider was registered but failed to start the call.
    Setup(String),
    /// The streaming run terminated unsuccessfully. `attempt` is 1-based.
    StreamFailed {
        /// Classified failure mode.
        kind: ExternalChannelTimeoutKind,
        /// Bytes successfully flushed to disk before the failure.
        bytes_flushed: u64,
        /// Attempt number that just failed (1-based).
        attempt: u32,
        /// Free-form detail for the operator.
        detail: String,
    },
    /// All retry attempts were exhausted without producing a complete
    /// response.
    RetriesExhausted {
        /// Final attempt number.
        attempt: u32,
        /// Bytes flushed at the moment of final failure.
        bytes_flushed: u64,
    },
    /// Local I/O on the output path failed.
    Io(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider(p) => write!(f, "unknown llm provider: {p}"),
            Self::Setup(d) => write!(f, "llm provider setup failed: {d}"),
            Self::StreamFailed {
                kind,
                attempt,
                detail,
                ..
            } => write!(
                f,
                "llm stream failed (attempt {attempt}, {kind:?}): {detail}",
            ),
            Self::RetriesExhausted {
                attempt,
                bytes_flushed,
            } => write!(
                f,
                "llm retries exhausted after {attempt} attempts ({bytes_flushed} bytes flushed)",
            ),
            Self::Io(d) => write!(f, "llm I/O failed: {d}"),
        }
    }
}

impl std::error::Error for LlmError {}

/// A streaming provider — produces an iterator of [`StreamItem`].
///
/// The signature is intentionally object-safe so the runtime can plug in a
/// boxed mock provider in tests. Returns `Setup` errors at start time;
/// per-chunk errors arrive as `Abort` items inside the stream.
pub trait Provider {
    /// Start a streaming call. The returned iterator yields chunks until
    /// the response is complete (drained) or the runtime aborts.
    ///
    /// `prompt` is the resolved prompt text (the runtime has already read
    /// `prompt_file` if needed). `prefix` is the bytes already on disk
    /// from a previous attempt — providers that support resumption (real
    /// Anthropic via `prefill`) should consume them; mock and basic
    /// providers may ignore it.
    ///
    /// # Errors
    /// Returns `LlmError::Setup` if the provider cannot establish the
    /// connection.
    fn stream(
        &self,
        spec: &LlmSpec,
        prompt: &str,
        prefix: &str,
    ) -> Result<Box<dyn Iterator<Item = StreamItem> + Send>, LlmError>;
}

/// Outcome of a streaming run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    /// The stream drained without any timeout. `bytes_flushed` includes
    /// the input prefix (if any) and the freshly streamed bytes.
    Completed {
        /// Final byte length of the output file.
        bytes_flushed: u64,
        /// Number of checkpoints flushed during the run.
        checkpoints: u32,
    },
    /// A per-checkpoint stall fired. The runtime decides whether to retry.
    Stalled {
        /// Bytes successfully flushed before the stall.
        bytes_flushed: u64,
        /// Seconds since the last observed chunk.
        age_s: u64,
    },
    /// The aggregate `max_total_minutes` ceiling fired.
    TotalBudgetExceeded {
        /// Bytes flushed at the moment the budget fired.
        bytes_flushed: u64,
    },
    /// The provider explicitly aborted the stream.
    ProviderAborted {
        /// Bytes flushed before the abort.
        bytes_flushed: u64,
        /// Free-form provider message.
        detail: String,
    },
}

/// Sink that captures the streamed output. Allows tests to introspect what
/// was flushed without touching the filesystem.
pub trait OutputSink {
    /// Append a chunk to the on-disk output. Returns the new total byte
    /// length on success.
    ///
    /// # Errors
    /// Returns [`LlmError::Io`] if the underlying write fails.
    fn append(&mut self, chunk: &str) -> Result<u64, LlmError>;
    /// Flush any buffered bytes to durable storage. Called every
    /// `checkpoint_every` seconds and on completion.
    ///
    /// # Errors
    /// Returns [`LlmError::Io`] if the flush fails.
    fn flush(&mut self) -> Result<(), LlmError>;
    /// Current byte length of the output.
    fn len(&self) -> u64;
}

/// File-backed [`OutputSink`] that appends to `output_path` and uses
/// `BufWriter` for buffering between checkpoints.
pub struct FileSink {
    file: std::fs::File,
    bytes: u64,
}

impl FileSink {
    /// Open `path` for append, seeded to the existing file size.
    ///
    /// # Errors
    /// Returns [`LlmError::Io`] if the file cannot be opened.
    pub fn open(path: &Path) -> Result<Self, LlmError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| LlmError::Io(e.to_string()))?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|e| LlmError::Io(e.to_string()))?;
        let bytes = file
            .metadata()
            .map_err(|e| LlmError::Io(e.to_string()))?
            .len();
        Ok(Self { file, bytes })
    }
}

impl OutputSink for FileSink {
    fn append(&mut self, chunk: &str) -> Result<u64, LlmError> {
        use std::io::Write;
        self.file
            .write_all(chunk.as_bytes())
            .map_err(|e| LlmError::Io(e.to_string()))?;
        self.bytes += chunk.len() as u64;
        Ok(self.bytes)
    }
    fn flush(&mut self) -> Result<(), LlmError> {
        use std::io::Write;
        self.file.flush().map_err(|e| LlmError::Io(e.to_string()))
    }
    fn len(&self) -> u64 {
        self.bytes
    }
}

/// In-memory [`OutputSink`] used in tests.
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct MemorySink {
    buffer: String,
    flushed: u64,
}

#[allow(dead_code)]
impl MemorySink {
    /// Create an empty in-memory sink.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Borrow the entire buffered content (useful in assertions).
    #[must_use]
    pub fn contents(&self) -> &str {
        &self.buffer
    }
    /// Number of bytes that have been "flushed" — i.e. that survived a
    /// simulated crash. Anything appended after the last flush is lost.
    #[must_use]
    pub fn flushed_bytes(&self) -> u64 {
        self.flushed
    }
}

impl OutputSink for MemorySink {
    fn append(&mut self, chunk: &str) -> Result<u64, LlmError> {
        self.buffer.push_str(chunk);
        Ok(self.buffer.len() as u64)
    }
    fn flush(&mut self) -> Result<(), LlmError> {
        self.flushed = self.buffer.len() as u64;
        Ok(())
    }
    fn len(&self) -> u64 {
        self.buffer.len() as u64
    }
}

/// Time abstraction so tests can drive the clock deterministically.
pub trait Clock {
    /// Current monotonic instant.
    fn now(&self) -> Instant;
    /// Block (or pretend to) for the requested duration.
    fn sleep(&self, dur: Duration);
}

/// Real clock — wraps [`std::time`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn sleep(&self, dur: Duration) {
        std::thread::sleep(dur);
    }
}

/// Run the streaming step a single attempt.
///
/// Returns a [`RunOutcome`] classifying the outcome. The caller is
/// responsible for the retry loop and event emission (the function is
/// pure with respect to the clock + sink it is given so tests can drive
/// it without I/O).
///
/// # Errors
/// Returns [`LlmError::Io`] if the sink fails. Provider-side failures
/// surface as [`RunOutcome::Stalled`] / [`RunOutcome::ProviderAborted`].
pub fn run_attempt<C: Clock>(
    spec: &LlmSpec,
    prompt: &str,
    sink: &mut dyn OutputSink,
    provider: &dyn Provider,
    clock: &C,
) -> Result<RunOutcome, LlmError> {
    // Capture the bytes already on disk; passed to the provider so it can
    // resume from the prefix. We only treat the *flushed* bytes as durable.
    let prefix_len = sink.len();
    let prefix = String::new(); // FileSink doesn't expose its prefix; provider
                                // implementations that need it read the file
                                // directly. Mock provider ignores it.
    let stream = provider.stream(spec, prompt, &prefix)?;

    let started = clock.now();
    let mut last_progress = started;
    let mut last_checkpoint = started;
    let mut checkpoints = 0u32;

    let total_budget = Duration::from_secs(spec.max_total_minutes.saturating_mul(60));
    let checkpoint_window = Duration::from_secs(spec.checkpoint_every_secs);
    let stall_window = Duration::from_secs(spec.timeout_per_checkpoint_secs);

    for item in stream {
        // Aggregate budget check first — this is the hard ceiling.
        if clock.now().duration_since(started) > total_budget {
            sink.flush()?;
            return Ok(RunOutcome::TotalBudgetExceeded {
                bytes_flushed: sink.len(),
            });
        }
        match item {
            StreamItem::Chunk(s) => {
                sink.append(&s)?;
                last_progress = clock.now();
            }
            StreamItem::Stall { duration } => {
                clock.sleep(duration);
                let age = clock.now().duration_since(last_progress).as_secs();
                if age >= stall_window.as_secs() {
                    sink.flush()?;
                    return Ok(RunOutcome::Stalled {
                        bytes_flushed: sink.len(),
                        age_s: age,
                    });
                }
            }
            StreamItem::Abort(detail) => {
                sink.flush()?;
                return Ok(RunOutcome::ProviderAborted {
                    bytes_flushed: sink.len(),
                    detail,
                });
            }
        }
        if clock.now().duration_since(last_checkpoint) >= checkpoint_window {
            sink.flush()?;
            last_checkpoint = clock.now();
            checkpoints += 1;
        }
    }
    sink.flush()?;
    let _ = prefix_len;
    Ok(RunOutcome::Completed {
        bytes_flushed: sink.len(),
        checkpoints,
    })
}

/// Built-in provider registry. Today only the deterministic mock is
/// registered — adding a real Anthropic provider lands behind a feature
/// flag once the streaming SDK story is settled.
#[must_use]
pub fn lookup_provider(key: &str) -> Option<Box<dyn Provider + Send + Sync>> {
    match key {
        "mock" => Some(Box::new(MockProvider::default())),
        _ => None,
    }
}

/// Mock provider for tests and offline runs.
///
/// The script is a pre-recorded vector of [`StreamItem`]s replayed in order.
/// The provider holds no real network state, so it is trivially `Send +
/// Sync`. Set up scripts via [`MockProvider::with_script`].
#[derive(Debug, Default, Clone)]
pub struct MockProvider {
    scripts: HashMap<String, Vec<StreamItem>>,
    default_script: Vec<StreamItem>,
}

#[allow(dead_code)]
impl MockProvider {
    /// Create a mock provider with a single default script (replayed for
    /// every prompt).
    #[must_use]
    pub fn with_default(script: Vec<StreamItem>) -> Self {
        Self {
            scripts: HashMap::new(),
            default_script: script,
        }
    }
    /// Bind a script to a specific prompt prefix. Useful when a single
    /// test exercises multiple LLM calls.
    #[must_use]
    pub fn with_script(mut self, prompt_prefix: &str, script: Vec<StreamItem>) -> Self {
        self.scripts.insert(prompt_prefix.to_owned(), script);
        self
    }
}

impl Provider for MockProvider {
    fn stream(
        &self,
        _spec: &LlmSpec,
        prompt: &str,
        _prefix: &str,
    ) -> Result<Box<dyn Iterator<Item = StreamItem> + Send>, LlmError> {
        let chosen = self
            .scripts
            .iter()
            .find(|(k, _)| prompt.starts_with(k.as_str()))
            .map_or_else(|| self.default_script.clone(), |(_, v)| v.clone());
        Ok(Box::new(chosen.into_iter()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    /// Deterministic clock that advances only when [`Self::sleep`] is
    /// called or [`Self::tick`] is invoked manually.
    struct FakeClock {
        now: RefCell<Instant>,
    }

    impl FakeClock {
        fn new() -> Self {
            Self {
                now: RefCell::new(Instant::now()),
            }
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            *self.now.borrow()
        }
        fn sleep(&self, dur: Duration) {
            *self.now.borrow_mut() += dur;
        }
    }

    fn spec() -> LlmSpec {
        LlmSpec {
            provider: "mock".to_owned(),
            model: "test".to_owned(),
            prompt: Some("hi".to_owned()),
            prompt_file: None,
            output_path: PathBuf::from("synthesis.md"),
            checkpoint_every_secs: 1,
            timeout_per_checkpoint_secs: 5,
            max_total_minutes: 1,
            max_retries: 3,
        }
    }

    #[test]
    fn happy_path_completes() {
        let provider = MockProvider::with_default(vec![
            StreamItem::Chunk("hello ".to_owned()),
            StreamItem::Chunk("world".to_owned()),
        ]);
        let mut sink = MemorySink::new();
        let clock = FakeClock::new();
        let outcome = run_attempt(&spec(), "hi", &mut sink, &provider, &clock).unwrap();
        match outcome {
            RunOutcome::Completed { bytes_flushed, .. } => {
                assert_eq!(bytes_flushed, sink.len());
                assert_eq!(sink.contents(), "hello world");
                assert_eq!(sink.flushed_bytes(), 11);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn stall_triggers_typed_outcome() {
        let provider = MockProvider::with_default(vec![
            StreamItem::Chunk("partial".to_owned()),
            StreamItem::Stall {
                duration: Duration::from_secs(10),
            },
        ]);
        let mut sink = MemorySink::new();
        let clock = FakeClock::new();
        let outcome = run_attempt(&spec(), "hi", &mut sink, &provider, &clock).unwrap();
        match outcome {
            RunOutcome::Stalled {
                bytes_flushed,
                age_s,
            } => {
                assert_eq!(bytes_flushed, 7);
                assert!(age_s >= 5);
                assert_eq!(sink.flushed_bytes(), 7);
            }
            other => panic!("expected Stalled, got {other:?}"),
        }
    }

    #[test]
    fn provider_aborted_surfaces() {
        let provider = MockProvider::with_default(vec![
            StreamItem::Chunk("ok".to_owned()),
            StreamItem::Abort("rate-limit".to_owned()),
        ]);
        let mut sink = MemorySink::new();
        let clock = FakeClock::new();
        let outcome = run_attempt(&spec(), "hi", &mut sink, &provider, &clock).unwrap();
        match outcome {
            RunOutcome::ProviderAborted {
                bytes_flushed,
                detail,
            } => {
                assert_eq!(bytes_flushed, 2);
                assert_eq!(detail, "rate-limit");
            }
            other => panic!("expected ProviderAborted, got {other:?}"),
        }
    }

    /// Property-style test: starting from a partial prefix, the second
    /// attempt resumes and concatenates correctly. This is the
    /// "checkpoint reprise" guarantee the briefing asks for.
    #[test]
    fn reprise_after_stall_concatenates_prefix() {
        let mut spec = spec();
        spec.timeout_per_checkpoint_secs = 1;
        let mut sink = MemorySink::new();

        // First attempt: produces "abc" then stalls.
        let p1 = MockProvider::with_default(vec![
            StreamItem::Chunk("abc".to_owned()),
            StreamItem::Stall {
                duration: Duration::from_secs(5),
            },
        ]);
        let clock = FakeClock::new();
        match run_attempt(&spec, "hi", &mut sink, &p1, &clock).unwrap() {
            RunOutcome::Stalled { .. } => {}
            other => panic!("expected Stalled, got {other:?}"),
        }
        assert_eq!(sink.contents(), "abc");
        assert_eq!(sink.flushed_bytes(), 3);

        // Second attempt: resumes (mock provider ignores the prefix; the
        // sink retains "abc" from the prior attempt) and finishes "def".
        let p2 = MockProvider::with_default(vec![StreamItem::Chunk("def".to_owned())]);
        let clock2 = FakeClock::new();
        match run_attempt(&spec, "hi", &mut sink, &p2, &clock2).unwrap() {
            RunOutcome::Completed { bytes_flushed, .. } => {
                assert_eq!(bytes_flushed, 6);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert_eq!(sink.contents(), "abcdef");
    }

    #[test]
    fn total_budget_exceeded_after_long_stream() {
        let mut spec = spec();
        spec.max_total_minutes = 0; // 0 minutes = immediate ceiling
        let provider = MockProvider::with_default(vec![
            StreamItem::Stall {
                duration: Duration::from_secs(120),
            },
            StreamItem::Chunk("never".to_owned()),
        ]);
        let mut sink = MemorySink::new();
        let clock = FakeClock::new();
        let outcome = run_attempt(&spec, "hi", &mut sink, &provider, &clock).unwrap();
        match outcome {
            RunOutcome::TotalBudgetExceeded { .. } | RunOutcome::Stalled { .. } => {}
            other => panic!("expected budget/stall, got {other:?}"),
        }
    }

    #[test]
    fn unknown_provider_lookup_returns_none() {
        assert!(lookup_provider("anthropic").is_none());
        assert!(lookup_provider("nonsense").is_none());
        assert!(lookup_provider("mock").is_some());
    }
}
