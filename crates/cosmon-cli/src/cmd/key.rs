// SPDX-License-Identifier: AGPL-3.0-only

//! `cs key` — operator Ed25519 key management.
//!
//! The notary protocol (ADR-056) signs commitments with an Ed25519
//! secret held in a filesystem key file. Until this command landed, a
//! first-time operator had to read the notary-operator-guide and paste
//! a `python3 -c 'os.urandom(32).hex()'` snippet just to get a file
//! that `cs notarize --key` could read. That is a cliff, not a
//! threshold: anyone cloning cosmon and typing `cs key` hit
//! *"unknown subcommand"*.
//!
//! This module closes the gap with one verb today — `generate` — and
//! leaves room for the rotation-companion verbs (ADR-060) to dock here
//! once the Custody Vault lands.
//!
//! # Storage convention
//!
//! Keys are 64-char lowercase hex strings (32 raw bytes, encoded) — the
//! exact shape `cs notarize --key <path>` and the ILB / operator-demo
//! `verify.sh` scripts already expect. The default path is
//! `~/.config/cosmon/operator.key` (matching every Makefile, doc, and
//! chronicle on disk). Mode is `0o600` on Unix; on non-Unix we fall
//! through with whatever default the filesystem gives, since the
//! notary protocol is not a Windows deployment target.
//!
//! # Intentional non-goals
//!
//! - **No rotation.** Retirement / successor-key publication is
//!   ADR-060's job and must flow through the notary event log, not a
//!   filesystem overwrite.
//! - **No HSM / PKCS#11 proxy.** The Custody Vault (S4) will abstract
//!   over these. `cs key generate` is deliberately the minimal
//!   filesystem-only path.
//! - **No PEM / OpenSSH wrapping.** `cs notarize` refuses those
//!   formats; generating one here would be a footgun.

use std::path::{Path, PathBuf};

use cosmon_notary::{Ed25519Scheme, Scheme};

use super::Context;

/// Arguments for the `key` subcommand.
#[derive(clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: KeyCommand,
}

/// `cs key` subcommands.
#[derive(clap::Subcommand)]
pub enum KeyCommand {
    /// Generate a fresh Ed25519 operator key (32 bytes OS randomness, hex-encoded).
    Generate(GenerateArgs),
    /// Print the public key (and its path) for an existing operator key file.
    Show(ShowArgs),
}

/// Arguments for `cs key generate`.
#[derive(clap::Args)]
pub struct GenerateArgs {
    /// Destination path. Defaults to `~/.config/cosmon/operator.key`.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Overwrite an existing key file. Without this flag, refuses to
    /// clobber any pre-existing key — silent rotation is forbidden
    /// (ADR-060 §Alternatives-rejected).
    #[arg(long)]
    pub force: bool,
}

/// Arguments for `cs key show`.
#[derive(clap::Args)]
pub struct ShowArgs {
    /// Path to the operator key file. Defaults to
    /// `~/.config/cosmon/operator.key`.
    #[arg(long, value_name = "PATH")]
    pub key: Option<PathBuf>,
}

/// Dispatch `cs key <sub>`.
///
/// # Errors
/// Propagates errors from subcommand handlers.
pub fn run(ctx: &Context, args: &Args) -> anyhow::Result<()> {
    match &args.command {
        KeyCommand::Generate(a) => run_generate(ctx, a),
        KeyCommand::Show(a) => run_show(ctx, a),
    }
}

fn run_generate(ctx: &Context, args: &GenerateArgs) -> anyhow::Result<()> {
    let path = args
        .output
        .clone()
        .unwrap_or_else(default_operator_key_path);

    if path.exists() && !args.force {
        anyhow::bail!(
            "key file already exists at {} — refusing to overwrite without --force \
             (rotation is a distinct ceremony, see ADR-060)",
            path.display()
        );
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            anyhow::anyhow!(
                "could not create parent directory {}: {e}",
                parent.display()
            )
        })?;
    }

    let scheme = Ed25519Scheme::generate();
    let secret = scheme.secret_bytes();
    let hex = hex_encode(&secret);

    write_secret(&path, &hex)?;
    let pubkey = scheme.public_key();

    if ctx.json {
        let out = serde_json::json!({
            "path": path.display().to_string(),
            "public_key_hex": pubkey.bytes_hex,
            "scheme": pubkey.tag,
        });
        println!("{out}");
    } else {
        println!("generated new ed25519 operator key");
        println!("  path       : {}", path.display());
        println!("  public key : {}", pubkey.bytes_hex);
        println!();
        println!("next step: cs notarize <mol_id> --key {}", path.display());
    }
    Ok(())
}

fn run_show(ctx: &Context, args: &ShowArgs) -> anyhow::Result<()> {
    let path = args.key.clone().unwrap_or_else(default_operator_key_path);

    let scheme = load_scheme(&path)?;
    let pubkey = scheme.public_key();

    if ctx.json {
        let out = serde_json::json!({
            "path": path.display().to_string(),
            "public_key_hex": pubkey.bytes_hex,
            "scheme": pubkey.tag,
        });
        println!("{out}");
    } else {
        println!("path       : {}", path.display());
        println!("public key : {}", pubkey.bytes_hex);
    }
    Ok(())
}

