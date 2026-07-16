// SPDX-License-Identifier: AGPL-3.0-only

//! `cs doctor leaks` — detect accidentally committed secrets and non-public files.
//!
//! The probe walks the set of files tracked by git (i.e., what a public
//! mirror would see) and flags two classes of hazard:
//!
//! 1. **Suspicious paths** — committing `.cosmon/state/**`, `.ssh/**`,
//!    `.config/gh/**`, `.env*`, `*.pem`, `*.p12`, `id_rsa*`, browser
//!    history dumps. The fleet state file reveals worker/molecule
//!    topology to any attacker who clones the repo; the rest are
//!    unambiguous credential material.
//! 2. **Suspicious content patterns** — a curated pattern set targeting
//!    the most frequently leaked keys: AWS access keys (`AKIA…`/`ASIA…`),
//!    GitHub PAT (`ghp_…`, `github_pat_…`), Anthropic/OpenAI keys
//!    (`sk-ant-…`, `sk-…`), Slack bot tokens, Google API keys, and PEM
//!    private-key headers. Each rule is either a [`Pattern::Literal`]
//!    (substring match — used when the prefix is unambiguous, e.g.
//!    `ghp_`, `sk-ant-`, BEGIN-headers) or a [`Pattern::Regex`] (full
//!    token shape — used when the prefix occurs in natural text and
//!    needs the trailing entropy to disambiguate, e.g. AWS keys are
//!    `AKIA[A-Z0-9]{16}` / `ASIA[A-Z0-9]{16}`, exactly 20 chars).
//!
//! On any match the probe emits a `Severity::Error` finding — `cs doctor
//! leaks` exits non-zero. This is the only blocking probe in the sprint 1
//! scope.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use regex::Regex;

use super::findings::{Finding, ProbeReport, Severity};

const PROBE: &str = "leaks";

/// Maximum number of bytes read per tracked file for content scanning.
///
/// The budget caps worst-case runtime on large binaries that slip through
/// path-based filtering; real secret material sits near the file head.
const MAX_SCAN_BYTES: usize = 256 * 1024;

/// File extensions we deliberately skip when scanning content (binaries,
/// pre-built artifacts). Path-based checks still apply.
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "ico", "pdf", "zip", "gz", "tar", "xz", "bz2", "7z",
    "mp3", "mp4", "mov", "avi", "wav", "woff", "woff2", "ttf", "otf", "eot", "so", "dylib", "dll",
    "wasm", "bin", "class", "jar", "node", "o", "a", "rlib", "exe",
];

/// Path fragments whose presence in a tracked file is always suspicious.
///
/// Each tuple carries `(match fragment, severity, title, remediation)`.
const SUSPICIOUS_PATH_RULES: &[(&str, Severity, &str, &str)] = &[
    (
        ".cosmon/state/",
        Severity::Error,
        "fleet/molecule state file committed",
        "Remove from git (`git rm --cached`) and verify .cosmon/.gitignore covers state/.",
    ),
    (
        "/.ssh/",
        Severity::Error,
        "SSH directory committed",
        "These are private keys — rotate them and purge from history.",
    ),
    (
        "/.config/gh/",
        Severity::Error,
        "gh CLI config committed",
        "Contains GitHub auth tokens — rotate via `gh auth refresh` and purge from history.",
    ),
    (
        "/.aws/",
        Severity::Error,
        "AWS credentials dir committed",
        "Rotate keys immediately, then purge from history.",
    ),
    (
        "/.netrc",
        Severity::Error,
        ".netrc committed (machine credentials)",
        "Rotate any passwords found inside, then purge.",
    ),
    (
        "id_rsa",
        Severity::Error,
        "SSH private key filename committed",
        "Rotate the key pair and purge from history.",
    ),
    (
        "id_ed25519",
        Severity::Error,
        "SSH private key filename committed",
        "Rotate the key pair and purge from history.",
    ),
];

/// File extensions (including the dot-stripped form) that should never be
/// committed — private keys, certificate material, credential bundles.
const SUSPICIOUS_EXTENSIONS: &[&str] = &["pem", "p12", "pfx", "jks", "keystore", "kdbx", "key"];

