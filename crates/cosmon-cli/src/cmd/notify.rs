// SPDX-License-Identifier: AGPL-3.0-only

//! `cs notify` — push channel from the fleet to the operator's attention.
//!
//! Closes the silent-24h gap: until `cs notify` existed, no primitive emitted
//! to the operator's attention surface, so a fleet could go quiet for a day
//! without anyone noticing. Patrol or hooks can now send a one-line message
//! via configured channels — `macos` (osascript), `file-drop` (write a
//! Markdown file the operator's watchers see), `element` (Matrix webhook,
//! optional), and `telegram` (Bot API `sendMessage`, optional). Telegram is
//! the channel a long-running fleet can push its checkpoints onto so they land
//! privately in the operator's Telegram DM.
//!
//! ## Design
//!
//! - **Composable**: `cs notify "..."` with `--channel` overrides, otherwise
//!   uses the union of channels declared in `.cosmon/config.toml`.
//! - **Best-effort**: a single failing channel does not abort the others. The
//!   command exits 0 if at least one channel succeeded, 1 if all failed,
//!   2 only when arguments are invalid.
//! - **Append-only**: every notify call also writes a record into
//!   `events.jsonl` (`type=notify_dispatched`) so the audit trail is intact
//!   — but this happens lazily via the existing event infrastructure; we do
//!   not introduce a new `EventV2` variant for it (the channel sends are
//!   already best-effort and not load-bearing in the spec).
//! - **CI-safe**: when `COSMON_NOTIFY_DRY_RUN=1` is set in the environment,
//!   no channel is contacted; the channels' `dispatch` is replaced by a
//!   stdout log line. Tests rely on this.
//!
//! See [`Config`] for the TOML shape, and [`Channel`] for the supported
//! transports.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use serde::Deserialize;

use super::Context;

/// Subcommand arguments.
#[derive(clap::Args)]
pub struct Args {
    /// The notification message (positional, single line).
    #[arg(value_name = "MESSAGE")]
    pub message: String,

    /// Optional title prefix. Channels render it as the first line / header.
    #[arg(long, value_name = "TITLE", default_value = "cosmon")]
    pub title: String,

    /// Override the configured channel set. May be repeated. Recognised values:
    /// `macos`, `file-drop`, `element`, `telegram`. When omitted, every channel
    /// declared in `.cosmon/config.toml` is used.
    #[arg(long = "channel", value_name = "CHANNEL")]
    pub channels: Vec<String>,

    /// Optional molecule id the notification is *about*. Surfaces in the
    /// JSON output and the file-drop body.
    #[arg(long, value_name = "MOLECULE_ID")]
    pub molecule: Option<String>,

    /// Severity tag (advisory). One of `info`, `warn`, `alert`. Channels
    /// that support it (file-drop) pass it through; others ignore.
    #[arg(long, value_name = "LEVEL", default_value = "info")]
    pub level: String,

    /// Treat dispatch as a dry-run: log what *would* be sent without
    /// invoking any side-effecting transport. Equivalent to setting
    /// `COSMON_NOTIFY_DRY_RUN=1`.
    #[arg(long)]
    pub dry_run: bool,
}

