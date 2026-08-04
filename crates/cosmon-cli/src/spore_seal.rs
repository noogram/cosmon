// SPDX-License-Identifier: AGPL-3.0-only

//! Shell-side seams for the seal-verification contract (ADR-140 D4, N4).
//!
//! The pure decision logic - the three-state honesty contract, the gate, the
//! proof hash, the orchestration - lives in
//! [`cosmon_core::spore::seal`]. This module supplies the two I/O
//! implementations the core declares as traits, so `cs spore run` (N5) can wire
//! a real TLC run and a persistent verdict cache:
//!
//! * `RealTlcRunner` - locates a JRE + `tla2tools.jar` and runs TLC against
//!   the seal's `.tla` module and `.cfg` config. **Detection is honest**: when
//!   no JRE or no `tla2tools.jar` is found, `available`
//!   returns `false`, the core reports the seal as unchecked, and the default
//!   gate refuses - it never silently passes.
//! * `FileSealVerdictCache` - persists the `proof_hash -> passed` verdict
//!   under `.cosmon/cache/seal/<hash>`, keyed by the BLAKE3 of the proof content
//!   so an edited proof is a cache miss by construction.
//!
//! Locating `tla2tools.jar`, in order: the `TLA2TOOLS_JAR` environment variable
//! (a full path to the jar), else the common install paths probed by
//! `locate_tla2tools`. The JRE is found via the `JAVA_HOME` environment
//! variable or a `java` binary on `PATH`.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use cosmon_core::error::CosmonError;
use cosmon_core::spore::seal::{SealVerdictCache, TlcOutcome, TlcRunner};

/// Locate the `java` executable: `$JAVA_HOME/bin/java` if `JAVA_HOME` is set and
/// the binary exists, else bare `java` (resolved on `PATH` at spawn time).
fn locate_java() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("JAVA_HOME") {
        let candidate = Path::new(&home).join("bin").join("java");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // Fall back to PATH resolution: probe `java -version`, which exits 0 when a
    // JRE is present. We do not hard-require it here; the spawn in `check` will
    // surface a missing binary as `Unavailable`.
    let probe = Command::new("java")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    probe.then(|| PathBuf::from("java"))
}

/// Locate `tla2tools.jar`: the `TLA2TOOLS_JAR` env var (full path) first, then a
/// short list of common install locations.
fn locate_tla2tools() -> Option<PathBuf> {
    if let Some(jar) = std::env::var_os("TLA2TOOLS_JAR") {
        let p = PathBuf::from(jar);
        if p.is_file() {
            return Some(p);
        }
    }
    let candidates = [
        "/usr/local/lib/tla2tools.jar",
        "/usr/local/share/tla/tla2tools.jar",
        "/opt/tla/tla2tools.jar",
        "/usr/share/java/tla2tools.jar",
    ];
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| {
            // The TLA+ Toolbox / VS Code extension ships the jar under the home
            // directory; probe the conventional spot.
            std::env::var_os("HOME").and_then(|home| {
                let p = Path::new(&home).join(".tla").join("tla2tools.jar");
                p.is_file().then_some(p)
            })
        })
}

/// A [`TlcRunner`] that spawns a real JRE + TLC.
///
/// Constructed via `detect`, which resolves the JRE and
/// `tla2tools.jar` once. If either is missing the runner reports
/// `available` `= false` and the core stays honest:
/// the seal is reported unchecked, never verified.
#[derive(Debug, Clone)]
pub struct RealTlcRunner {
    java: Option<PathBuf>,
    jar: Option<PathBuf>,
}

impl RealTlcRunner {
    /// Detect the JRE and `tla2tools.jar` on this machine.
    #[must_use]
    pub fn detect() -> Self {
        Self {
            java: locate_java(),
            jar: locate_tla2tools(),
        }
    }

    /// The resolved `java` path, if any (exposed for diagnostics).
    #[must_use]
    pub fn java_path(&self) -> Option<&Path> {
        self.java.as_deref()
    }

    /// The resolved `tla2tools.jar` path, if any (exposed for diagnostics).
    #[must_use]
    pub fn jar_path(&self) -> Option<&Path> {
        self.jar.as_deref()
    }
}

/// Build the TLC command used by recipient-side seal verification.
fn tlc_command(java: &Path, jar: &Path, module: &Path, config: Option<&Path>) -> Command {
    let mut cmd = Command::new(java);
    cmd.arg("-XX:+UseParallelGC")
        .arg("-cp")
        .arg(jar)
        .arg("tlc2.TLC")
        .arg("-workers")
        .arg("auto");
    if let Some(cfg) = config {
        cmd.arg("-config").arg(cfg);
    }
    cmd.arg(module);
    cmd
}

/// Relay one TLC stream live to stderr while retaining it for diagnostics.
///
/// TLC progress belongs on stderr because `cs spore run --json` reserves
/// stdout for NDJSON molecule records.
fn relay_tlc_output(reader: impl Read) -> std::io::Result<String> {
    let mut reader = BufReader::new(reader);
    let mut captured = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        eprint!("{line}");
        captured.push_str(&line);
    }
    Ok(captured)
}

/// Extract the invariant TLC names in its standard rejection line.
fn violated_invariant(output: &str) -> Option<&str> {
    output.lines().find_map(|line| {
        let (_, after_prefix) = line.split_once("Invariant ")?;
        let (name, _) = after_prefix.split_once(" is violated")?;
        let name = name.trim();
        (!name.is_empty()).then_some(name)
    })
}