/// File base-names that should never be committed — dotenv variants.
const SUSPICIOUS_BASENAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.staging",
    ".env.secret",
    ".envrc",
];

/// A content-pattern rule is either a literal substring or a full regex.
///
/// `Literal(s)` matches via `line.contains(s)` — fast, audit-friendly, and
/// the right tool when the prefix itself is unambiguous (e.g. `ghp_`,
/// `sk-ant-`, BEGIN-headers). `Regex(src)` is compiled lazily at first
/// scan and matches via `Regex::find` — required when the prefix occurs
/// in natural text and only the trailing token shape disambiguates the
/// real secret from a false positive.
#[derive(Debug, Clone, Copy)]
enum Pattern {
    Literal(&'static str),
    Regex(&'static str),
}

/// Patterns scanned in every UTF-8 line of every tracked file.
///
/// Order doesn't matter — the first match on a line wins for reporting
/// purposes.
///
/// **AWS keys are `Pattern::Regex` on purpose.** AWS access key IDs have
/// the strict shape `AKIA[A-Z0-9]{16}` (permanent) / `ASIA[A-Z0-9]{16}`
/// (temporary), exactly 20 uppercase-alphanumeric characters. The bare
/// substrings `AKIA` and `ASIA` collide with natural-language tokens —
/// notably the Springer LNCS conference title *"Advances in Cryptology –
/// ASIACRYPT 2015"*, which blocked a knowledge-galaxy push on 2026-05-09
/// before this fix. Anchoring on the full 20-char shape eliminates the
/// false positive while preserving real-key detection.
///
/// Every other rule is `Pattern::Literal` because its prefix is rare
/// enough in natural text (`ghp_`, `sk-ant-`, BEGIN-headers, …) that
/// substring matching is both sufficient and faster.
const CONTENT_PATTERNS: &[(Pattern, &str)] = &[
    (
        Pattern::Regex(r"AKIA[A-Z0-9]{16}"),
        "possible AWS access key ID",
    ),
    (
        Pattern::Regex(r"ASIA[A-Z0-9]{16}"),
        "possible AWS temporary access key",
    ),
    (
        Pattern::Literal("ghp_"),
        "possible GitHub personal access token",
    ),
    (
        Pattern::Literal("github_pat_"),
        "possible GitHub fine-grained PAT",
    ),
    (Pattern::Literal("gho_"), "possible GitHub OAuth token"),
    (
        Pattern::Literal("ghu_"),
        "possible GitHub user-to-server token",
    ),
    (
        Pattern::Literal("ghs_"),
        "possible GitHub server-to-server token",
    ),
    (Pattern::Literal("ghr_"), "possible GitHub refresh token"),
    (Pattern::Literal("sk-ant-"), "possible Anthropic API key"),
    (Pattern::Literal("sk-proj-"), "possible OpenAI project key"),
    (Pattern::Literal("xoxb-"), "possible Slack bot token"),
    (Pattern::Literal("xoxp-"), "possible Slack user token"),
    (Pattern::Literal("AIza"), "possible Google API key"),
    (
        Pattern::Literal("-----BEGIN RSA PRIVATE KEY-----"),
        "PEM RSA private key header",
    ),
    (
        Pattern::Literal("-----BEGIN OPENSSH PRIVATE KEY-----"),
        "OpenSSH private key header",
    ),
    (
        Pattern::Literal("-----BEGIN EC PRIVATE KEY-----"),
        "PEM EC private key header",
    ),
    (
        Pattern::Literal("-----BEGIN DSA PRIVATE KEY-----"),
        "PEM DSA private key header",
    ),
    (
        Pattern::Literal("-----BEGIN PRIVATE KEY-----"),
        "PEM PKCS#8 private key header",
    ),
];

/// Compiled form of a [`CONTENT_PATTERNS`] entry. Built once on first
/// scan and reused for every line of every file.
enum CompiledPattern {
    Literal(&'static str),
    Regex(Regex),
}

struct CompiledRule {
    pattern: CompiledPattern,
    label: &'static str,
}

/// Lazily compile [`CONTENT_PATTERNS`] into matchers. Compilation
/// happens once per process; subsequent scans reuse the cached vector.
fn compiled_patterns() -> &'static [CompiledRule] {
    static CELL: OnceLock<Vec<CompiledRule>> = OnceLock::new();
    CELL.get_or_init(|| {
        CONTENT_PATTERNS
            .iter()
            .map(|(pat, label)| {
                let pattern = match pat {
                    Pattern::Literal(s) => CompiledPattern::Literal(s),
                    Pattern::Regex(src) => CompiledPattern::Regex(
                        Regex::new(src).expect("CONTENT_PATTERNS regex compiles"),
                    ),
                };
                CompiledRule { pattern, label }
            })
            .collect()
    })
}

/// Arguments for `cs doctor leaks`.
#[derive(clap::Args, Default)]
pub struct Args {
    /// Limit scanning to this subdirectory (relative to repo root).
    #[arg(long)]
    pub path: Option<PathBuf>,
    /// Also scan untracked working-tree files (use when pre-commit check).
    #[arg(long)]
    pub include_untracked: bool,
    /// Byte-literal patterns, one per line (UTF-8, `#` comments).
    #[arg(long, value_name = "FILE")]
    pub corpus: Option<PathBuf>,
}

/// Scan a repository for leaked credentials or non-public state files.
///
/// `root` is the repository root; normally the current working directory.
/// The probe is read-only and pure (except for reading the filesystem).
///
/// # Errors
/// Returns an error if the repository has no git history (`git ls-files`
/// fails); individual file read errors become `Severity::Warning` findings
/// rather than aborting the probe.
pub fn scan(root: &Path, args: &Args) -> anyhow::Result<ProbeReport> {
    let mut report = ProbeReport::new(PROBE);

    let corpus = match &args.corpus {
        Some(path) => load_corpus(path)?,
        None => Vec::new(),
    };

    let files = list_tracked_files(root, args.include_untracked)?;
    for rel in files {
        if let Some(subdir) = &args.path {
            if !rel.starts_with(subdir) {
                continue;
            }
        }
        report.scanned += 1;
        scan_path_rules(&rel, &mut report);

        let abs = root.join(&rel);
        if is_binary_extension(&rel) {
            continue;
        }
        match read_head(&abs, MAX_SCAN_BYTES) {
            Ok(bytes) => scan_content(&rel, &bytes, &corpus, &mut report),
            Err(e) => {
                report.findings.push(
                    Finding::new(
                        PROBE,
                        Severity::Warning,
                        format!("could not read {}: {e}", rel.display()),
                    )
                    .with_path(&rel),
                );
            }
        }
    }

    // Sort findings so errors float to the top in text output.
    report.findings.sort_by(|a, b| {
        severity_rank(a.severity)
            .cmp(&severity_rank(b.severity))
            .then_with(|| a.probe.cmp(b.probe))
            .then_with(|| a.title.cmp(&b.title))
    });
    Ok(report)
}

/// Entry point invoked by the `cs doctor leaks` subcommand dispatcher.
///
/// # Errors
/// Returns an error if the scan cannot be started (non-git directory).
pub fn run(ctx: &super::Context, args: &Args) -> anyhow::Result<()> {
    let root = git_root(&std::env::current_dir()?)?;
    let report = scan(&root, args)?;
    super::emit_report_and_exit(ctx, &[report])
}

fn severity_rank(sev: Severity) -> u8 {
    match sev {
        Severity::Error => 0,
        Severity::Warning => 1,
        Severity::Info => 2,
    }
}

fn is_binary_extension(rel: &Path) -> bool {
    rel.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        let lower = e.to_ascii_lowercase();
        BINARY_EXTENSIONS.iter().any(|b| *b == lower)
    })
}