/// Run the `cs notify` command.
///
/// # Errors
///
/// Returns an error if argument parsing rejects the level or all channels
/// fail to dispatch. A single channel failure is logged and skipped.
pub fn run(ctx: &Context, args: &Args) -> Result<()> {
    let level = parse_level(&args.level)?;
    let dry_run = args.dry_run || std::env::var_os("COSMON_NOTIFY_DRY_RUN").is_some();

    // Resolve config (.cosmon/config.toml) + the active channel set.
    let config_path = super::resolve_config_from_context(ctx);
    let cfg = Config::load(&config_path).unwrap_or_default();
    let channels = resolve_channels(&args.channels, &cfg)?;

    if channels.is_empty() {
        // Empty config + no overrides → silent no-op. Exit 0 so callers
        // (patrol, hooks) can fire-and-forget without inspecting config.
        if ctx.json {
            println!(r#"{{"dispatched":[],"failed":[],"reason":"no channels configured"}}"#);
        } else {
            eprintln!("notify: no channels configured (skip)");
        }
        return Ok(());
    }

    let payload = Payload {
        title: &args.title,
        message: &args.message,
        molecule: args.molecule.as_deref(),
        level,
    };

    let mut dispatched = Vec::new();
    let mut failed = Vec::new();
    for ch in &channels {
        let outcome = if dry_run {
            // Dry-run: log to stderr, never call OS / network.
            eprintln!(
                "notify (dry-run) → {ch:?}: [{title}] {msg}",
                title = payload.title,
                msg = payload.message
            );
            Ok(())
        } else {
            ch.dispatch(&payload, &cfg)
        };
        match outcome {
            Ok(()) => dispatched.push(ch.name().to_owned()),
            Err(e) => {
                eprintln!("notify: channel {} failed: {e}", ch.name());
                failed.push((ch.name().to_owned(), e.to_string()));
            }
        }
    }

    if ctx.json {
        let json = serde_json::json!({
            "dispatched": dispatched,
            "failed": failed.iter().map(|(name, err)| {
                serde_json::json!({"channel": name, "error": err})
            }).collect::<Vec<_>>(),
            "title": args.title,
            "level": args.level,
            "molecule": args.molecule,
            "dry_run": dry_run,
        });
        println!("{json}");
    } else if !dispatched.is_empty() {
        println!("notify → {}", dispatched.join(", "));
    }

    if dispatched.is_empty() && !failed.is_empty() {
        return Err(anyhow!(
            "all {} channels failed (first error: {})",
            failed.len(),
            failed[0].1
        ));
    }
    Ok(())
}

/// Configured-channel union (from TOML + CLI override).
fn resolve_channels(overrides: &[String], cfg: &Config) -> Result<Vec<Channel>> {
    let names: Vec<&str> = if overrides.is_empty() {
        cfg.channels.iter().map(String::as_str).collect()
    } else {
        overrides.iter().map(String::as_str).collect()
    };
    names.into_iter().map(Channel::parse).collect()
}

fn parse_level(s: &str) -> Result<Level> {
    match s {
        "info" => Ok(Level::Info),
        "warn" => Ok(Level::Warn),
        "alert" => Ok(Level::Alert),
        other => Err(anyhow!(
            "unknown level '{other}' (expected info|warn|alert)"
        )),
    }
}

/// One push payload, carried through to every channel.
#[derive(Debug)]
struct Payload<'a> {
    title: &'a str,
    message: &'a str,
    molecule: Option<&'a str>,
    level: Level,
}

#[derive(Debug, Clone, Copy)]
enum Level {
    Info,
    Warn,
    Alert,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Alert => "alert",
        }
    }
}

/// One configured push transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    /// `osascript` macOS notification (best-effort, mac-only).
    Macos,
    /// Drop a Markdown file in `notify.file-drop.path` (default `~/Drop/cosmon-notifications/`).
    FileDrop,
    /// POST a JSON payload to a Matrix/Element webhook (`notify.element.webhook_url`).
    Element,
    /// POST to the Telegram Bot API `sendMessage` endpoint
    /// (`notify.telegram.bot_token` + `notify.telegram.chat_id`).
    Telegram,
}