/// Resolve the default operator-key path.
///
/// Honours `COSMON_CONFIG_HOME` for test isolation (same override as
/// [`super::opt_in_share::config_base_dir`]), then [`dirs::config_dir`],
/// then `~/.config/` as a last resort. The file name is always
/// `cosmon/operator.key` — that matches every Makefile, guide, and
/// chronicle on disk.
fn default_operator_key_path() -> PathBuf {
    super::opt_in_share::config_base_dir()
        .join("cosmon")
        .join("operator.key")
}

fn write_secret(path: &Path, hex: &str) -> anyhow::Result<()> {
    // Write the hex string with no trailing newline — that is the
    // exact shape `cs notarize --key` (see `load_ed25519_scheme` in
    // cmd/notarize.rs) and the Makefiles under `theater/` already
    // produce. Adding a newline would still parse, but roundtripping
    // through `cs key show` depends on a stable on-disk form.
    std::fs::write(path, hex.as_bytes())
        .map_err(|e| anyhow::anyhow!("could not write key file {}: {e}", path.display()))?;
    set_mode_0600(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_mode_0600(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
        .map_err(|e| anyhow::anyhow!("chmod 0600 {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_mode_0600(_path: &Path) -> anyhow::Result<()> {
    // No-op on non-Unix. The notary protocol's deployment targets are
    // Linux and macOS; Windows operators are expected to use the
    // platform's native ACL tooling.
    Ok(())
}

/// Load an [`Ed25519Scheme`] from a key file.
///
/// Accepts the same two forms [`super::notarize::run`] accepts:
/// 32 raw bytes, or a 64-char lowercase hex string (trailing whitespace
/// allowed). This keeps `cs key show` and `cs notarize --key` in sync —
/// a file that works for one must work for the other.
fn load_scheme(path: &Path) -> anyhow::Result<Ed25519Scheme> {
    let raw = std::fs::read(path)
        .map_err(|e| anyhow::anyhow!("could not read key file {}: {e}", path.display()))?;
    if raw.len() == 32 {
        return Ed25519Scheme::from_secret_bytes(&raw).map_err(|e| anyhow::anyhow!("{e}"));
    }
    let trimmed = std::str::from_utf8(&raw)
        .map_err(|e| anyhow::anyhow!("key file is not 32 raw bytes and not UTF-8 hex: {e}"))?
        .trim();
    if trimmed.len() != 64 {
        anyhow::bail!(
            "key file must be 32 raw bytes or 64-char hex; got {} chars",
            trimmed.len()
        );
    }
    let bytes = hex_decode(trimmed)?;
    Ed25519Scheme::from_secret_bytes(&bytes).map_err(|e| anyhow::anyhow!("{e}"))
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn hex_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        anyhow::bail!("hex string has odd length");
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks(2) {
        let hi = nibble(chunk[0])?;
        let lo = nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nibble(b: u8) -> anyhow::Result<u8> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        other => anyhow::bail!("non-hex character: {}", char::from(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cs-key-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn ctx() -> Context {
        Context {
            verbose: false,
            json: false,
            config: None,
        }
    }

    #[test]
    fn generate_writes_64_char_hex() {
        let path = tmp_path("gen-basic.key");
        let _ = std::fs::remove_file(&path);
        run_generate(
            &ctx(),
            &GenerateArgs {
                output: Some(path.clone()),
                force: false,
            },
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.len(), 64, "expected 64-char hex, got {raw:?}");
        assert!(
            raw.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
            "expected lowercase hex, got {raw:?}"
        );
    }

    #[test]
    fn generate_refuses_clobber_without_force() {
        let path = tmp_path("gen-refuse.key");
        std::fs::write(&path, "preexisting").unwrap();
        let err = run_generate(
            &ctx(),
            &GenerateArgs {
                output: Some(path.clone()),
                force: false,
            },
        )
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("already exists"),
            "expected clobber guard, got {msg}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "preexisting");
    }

    #[test]
    fn generate_force_overwrites() {
        let path = tmp_path("gen-force.key");
        std::fs::write(&path, "preexisting").unwrap();
        run_generate(
            &ctx(),
            &GenerateArgs {
                output: Some(path.clone()),
                force: true,
            },
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.len(), 64);
    }

    #[cfg(unix)]
    #[test]
    fn generate_sets_0600_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmp_path("gen-mode.key");
        let _ = std::fs::remove_file(&path);
        run_generate(
            &ctx(),
            &GenerateArgs {
                output: Some(path.clone()),
                force: false,
            },
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "expected mode 0600, got {mode:o}");
    }

    #[test]
    fn show_roundtrips_public_key() {
        let path = tmp_path("show-roundtrip.key");
        let _ = std::fs::remove_file(&path);
        run_generate(
            &ctx(),
            &GenerateArgs {
                output: Some(path.clone()),
                force: false,
            },
        )
        .unwrap();
        // Direct load — `show` prints to stdout so we test the load path.
        let scheme = load_scheme(&path).unwrap();
        let pk = scheme.public_key();
        assert_eq!(pk.tag, Ed25519Scheme::TAG);
        assert_eq!(pk.bytes_hex.len(), 64);
    }

    #[test]
    fn load_rejects_wrong_length() {
        let path = tmp_path("bad-length.key");
        std::fs::write(&path, "deadbeef").unwrap();
        assert!(load_scheme(&path).is_err());
    }

    #[test]
    fn hex_roundtrip() {
        let bytes = vec![0u8, 1, 2, 3, 255];
        assert_eq!(hex_encode(&bytes), "00010203ff");
        assert_eq!(hex_decode("00010203ff").unwrap(), bytes);
    }
}