fn scan_path_rules(rel: &Path, report: &mut ProbeReport) {
    let as_str = rel.to_string_lossy();
    for (fragment, sev, title, hint) in SUSPICIOUS_PATH_RULES {
        if as_str.contains(fragment) {
            report.findings.push(
                Finding::new(PROBE, *sev, (*title).to_owned())
                    .with_path(rel)
                    .with_remediation((*hint).to_owned()),
            );
        }
    }

    if let Some(ext) = rel.extension().and_then(|e| e.to_str()) {
        let lower = ext.to_ascii_lowercase();
        if SUSPICIOUS_EXTENSIONS.iter().any(|e| *e == lower) {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Error,
                    format!("credential-like file extension .{lower}"),
                )
                .with_path(rel)
                .with_remediation(
                    "Move to an out-of-tree vault and delete from git history.".to_owned(),
                ),
            );
        }
    }

    if let Some(name) = rel.file_name().and_then(|n| n.to_str()) {
        if SUSPICIOUS_BASENAMES.contains(&name) {
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Error,
                    format!("dotenv-style file committed: {name}"),
                )
                .with_path(rel)
                .with_remediation(
                    "Add to .gitignore, `git rm --cached`, and purge from history.".to_owned(),
                ),
            );
        }
    }
}

fn scan_content(rel: &Path, bytes: &[u8], corpus: &[(String, String)], report: &mut ProbeReport) {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return; // non-utf8 binary-ish file; skip content scan
    };
    let rules = compiled_patterns();
    for (lineno, line) in text.lines().enumerate() {
        // Match returns the actual matched substring of `line`, so the
        // snippet sanitizer can locate it for context. For Literal rules
        // that's the pattern itself; for Regex rules it's the captured
        // span (e.g. the full 20-char AWS key, not just the regex source).
        let mut hit: Option<(&str, &str)> = None;
        for rule in rules {
            let matched: Option<&str> = match &rule.pattern {
                CompiledPattern::Literal(s) => line.find(*s).map(|idx| &line[idx..idx + s.len()]),
                CompiledPattern::Regex(re) => re.find(line).map(|m| m.as_str()),
            };
            if let Some(m) = matched {
                hit = Some((m, rule.label));
                break;
            }
        }
        if hit.is_none() {
            for (pat, label) in corpus {
                if line.contains(pat.as_str()) {
                    hit = Some((pat.as_str(), label.as_str()));
                    break;
                }
            }
        }
        if let Some((pat, label)) = hit {
            let snippet = sanitized_snippet(line, pat);
            report.findings.push(
                Finding::new(
                    PROBE,
                    Severity::Error,
                    format!("{label} in {}", rel.display()),
                )
                .with_path(rel)
                .with_detail(format!("line {}: {snippet}", lineno + 1))
                .with_remediation(
                    "Rotate the credential NOW, then remove the commit via \
                     `git filter-repo` or BFG. Password changes alone are not enough."
                        .to_owned(),
                ),
            );
        }
    }
}