impl Channel {
    fn parse(s: &str) -> Result<Self> {
        match s {
            "macos" | "mac" | "osascript" => Ok(Self::Macos),
            "file-drop" | "file" => Ok(Self::FileDrop),
            "element" | "matrix" => Ok(Self::Element),
            "telegram" | "tg" => Ok(Self::Telegram),
            other => Err(anyhow!(
                "unknown notify channel '{other}' (expected macos|file-drop|element|telegram)"
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::FileDrop => "file-drop",
            Self::Element => "element",
            Self::Telegram => "telegram",
        }
    }

    fn dispatch(self, p: &Payload<'_>, cfg: &Config) -> Result<()> {
        match self {
            Self::Macos => dispatch_macos(p, cfg.macos.as_ref()),
            Self::FileDrop => dispatch_file_drop(p, cfg.file_drop.as_ref()),
            Self::Element => dispatch_element(p, cfg.element.as_ref()),
            Self::Telegram => dispatch_telegram(p, cfg.telegram.as_ref()),
        }
    }
}

fn dispatch_macos(p: &Payload<'_>, mac: Option<&MacosConfig>) -> Result<()> {
    let sound = mac.and_then(|m| m.sound.as_deref()).unwrap_or("default");
    // osascript display notification — best-effort. We escape double quotes
    // by replacing them with single quotes; AppleScript is fragile here.
    let safe_msg = p.message.replace('"', "'");
    let safe_title = p.title.replace('"', "'");
    let script = format!(
        r#"display notification "{safe_msg}" with title "{safe_title}" sound name "{sound}""#
    );
    let status = std::process::Command::new("osascript")
        .args(["-e", &script])
        .status()
        .with_context(|| "failed to invoke osascript (is this macOS?)")?;
    if !status.success() {
        return Err(anyhow!("osascript exited with {status}"));
    }
    Ok(())
}

fn dispatch_file_drop(p: &Payload<'_>, fd: Option<&FileDropConfig>) -> Result<()> {
    let raw_path = fd
        .and_then(|f| f.path.as_deref())
        .unwrap_or("~/Drop/cosmon-notifications/");
    let dir = expand_home(raw_path);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating drop dir {}", dir.display()))?;
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let slug: String = p
        .title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mol_seg = p.molecule.map(|m| format!("-{m}")).unwrap_or_default();
    let file = dir.join(format!("{ts}-{slug}{mol_seg}.md"));
    let body = format!(
        "# {title}\n\n- level: **{level}**\n- molecule: {mol}\n- ts: {ts}\n\n{msg}\n",
        title = p.title,
        level = p.level.as_str(),
        mol = p.molecule.unwrap_or("(none)"),
        msg = p.message,
    );
    std::fs::write(&file, body).with_context(|| format!("writing {}", file.display()))?;
    Ok(())
}

fn dispatch_element(p: &Payload<'_>, el: Option<&ElementConfig>) -> Result<()> {
    let cfg =
        el.ok_or_else(|| anyhow!("element channel selected but [notify.element] is missing"))?;
    let url = cfg
        .webhook_url
        .as_deref()
        .ok_or_else(|| anyhow!("element webhook_url not set"))?;
    let body = serde_json::json!({
        "title": p.title,
        "message": p.message,
        "molecule": p.molecule,
        "level": p.level.as_str(),
    });

    // Shell out to `curl` rather than depending on an HTTP client crate.
    // The element channel is best-effort and bounded by a short timeout —
    // a missing curl on the host is the operator's signal to switch to
    // the file-drop channel.
    let status = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "5",
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-d",
            &body.to_string(),
            url,
        ])
        .status()
        .with_context(|| "failed to invoke curl for element webhook")?;
    if !status.success() {
        return Err(anyhow!("curl exited with {status}"));
    }
    Ok(())
}

/// Dispatch via the Telegram Bot API `sendMessage` endpoint.
///
/// Sends a plain-text message to `chat_id` using `bot_token`. Outbound-only:
/// `sendMessage` does **not** require the bot to be long-polling, so the same
/// token a long-poll bot uses elsewhere (e.g. notification-bot's `@studio_noog_bot`)
/// can be reused here without contention. The message is rendered as
/// `<title> · <level>\n<message>` and, when present, the molecule id is
/// appended on its own line. Best-effort like every other channel — shells out
/// to `curl` with a short timeout rather than pulling in an HTTP client crate.
fn dispatch_telegram(p: &Payload<'_>, tg: Option<&TelegramConfig>) -> Result<()> {
    let cfg =
        tg.ok_or_else(|| anyhow!("telegram channel selected but [notify.telegram] is missing"))?;
    let token = cfg
        .bot_token
        .as_deref()
        .ok_or_else(|| anyhow!("telegram bot_token not set"))?;
    let chat_id = cfg
        .chat_id
        .as_deref()
        .ok_or_else(|| anyhow!("telegram chat_id not set"))?;

    // One human-readable text body. Telegram has no separate title field, so
    // we fold title + level into the first line, message into the second, and
    // (optionally) the molecule id as a trailing line.
    let mol_line = p
        .molecule
        .map(|mol| format!("\nmolecule: {mol}"))
        .unwrap_or_default();
    let text = format!(
        "{} · {}\n{}{mol_line}",
        p.title,
        p.level.as_str(),
        p.message
    );

    let url = format!("https://api.telegram.org/bot{token}/sendMessage");
    let body = serde_json::json!({
        "chat_id": chat_id,
        "text": text,
    });

    let status = std::process::Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            "5",
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "-d",
            &body.to_string(),
            &url,
        ])
        .status()
        .with_context(|| "failed to invoke curl for telegram sendMessage")?;
    if !status.success() {
        return Err(anyhow!("curl exited with {status}"));
    }
    Ok(())
}

fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(p)
}

// ---------------------------------------------------------------------------
// Config — `.cosmon/config.toml` `[notify]` section
// ---------------------------------------------------------------------------

/// Top-level `[notify]` config block.
///
/// All fields are optional. A missing block is treated as "no channels"
/// — `cs notify` becomes a silent no-op (audit trail still appended via
/// stderr in non-JSON mode). Channel-specific sub-blocks are only consulted
/// when the channel name is in `channels`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Channel names to fire by default. Repeated invocations of `cs notify`
    /// without `--channel` override use this list.
    #[serde(default)]
    pub channels: Vec<String>,
    /// `[notify.macos]` settings.
    #[serde(default)]
    pub macos: Option<MacosConfig>,
    /// `[notify.file-drop]` settings.
    #[serde(default, rename = "file-drop", alias = "file_drop")]
    pub file_drop: Option<FileDropConfig>,
    /// `[notify.element]` settings.
    #[serde(default)]
    pub element: Option<ElementConfig>,
    /// `[notify.telegram]` settings.
    #[serde(default)]
    pub telegram: Option<TelegramConfig>,
}

/// `macOS`-channel settings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct MacosConfig {
    /// `sound name "..."` to pass to `AppleScript`. Default: `"default"`.
    pub sound: Option<String>,
}

/// File-drop-channel settings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FileDropConfig {
    /// Directory the channel writes Markdown files to. Default:
    /// `~/Drop/cosmon-notifications/`. `~` is expanded against `$HOME`.
    pub path: Option<String>,
}

/// Element/Matrix-channel settings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ElementConfig {
    /// Webhook URL the channel POSTs JSON payloads to. Required when the
    /// `element` channel is active.
    pub webhook_url: Option<String>,
}

/// Telegram-channel settings.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TelegramConfig {
    /// Bot token issued by `@BotFather`, used in the
    /// `api.telegram.org/bot<token>/sendMessage` URL. Required when the
    /// `telegram` channel is active. Outbound `sendMessage` does not require
    /// the bot to be long-polling, so a token already in use by a long-poll
    /// bot may be reused here.
    pub bot_token: Option<String>,
    /// Destination chat id — a numeric user id for a private DM, or a
    /// (possibly negative) group/channel id. Required when the `telegram`
    /// channel is active.
    pub chat_id: Option<String>,
}

#[derive(Deserialize)]
struct ConfigToml {
    #[serde(default)]
    notify: Option<Config>,
}