/// Choose a concise failure detail, preferring the violated property over the
/// final raw counterexample state that TLC prints later.
fn failure_detail(stdout: &str, stderr: &str) -> String {
    if let Some(name) = violated_invariant(stdout).or_else(|| violated_invariant(stderr)) {
        return format!("invariant {name} violated");
    }

    stdout
        .lines()
        .chain(stderr.lines())
        .rev()
        .find(|line| {
            let line = line.trim();
            !line.is_empty() && !line.starts_with("Finished")
        })
        .map_or("TLC reported a non-zero exit", str::trim)
        .chars()
        .take(200)
        .collect()
}

impl TlcRunner for RealTlcRunner {
    fn available(&self) -> bool {
        self.java.is_some() && self.jar.is_some()
    }

    fn check(&self, module: &Path, config: Option<&Path>) -> TlcOutcome {
        let (Some(java), Some(jar)) = (&self.java, &self.jar) else {
            return TlcOutcome::Unavailable;
        };

        eprintln!("seal: verifying with TLC (parallel workers; this can take several minutes)...");
        let mut cmd = tlc_command(java, jar, module, config);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        match cmd.spawn() {
            Ok(mut child) => {
                let Some(stdout) = child.stdout.take() else {
                    return TlcOutcome::Unavailable;
                };
                let Some(stderr) = child.stderr.take() else {
                    return TlcOutcome::Unavailable;
                };
                let (status, stdout, stderr) = std::thread::scope(|scope| {
                    let stdout_reader = scope.spawn(|| relay_tlc_output(stdout));
                    let stderr_reader = scope.spawn(|| relay_tlc_output(stderr));
                    let status = child.wait();
                    let stdout = stdout_reader
                        .join()
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default();
                    let stderr = stderr_reader
                        .join()
                        .ok()
                        .and_then(Result::ok)
                        .unwrap_or_default();
                    (status, stdout, stderr)
                });

                match status {
                    Ok(status) if status.success() => TlcOutcome::Passed,
                    Ok(_) => TlcOutcome::Failed {
                        detail: failure_detail(&stdout, &stderr),
                    },
                    Err(_) => TlcOutcome::Unavailable,
                }
            }
            // The runner claimed availability but the spawn failed (jar moved,
            // JRE removed mid-run): honest Unavailable, never a silent pass.
            Err(_) => TlcOutcome::Unavailable,
        }
    }
}

/// A filesystem-backed [`SealVerdictCache`] under `.cosmon/cache/seal/<hash>`.
///
/// A verdict is a file named for the proof's BLAKE3 hash; its presence (content
/// `passed`) means a prior TLC pass over byte-identical proof content. An edited
/// proof has a different hash and therefore a different (absent) file: a cache
/// miss that re-runs TLC.
#[derive(Debug, Clone)]
pub struct FileSealVerdictCache {
    dir: PathBuf,
}

impl FileSealVerdictCache {
    /// Construct a cache rooted at `dir` (typically `.cosmon/cache/seal`).
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The on-disk path for a proof hash's verdict file.
    fn path_for(&self, proof_hash: &str) -> PathBuf {
        self.dir.join(proof_hash)
    }
}

impl SealVerdictCache for FileSealVerdictCache {
    fn get(&self, proof_hash: &str) -> Result<Option<bool>, CosmonError> {
        let path = self.path_for(proof_hash);
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(Some(s.trim() == "passed")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(CosmonError::Io(e)),
        }
    }

    fn put(&self, proof_hash: &str, passed: bool) -> Result<(), CosmonError> {
        std::fs::create_dir_all(&self.dir).map_err(CosmonError::Io)?;
        let body = if passed { "passed" } else { "failed" };
        std::fs::write(self.path_for(proof_hash), body).map_err(CosmonError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_cache_roundtrips_a_verdict() {
        let tmp = std::env::temp_dir().join(format!("cosmon-seal-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let cache = FileSealVerdictCache::new(tmp.clone());

        // Miss before any write.
        assert_eq!(cache.get("deadbeef").unwrap(), None);

        // Store a pass, then hit.
        cache.put("deadbeef", true).unwrap();
        assert_eq!(cache.get("deadbeef").unwrap(), Some(true));

        // A different hash (edited proof) is still a miss.
        assert_eq!(cache.get("cafef00d").unwrap(), None);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn detection_is_honest_when_no_jre() {
        // We cannot assume a JRE is present in CI, but we CAN assert the
        // contract: when the runner has no java or no jar, it reports
        // unavailable and `check` yields Unavailable (never a silent pass).
        let runner = RealTlcRunner {
            java: None,
            jar: None,
        };
        assert!(!runner.available());
        assert_eq!(
            runner.check(Path::new("spore.tla"), None),
            TlcOutcome::Unavailable
        );
    }

    #[test]
    fn tlc_uses_parallel_gc_and_all_workers() {
        let command = tlc_command(
            Path::new("java"),
            Path::new("tla2tools.jar"),
            Path::new("spore.tla"),
            Some(Path::new("spore.cfg")),
        );
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert_eq!(
            args,
            [
                "-XX:+UseParallelGC",
                "-cp",
                "tla2tools.jar",
                "tlc2.TLC",
                "-workers",
                "auto",
                "-config",
                "spore.cfg",
                "spore.tla",
            ]
        );
    }

    #[test]
    fn failure_detail_names_the_violated_invariant_not_the_last_state() {
        let output = r#"Error: Invariant BlindBeforeConfrontation is violated.
State 42: <Action line 7, col 1 to line 9, col 2 of module spore>
/\\ role = "notice"
/\\ regime = "rgpd"
Finished in 01s at (2026-08-04 12:00:00)"#;

        assert_eq!(
            failure_detail(output, ""),
            "invariant BlindBeforeConfrontation violated"
        );
    }
}