/// Parse a byte-literal corpus file. Each non-comment, non-empty line is a
/// pattern checked via `line.contains(pattern)`. Inline `# …` comments and
/// leading/trailing whitespace are stripped. The label is the pattern
/// itself, prefixed with `corpus:` so findings stay distinguishable from
/// built-in matches.
fn load_corpus(path: &Path) -> anyhow::Result<Vec<(String, String)>> {
    let text = fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read corpus {}: {e}", path.display()))?;
    let mut out = Vec::new();
    for raw in text.lines() {
        let without_comment = raw.split_once('#').map_or(raw, |(head, _)| head);
        let pat = without_comment.trim();
        if pat.is_empty() {
            continue;
        }
        out.push((pat.to_owned(), format!("corpus pattern `{pat}`")));
    }
    Ok(out)
}

/// Produce a truncated snippet showing the pattern and up to ~20 surrounding
/// characters on each side. Never prints the full line — we do not want
/// `cs doctor --json` output itself to become the leak vector.
fn sanitized_snippet(line: &str, pattern: &str) -> String {
    let idx = line.find(pattern).unwrap_or(0);
    let start = idx.saturating_sub(16);
    let end = (idx + pattern.len() + 16).min(line.len());
    let slice = line.get(start..end).unwrap_or("");
    let mut out = slice.replace(['\n', '\r'], " ");
    if start > 0 {
        out.insert(0, '…');
    }
    if end < line.len() {
        out.push('…');
    }
    out
}