impl Config {
    /// Load the `[notify]` section from a `config.toml` path.
    ///
    /// Returns `Ok(None)` (mapped to default) when the file does not exist
    /// or has no `[notify]` block. Returns an error for malformed TOML.
    fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let parsed: ConfigToml =
            toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        Ok(parsed.notify.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_channel_aliases() {
        assert_eq!(Channel::parse("macos").unwrap(), Channel::Macos);
        assert_eq!(Channel::parse("mac").unwrap(), Channel::Macos);
        assert_eq!(Channel::parse("file-drop").unwrap(), Channel::FileDrop);
        assert_eq!(Channel::parse("file").unwrap(), Channel::FileDrop);
        assert_eq!(Channel::parse("element").unwrap(), Channel::Element);
        assert_eq!(Channel::parse("matrix").unwrap(), Channel::Element);
        assert_eq!(Channel::parse("telegram").unwrap(), Channel::Telegram);
        assert_eq!(Channel::parse("tg").unwrap(), Channel::Telegram);
        assert!(Channel::parse("twitter").is_err());
    }

    #[test]
    fn parse_level_accepts_known_values() {
        assert!(matches!(parse_level("info"), Ok(Level::Info)));
        assert!(matches!(parse_level("warn"), Ok(Level::Warn)));
        assert!(matches!(parse_level("alert"), Ok(Level::Alert)));
        assert!(parse_level("emergency").is_err());
    }

    #[test]
    fn config_load_missing_file_yields_default() {
        let cfg = Config::load(Path::new("/nonexistent/config.toml")).unwrap();
        assert!(cfg.channels.is_empty());
    }

    #[test]
    fn config_load_full_block() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
[notify]
channels = ["macos", "file-drop"]

[notify.macos]
sound = "Glass"

[notify.file-drop]
path = "/tmp/drops"

[notify.element]
webhook_url = "https://example.invalid/hook"

[notify.telegram]
bot_token = "123:ABC"
chat_id = "100000000"
"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        assert_eq!(cfg.channels, vec!["macos", "file-drop"]);
        assert_eq!(
            cfg.macos.as_ref().and_then(|m| m.sound.as_deref()),
            Some("Glass")
        );
        assert_eq!(
            cfg.file_drop.as_ref().and_then(|f| f.path.as_deref()),
            Some("/tmp/drops")
        );
        assert_eq!(
            cfg.element.as_ref().and_then(|e| e.webhook_url.as_deref()),
            Some("https://example.invalid/hook")
        );
        assert_eq!(
            cfg.telegram.as_ref().and_then(|t| t.bot_token.as_deref()),
            Some("123:ABC")
        );
        assert_eq!(
            cfg.telegram.as_ref().and_then(|t| t.chat_id.as_deref()),
            Some("100000000")
        );
    }

    #[test]
    fn dispatch_telegram_requires_token_and_chat() {
        let payload = Payload {
            title: "test",
            message: "automata checkpoint",
            molecule: None,
            level: Level::Info,
        };
        // Missing block → error.
        assert!(dispatch_telegram(&payload, None).is_err());
        // Block present but no token → error.
        let no_token = TelegramConfig {
            bot_token: None,
            chat_id: Some("123".into()),
        };
        assert!(dispatch_telegram(&payload, Some(&no_token)).is_err());
        // Block present but no chat_id → error.
        let no_chat = TelegramConfig {
            bot_token: Some("123:ABC".into()),
            chat_id: None,
        };
        assert!(dispatch_telegram(&payload, Some(&no_chat)).is_err());
    }

    #[test]
    fn resolve_channels_uses_overrides_when_present() {
        let cfg = Config {
            channels: vec!["macos".into()],
            ..Default::default()
        };
        let resolved = resolve_channels(&["file-drop".into()], &cfg).unwrap();
        assert_eq!(resolved, vec![Channel::FileDrop]);
    }

    #[test]
    fn resolve_channels_falls_back_to_config_when_no_override() {
        let cfg = Config {
            channels: vec!["element".into(), "macos".into()],
            ..Default::default()
        };
        let resolved = resolve_channels(&[], &cfg).unwrap();
        assert_eq!(resolved, vec![Channel::Element, Channel::Macos]);
    }

    #[test]
    fn dispatch_file_drop_writes_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = FileDropConfig {
            path: Some(dir.path().to_string_lossy().into_owned()),
        };
        let payload = Payload {
            title: "test",
            message: "worker quartz silent for 240s",
            molecule: Some("cs-20260426-a7e6"),
            level: Level::Warn,
        };
        dispatch_file_drop(&payload, Some(&cfg)).unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert_eq!(entries.len(), 1);
        let body = std::fs::read_to_string(entries[0].path()).unwrap();
        assert!(body.contains("worker quartz silent for 240s"));
        assert!(body.contains("level: **warn**"));
        assert!(body.contains("cs-20260426-a7e6"));
    }

    #[test]
    fn expand_home_replaces_tilde_against_real_home() {
        // Don't mutate $HOME — that races other tests in the same process.
        // Instead read whatever the runner's HOME is and verify the prefix
        // is substituted; the suffix path is tested independently below.
        if let Some(home) = std::env::var_os("HOME") {
            let p = expand_home("~/Drop/cosmon-notifications/");
            let expected = PathBuf::from(home).join("Drop/cosmon-notifications/");
            assert_eq!(p, expected);
        }
        let p2 = expand_home("/abs/path");
        assert_eq!(p2, PathBuf::from("/abs/path"));
    }
}