fn read_head(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut buf = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut handle = f.by_ref().take(max_bytes as u64);
    handle.read_to_end(&mut buf)?;
    Ok(buf)
}

/// Locate the git root by walking up from `start`. Returns an error if
/// not inside a git checkout.
pub(super) fn git_root(start: &Path) -> anyhow::Result<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(start)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to invoke git: {e}"))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "not a git repository (starting at {}): {}",
            start.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let path = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if path.is_empty() {
        return Err(anyhow::anyhow!("git rev-parse returned empty toplevel"));
    }
    Ok(PathBuf::from(path))
}

fn list_tracked_files(root: &Path, include_untracked: bool) -> anyhow::Result<Vec<PathBuf>> {
    let mut args = vec!["ls-files", "-z"];
    if include_untracked {
        args.push("--others");
        args.push("--exclude-standard");
        args.push("--cached");
    }
    let out = Command::new("git")
        .args(&args)
        .current_dir(root)
        .output()
        .map_err(|e| anyhow::anyhow!("failed to invoke git ls-files: {e}"))?;
    if !out.status.success() {
        return Err(anyhow::anyhow!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| PathBuf::from(String::from_utf8_lossy(s).into_owned()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_pattern_detects_github_pat() {
        let mut report = ProbeReport::new(PROBE);
        let line = b"token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123\n";
        scan_content(Path::new("config.txt"), line, &[], &mut report);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Error);
        assert!(report.findings[0].title.contains("GitHub"));
    }

    #[test]
    fn content_pattern_detects_pem_header() {
        let mut report = ProbeReport::new(PROBE);
        let payload = b"-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n";
        scan_content(Path::new("foo"), payload, &[], &mut report);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].title.contains("OpenSSH"));
    }

    #[test]
    fn content_pattern_ignores_clean_text() {
        let mut report = ProbeReport::new(PROBE);
        scan_content(Path::new("x"), b"nothing to see here\n", &[], &mut report);
        assert!(report.findings.is_empty());
    }

    #[test]
    fn corpus_pattern_matches_user_supplied() {
        let mut report = ProbeReport::new(PROBE);
        let corpus = vec![(
            "Tenant-Demo secret".to_owned(),
            "corpus pattern `Tenant-Demo secret`".to_owned(),
        )];
        scan_content(
            Path::new("notes.md"),
            b"draft: Tenant-Demo secret alpha\n",
            &corpus,
            &mut report,
        );
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].title.contains("corpus pattern"));
    }

    #[test]
    fn corpus_line_does_not_double_fire_with_builtin() {
        let mut report = ProbeReport::new(PROBE);
        let corpus = vec![("ghp_".to_owned(), "corpus pattern `ghp_`".to_owned())];
        let line = b"token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123\n";
        scan_content(Path::new("x"), line, &corpus, &mut report);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].title.contains("GitHub"));
    }

    #[test]
    fn load_corpus_strips_comments_and_blanks() {
        let dir = std::env::temp_dir().join(format!("cosmon-leak-corpus-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("corpus.txt");
        fs::write(
            &file,
            "# header comment\n\nexample tenant secret  # IP label\nsk-ant-\n\n  # trailing\n",
        )
        .unwrap();
        let parsed = load_corpus(&file).unwrap();
        let pats: Vec<_> = parsed.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(pats, vec!["Tenant-Demo secret", "sk-ant-"]);
    }

    #[test]
    fn path_rule_flags_cosmon_state() {
        let mut report = ProbeReport::new(PROBE);
        scan_path_rules(Path::new(".cosmon/state/fleet.json"), &mut report);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].severity, Severity::Error);
    }

    #[test]
    fn path_rule_flags_dotenv_basename() {
        let mut report = ProbeReport::new(PROBE);
        scan_path_rules(Path::new("services/api/.env"), &mut report);
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("dotenv-style")));
    }

    #[test]
    fn path_rule_flags_pem_extension() {
        let mut report = ProbeReport::new(PROBE);
        scan_path_rules(Path::new("ops/certs/server.pem"), &mut report);
        assert!(report
            .findings
            .iter()
            .any(|f| f.title.contains("credential-like file extension")));
    }

    #[test]
    fn snippet_truncates_and_masks_boundaries() {
        let line = "x".repeat(100) + "ghp_ABCDEFG" + &"y".repeat(100);
        let snip = sanitized_snippet(&line, "ghp_");
        assert!(snip.contains("ghp_"));
        assert!(snip.len() < line.len());
        assert!(snip.starts_with('…'));
        assert!(snip.ends_with('…'));
    }

    #[test]
    fn is_binary_extension_detects_png() {
        assert!(is_binary_extension(Path::new("a/b/c.png")));
        assert!(!is_binary_extension(Path::new("a/b/c.rs")));
    }

    // --- AWS access-key regex regression suite ---------------------------
    //
    // Origin: 2026-05-09, knowledge galaxy push blocked because the
    // OpenAlex-canonical conference title "Advances in Cryptology –
    // ASIACRYPT 2015" contains the substring `ASIA`. The old byte-literal
    // rule fired; the new Pattern::Regex rule requires the full 20-char
    // AWS shape and lets the conference name through cleanly.

    #[test]
    fn asia_regex_ignores_asiacrypt_conference_title() {
        let mut report = ProbeReport::new(PROBE);
        let line = "Advances in Cryptology \u{2013} ASIACRYPT 2015\n";
        scan_content(Path::new("nodes.ndjson"), line.as_bytes(), &[], &mut report);
        assert!(
            report.findings.is_empty(),
            "ASIACRYPT title must not match the ASIA rule, got: {:?}",
            report.findings
        );
    }

    #[test]
    fn asia_regex_detects_real_temporary_key() {
        let mut report = ProbeReport::new(PROBE);
        // ASIA + 16 uppercase-alphanumeric = 20 chars total, matches the
        // documented AWS temporary access key shape.
        let line = b"creds: ASIA1234567890ABCDEF\n";
        scan_content(Path::new("config.txt"), line, &[], &mut report);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].title.contains("AWS temporary"));
    }

    #[test]
    fn akia_regex_detects_real_permanent_key() {
        let mut report = ProbeReport::new(PROBE);
        let line = b"AWS_ACCESS_KEY_ID=AKIA0123456789ABCDEF\n";
        scan_content(Path::new("env.sh"), line, &[], &mut report);
        assert_eq!(report.findings.len(), 1);
        assert!(report.findings[0].title.contains("AWS access key"));
    }

    #[test]
    fn akia_regex_ignores_natural_text_collision() {
        let mut report = ProbeReport::new(PROBE);
        // `EUR-AKIA-PROJECT` would have fired the old byte-literal rule;
        // the regex demands [A-Z0-9]{16} after AKIA, hyphens break it.
        let line = b"project codename: EUR-AKIA-PROJECT phase II\n";
        scan_content(Path::new("notes.md"), line, &[], &mut report);
        assert!(
            report.findings.is_empty(),
            "EUR-AKIA-PROJECT must not match the AKIA rule, got: {:?}",
            report.findings
        );
    }

    #[test]
    fn akia_regex_ignores_short_prefix() {
        let mut report = ProbeReport::new(PROBE);
        // Bare prefix without the 16-char tail is not a credential.
        let line = b"see also: AKIAS (a tribe in northeastern Asia)\n";
        scan_content(Path::new("ethnography.md"), line, &[], &mut report);
        assert!(
            report.findings.is_empty(),
            "Bare AKIA-prefixed words must not match, got: {:?}",
            report.findings
        );
    }

    #[test]
    fn aws_regex_compiles_at_startup() {
        // Forces compilation of every CONTENT_PATTERNS entry; will panic
        // on a malformed regex source. Cheap insurance against a future
        // edit that introduces a broken pattern.
        let rules = compiled_patterns();
        assert!(rules.len() >= CONTENT_PATTERNS.len());
    }
}
