// SPDX-License-Identifier: AGPL-3.0-only

//! Operator-side harvest workflow. Only the external signer sees a private key.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{Duration, Utc};
use clap::{Subcommand, ValueEnum};
use cosmon_core::harvest_authorization::{
    harvest_root_rotation_statement, policy_digest, DoneAuthorization, GrantEpoch, HarvestGrant,
    HarvestScope,
};
use cosmon_core::id::MoleculeId;
use cosmon_core::operator_attestation::{OperatorAttestation, OperatorKeyId};
use cosmon_notary::minisign::{self, MinisignPublicKey, MinisignSignature};
use cosmon_remote::client::Client;
use cosmon_remote::config::Profile;
use cosmon_remote::error::{Error, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The three explicit remote policy selections.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PolicyArg {
    /// Refuse all remote harvest requests.
    Disabled,
    /// Admit ordinary requests under the harvest scope.
    Scoped,
    /// Require a current independently signed grant.
    Sealed,
}

impl PolicyArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Scoped => "scoped",
            Self::Sealed => "sealed",
        }
    }
}

/// Operator harvest commands. Grant issuance and signing run on this device.
#[derive(Debug, Subcommand)]
pub enum HarvestCmd {
    /// Compare and set the selected remote policy using the admin credential.
    Configure {
        /// New explicit policy for the tenant galaxy.
        #[arg(long, value_enum)]
        policy: PolicyArg,
        /// File holding the independent host admin credential.
        #[arg(long)]
        admin_token_file: PathBuf,
    },
    /// Install a public root; rotation requires a signature from the current key.
    Init {
        /// File holding the independent host admin credential.
        #[arg(long)]
        admin_token_file: PathBuf,
        /// Required when replacing an installed public root.
        #[arg(long)]
        rotate_from: Option<String>,
        /// Store a key on a selected operator device instead of the default directory.
        #[arg(long)]
        key_file: Option<PathBuf>,
        /// Select the current signer for rotation when it is on another device.
        #[arg(long, requires = "rotate_from")]
        current_key_file: Option<PathBuf>,
    },
    /// Build, sign and install a grant, or split the workflow into offline steps.
    Grant {
        /// Issue for exactly this molecule.
        #[arg(long, conflicts_with = "mission")]
        molecule: Option<String>,
        /// Delegate to a mission rooted at this molecule.
        #[arg(long)]
        mission: Option<String>,
        /// Grant lifetime in minutes, hours or days (default: 1h).
        #[arg(long, conflicts_with = "no_expiry")]
        expires_in: Option<String>,
        /// Keep the grant valid until the epoch changes.
        #[arg(long)]
        no_expiry: bool,
        /// Write a checked challenge for offline signing.
        #[arg(long, conflicts_with_all = ["sign", "import"])]
        export: Option<PathBuf>,
        /// Sign a previously exported challenge on this device.
        #[arg(long, conflicts_with = "import")]
        sign: Option<PathBuf>,
        /// Upload a previously signed grant without consuming it.
        #[arg(long)]
        import: Option<PathBuf>,
        /// Output file for --sign (defaults to <challenge>.signed.json).
        #[arg(long, requires = "sign")]
        output: Option<PathBuf>,
        /// Select an operator-owned key, including one on a removable device.
        #[arg(long)]
        key_file: Option<PathBuf>,
    },
    /// Inspect effective policy, provenance, public root and grant validity.
    Status {
        /// Include installed grant validity for this molecule.
        #[arg(long)]
        molecule: Option<String>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeFile {
    molecule: String,
    grant: HarvestGrant,
    canonical: String,
    fingerprint: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedFile {
    molecule: String,
    authorization: DoneAuthorization,
}

fn fault(message: impl Into<String>) -> Error {
    Error::Config(message.into())
}

fn field<'a>(body: &'a Value, name: &str) -> Result<&'a str> {
    body.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| fault(format!("harvest response is missing {name}")))
}

fn epoch(body: &Value) -> Result<u64> {
    body.get("epoch")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(|| fault("harvest response has no valid epoch"))
}

fn expected_policy(status: &Value) -> Result<Option<&str>> {
    match field(status, "provenance")? {
        "legacy" => Ok(None),
        "explicit" => Ok(Some(field(status, "policy")?)),
        _ => Err(fault("harvest status has unknown policy provenance")),
    }
}

fn cas_body(
    status: &Value,
    policy: &str,
    public_key: Option<&str>,
    next_epoch: Option<u64>,
) -> Result<Value> {
    Ok(json!({
        "expected_policy": expected_policy(status)?,
        "expected_key_digest": status.get("key_fingerprint").unwrap_or(&Value::Null),
        "expected_epoch": epoch(status)?,
        "policy": policy,
        "public_key": public_key,
        "epoch": next_epoch,
    }))
}

fn admin_token(path: &Path) -> Result<String> {
    let token = fs::read_to_string(path)?;
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(fault("admin token file is empty or malformed"));
    }
    Ok(token.to_owned())
}

fn noyau(profile: &Profile) -> Result<&str> {
    profile
        .noyau
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| fault("profile has no noyau; set it before harvest administration"))
}

fn operator_dir(profile_name: &str) -> Result<PathBuf> {
    if profile_name.is_empty()
        || profile_name == "."
        || profile_name == ".."
        || !profile_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(fault("unsafe profile name for harvest key storage"));
    }
    let base = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(path) => PathBuf::from(path),
        None => dirs::home_dir()
            .ok_or_else(|| fault("operator home is unavailable"))?
            .join(".config"),
    };
    let dir = base.join("cosmon/harvest/keys").join(profile_name);
    guard_key_location(&dir.join("pending.key"))?;
    Ok(dir)
}

fn guard_operator_device() -> Result<()> {
    if [
        "COSMON_MOL_DIR",
        "COSMON_ARTIFACT_DIR",
        "COSMON_API_REQUEST",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some())
    {
        return Err(fault(
            "harvest signing requires an independent operator device",
        ));
    }
    Ok(())
}

fn guard_key_location(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| fault("harvest key has no parent directory"))?;
    let existing = parent
        .ancestors()
        .find(|candidate| candidate.exists())
        .ok_or_else(|| fault("harvest key path has no existing ancestor"))?;
    let resolved = fs::canonicalize(existing)?;
    if resolved.ancestors().any(|candidate| {
        candidate.join(".git").exists() || candidate.join(".cosmon/config.toml").exists()
    }) || parent
        .components()
        .any(|part| part.as_os_str() == ".worktrees")
    {
        return Err(fault(
            "harvest key location must be outside a galaxy or worktree",
        ));
    }
    Ok(())
}

fn protected_dir(path: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(fault("harvest key directory is not a plain directory"));
        }
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn check_key(path: &Path) -> Result<()> {
    guard_key_location(path)?;
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(fault("harvest key is not a plain file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(fault("harvest key permissions must be 0600"));
        }
    }
    let mut first = String::new();
    std::io::BufReader::new(fs::File::open(path)?).read_line(&mut first)?;
    if first != "untrusted comment: minisign encrypted secret key\n" {
        return Err(fault("harvest key must be encrypted by minisign"));
    }
    Ok(())
}

fn public_for(key: &Path) -> PathBuf {
    key.with_extension("pub")
}

fn signer_key(dir: &Path, selected: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = selected {
        check_key(&path)?;
        return Ok(path);
    }
    let mut keys = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "key") {
            check_key(&path)?;
            keys.push(path);
        }
    }
    match keys.len() {
        1 => Ok(keys.remove(0)),
        0 => Err(fault("no operator harvest key; run harvest init")),
        _ => Err(fault("multiple harvest keys; select one with --key-file")),
    }
}

fn current_rotation_key(dir: &Path, selected: Option<PathBuf>, installed: &str) -> Result<PathBuf> {
    if let Some(path) = selected {
        check_key(&path)?;
        let (text, _) = read_public(&path)?;
        if policy_digest(text.as_bytes()) != installed {
            return Err(fault("current key does not match installed public root"));
        }
        return Ok(path);
    }
    let mut matches = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|extension| extension == "key") {
            check_key(&path)?;
            let (text, _) = read_public(&path)?;
            if policy_digest(text.as_bytes()) == installed {
                matches.push(path);
            }
        }
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        _ => Err(fault(
            "select the installed current key with --current-key-file",
        )),
    }
}

fn sign_rotation(statement: &[u8], key: &Path, scratch: &Path) -> Result<String> {
    check_key(key)?;
    let (_, public) = read_public(key)?;
    let message = random_path(scratch, "rotation");
    let signature = message.with_extension("minisig");
    write_new(&message, statement)?;
    let result = Command::new("minisign")
        .args(["-S", "-s"])
        .arg(key)
        .arg("-m")
        .arg(&message)
        .arg("-x")
        .arg(&signature)
        .arg("-q")
        .stdout(signer_stdout()?)
        .status();
    let signed = match result {
        Ok(status) if status.success() => fs::read_to_string(&signature).map_err(Error::Io),
        Ok(_) => Err(fault("minisign declined root rotation signing")),
        Err(_) => Err(fault("minisign is required on the operator device")),
    };
    let _ = fs::remove_file(&message);
    let _ = fs::remove_file(&signature);
    let signed = signed?;
    let parsed = MinisignSignature::parse(&signed)
        .map_err(|_| fault("signer returned a malformed rotation signature"))?;
    minisign::verify(&public, statement, &parsed)
        .map_err(|_| fault("signer returned an invalid rotation signature"))?;
    Ok(signed)
}

fn read_public(key: &Path) -> Result<(String, MinisignPublicKey)> {
    let public_path = public_for(key);
    let meta = fs::symlink_metadata(&public_path)?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(fault("operator public key is not a plain file"));
    }
    let text = fs::read_to_string(public_path)?;
    let parsed =
        MinisignPublicKey::parse(&text).map_err(|_| fault("operator public key is malformed"))?;
    Ok((text, parsed))
}

fn signer_stdout() -> Result<Stdio> {
    #[cfg(unix)]
    {
        Ok(Stdio::from(
            OpenOptions::new().write(true).open("/dev/stderr")?,
        ))
    }
    #[cfg(not(unix))]
    {
        Ok(Stdio::inherit())
    }
}

fn generate_key(key: &Path) -> Result<()> {
    guard_key_location(key)?;
    if key.exists() || public_for(key).exists() {
        return Err(fault("harvest key already exists; refusing overwrite"));
    }
    let status = Command::new("minisign")
        .args(["-G", "-s"])
        .arg(key)
        .arg("-p")
        .arg(public_for(key))
        .stdout(signer_stdout()?)
        .status()
        .map_err(|_| fault("minisign is required on the operator device"))?;
    if !status.success() {
        return Err(fault("minisign declined key generation"));
    }
    check_key(key)?;
    Ok(())
}

fn finalize_pending(dir: &Path, key: &Path, public: &MinisignPublicKey) -> Result<()> {
    if key != dir.join("pending.key") {
        return Ok(());
    }
    let id = OperatorKeyId::from_bytes(public.key_id()).to_string();
    let final_key = dir.join(format!("{id}.key"));
    if final_key.exists() || public_for(&final_key).exists() {
        return Err(fault(
            "installed key is pending but final operator path already exists",
        ));
    }
    fs::rename(public_for(key), public_for(&final_key))?;
    fs::rename(key, &final_key)?;
    Ok(())
}

fn random_path(dir: &Path, suffix: &str) -> PathBuf {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    let name = bytes.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
        out
    });
    dir.join(format!(".{name}.{suffix}"))
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn checked_challenge(body: &Value, target: &str, kind: &str) -> Result<ChallengeFile> {
    let grant: HarvestGrant = serde_json::from_value(
        body.get("grant")
            .cloned()
            .ok_or_else(|| fault("challenge has no grant"))?,
    )?;
    grant
        .validate()
        .map_err(|_| fault("challenge grant is malformed"))?;
    let canonical = field(body, "canonical")?;
    let fingerprint = field(body, "fingerprint")?;
    if canonical.as_bytes() != grant.canonical_bytes()
        || fingerprint != grant.fingerprint().as_str()
        || !canonical.starts_with("cosmon-harvest-grant-v1\n")
    {
        return Err(fault("opaque or inconsistent harvest challenge refused"));
    }
    let selected = MoleculeId::new(target).map_err(|_| fault("invalid harvest target"))?;
    let matches_target = match (&grant.scope, kind) {
        (HarvestScope::Molecule { molecule }, "molecule") => molecule == &selected,
        (HarvestScope::Mission { mission, .. }, "mission") => mission == &selected,
        _ => false,
    };
    if !matches_target || grant.expires.is_some_and(|at| at <= Utc::now()) {
        return Err(fault(
            "harvest challenge does not match the selected target or has expired",
        ));
    }
    Ok(ChallengeFile {
        molecule: target.to_owned(),
        grant,
        canonical: canonical.to_owned(),
        fingerprint: fingerprint.to_owned(),
    })
}

fn check_saved_challenge(challenge: &ChallengeFile) -> Result<()> {
    let value = json!({
        "grant": challenge.grant,
        "canonical": challenge.canonical,
        "fingerprint": challenge.fingerprint,
    });
    let kind = match &challenge.grant.scope {
        HarvestScope::Molecule { .. } => "molecule",
        HarvestScope::Mission { .. } => "mission",
    };
    checked_challenge(&value, &challenge.molecule, kind)?;
    Ok(())
}

fn sign_challenge(challenge: &ChallengeFile, key: &Path, scratch: &Path) -> Result<SignedFile> {
    guard_operator_device()?;
    check_saved_challenge(challenge)?;
    check_key(key)?;
    let (_, public) = read_public(key)?;
    protected_dir(scratch)?;
    let message = random_path(scratch, "challenge");
    let signature = message.with_extension("minisig");
    write_new(&message, challenge.canonical.as_bytes())?;
    eprintln!(
        "Review the exact grant before approving the external signer:\n{}",
        challenge.canonical
    );
    let result = Command::new("minisign")
        .args(["-S", "-s"])
        .arg(key)
        .arg("-m")
        .arg(&message)
        .arg("-x")
        .arg(&signature)
        .arg("-q")
        .stdout(signer_stdout()?)
        .status();
    let signed = match result {
        Ok(status) if status.success() => fs::read_to_string(&signature).map_err(Error::Io),
        Ok(_) => Err(fault("minisign declined harvest signing")),
        Err(_) => Err(fault("minisign is required on the operator device")),
    };
    let _ = fs::remove_file(&message);
    let _ = fs::remove_file(&signature);
    let signed = signed?;
    let parsed = MinisignSignature::parse(&signed)
        .map_err(|_| fault("signer returned a malformed signature"))?;
    minisign::verify(&public, challenge.canonical.as_bytes(), &parsed)
        .map_err(|_| fault("signer returned a signature that does not verify"))?;
    let attestation = OperatorAttestation {
        key_id: OperatorKeyId::from_bytes(parsed.key_id),
        signature: parsed.signature_line(),
        global_signature: parsed.global_signature_line(),
        trusted_comment: parsed.trusted_comment,
        untrusted_comment: parsed.untrusted_comment,
    };
    let authority = match &challenge.grant.scope {
        HarvestScope::Mission { .. } => "delegated",
        HarvestScope::Molecule { .. } => "ratified",
    };
    let authorization: DoneAuthorization = serde_json::from_value(json!({
        "authority": authority,
        "grant": challenge.grant,
        "attestation": attestation,
    }))?;
    Ok(SignedFile {
        molecule: challenge.molecule.clone(),
        authorization,
    })
}

fn expiry_value(raw: &str) -> Result<String> {
    let (digits, unit) = raw.split_at(raw.len().saturating_sub(1));
    let count: i64 = digits
        .parse()
        .map_err(|_| fault("--expires-in expects a positive duration such as 1h"))?;
    if count <= 0 {
        return Err(fault("--expires-in must be positive"));
    }
    let seconds = match unit {
        "m" => count.checked_mul(60),
        "h" => count.checked_mul(3600),
        "d" => count.checked_mul(86400),
        _ => None,
    }
    .ok_or_else(|| fault("--expires-in expects minutes, hours or days"))?;
    let expiry = Utc::now()
        .checked_add_signed(Duration::seconds(seconds))
        .ok_or_else(|| fault("--expires-in is outside the supported range"))?;
    Ok(expiry.to_rfc3339())
}

fn render(json_mode: bool, body: &Value, success: &str) {
    if json_mode {
        println!("{body}");
    } else {
        println!("{success}");
    }
}

fn render_status(json_mode: bool, status: &Value) -> Result<()> {
    if json_mode {
        println!("{status}");
        return Ok(());
    }
    println!(
        "policy: {} ({})",
        field(status, "policy")?,
        field(status, "provenance")?
    );
    println!("required scope: {}", field(status, "required_scope")?);
    println!(
        "executor supported: {}",
        status
            .get("executor_supported")
            .and_then(Value::as_bool)
            .ok_or_else(|| fault("harvest status lacks executor support"))?
    );
    println!(
        "public key: {}",
        status
            .get("key_fingerprint")
            .and_then(Value::as_str)
            .unwrap_or("absent")
    );
    println!("epoch: {}", epoch(status)?);
    if let Some(grant) = status.get("grant").filter(|value| !value.is_null()) {
        println!(
            "matching grant valid: {}",
            grant
                .get("valid")
                .and_then(Value::as_bool)
                .ok_or_else(|| fault("harvest status lacks grant validity"))?
        );
    }
    Ok(())
}

/// Execute one operator harvest command against a selected deployment profile.
#[allow(clippy::too_many_lines)] // each branch is one complete operator gesture
pub async fn run(
    cmd: HarvestCmd,
    profile_name: &str,
    profile: &Profile,
    client: &Client,
    json_mode: bool,
) -> Result<()> {
    match cmd {
        HarvestCmd::Status { molecule } => {
            let status = client.harvest_status(molecule.as_deref()).await?;
            render_status(json_mode, &status)?;
        }
        HarvestCmd::Configure {
            policy,
            admin_token_file,
        } => {
            let before = client.harvest_status(None).await?;
            let body = cas_body(&before, policy.as_str(), None, None)?;
            let result = client
                .harvest_configure(noyau(profile)?, &admin_token(&admin_token_file)?, &body)
                .await?;
            render(json_mode, &result, "harvest policy configured");
        }
        HarvestCmd::Init {
            admin_token_file,
            rotate_from,
            key_file,
            current_key_file,
        } => {
            guard_operator_device()?;
            let before = client.harvest_status(None).await?;
            let dir = operator_dir(profile_name)?;
            protected_dir(&dir)?;
            let key = if let Some(path) = key_file {
                if path.exists() {
                    check_key(&path)?;
                } else {
                    generate_key(&path)?;
                }
                path
            } else if rotate_from.is_some() {
                let path = dir.join("pending.key");
                if path.exists() {
                    check_key(&path)?;
                } else {
                    generate_key(&path)?;
                }
                path
            } else {
                let pending = dir.join("pending.key");
                if pending.exists() {
                    check_key(&pending)?;
                    pending
                } else {
                    let mut keys = fs::read_dir(&dir)?
                        .filter_map(|entry| entry.ok().map(|e| e.path()))
                        .filter(|path| path.extension().is_some_and(|ext| ext == "key"));
                    match (keys.next(), keys.next()) {
                        (Some(path), None) => {
                            check_key(&path)?;
                            path
                        }
                        (None, _) => {
                            generate_key(&pending)?;
                            pending
                        }
                        _ => {
                            return Err(fault("multiple harvest keys; select one with --key-file"))
                        }
                    }
                }
            };
            let (public_text, public) = read_public(&key)?;
            let digest = policy_digest(public_text.as_bytes());
            let installed = before.get("key_fingerprint").and_then(Value::as_str);
            if rotate_from.is_some() && rotate_from.as_deref() != installed {
                return Err(fault(
                    "rotation fingerprint does not match current public root",
                ));
            }
            if installed == Some(digest.as_str()) && field(&before, "policy")? == "sealed" {
                finalize_pending(&dir, &key, &public)?;
                render(
                    json_mode,
                    &before,
                    "harvest key and sealed policy already installed",
                );
                return Ok(());
            }
            if installed.is_some()
                && installed != Some(digest.as_str())
                && rotate_from.as_deref() != installed
            {
                return Err(fault(
                    "existing public root differs; pass --rotate-from with its fingerprint",
                ));
            }
            let next_epoch = if installed.is_some() && installed != Some(digest.as_str()) {
                Some(
                    epoch(&before)?
                        .checked_add(1)
                        .ok_or_else(|| fault("epoch exhausted"))?,
                )
            } else {
                None
            };
            let mut body = cas_body(&before, "sealed", Some(&public_text), next_epoch)?;
            if let (Some(prior), Some(next)) = (installed, next_epoch) {
                let current_key = current_rotation_key(&dir, current_key_file, prior)?;
                let statement = harvest_root_rotation_statement(
                    noyau(profile)?,
                    prior,
                    &public_text,
                    GrantEpoch::from_u64(next),
                )
                .map_err(|_| fault("invalid rotation statement"))?;
                eprintln!(
                    "Review the exact root rotation before signing:\n{}",
                    String::from_utf8_lossy(&statement)
                );
                body["rotation_signature"] = json!(sign_rotation(&statement, &current_key, &dir)?);
            }
            let result = client
                .harvest_configure(noyau(profile)?, &admin_token(&admin_token_file)?, &body)
                .await?;
            finalize_pending(&dir, &key, &public)?;
            render(
                json_mode,
                &result,
                "public harvest key and sealed policy installed",
            );
        }
        HarvestCmd::Grant {
            molecule,
            mission,
            expires_in,
            no_expiry,
            export,
            sign,
            import,
            output,
            key_file,
        } => {
            let modes = usize::from(export.is_some())
                + usize::from(sign.is_some())
                + usize::from(import.is_some());
            if modes > 1 {
                return Err(fault("--export, --sign and --import are exclusive"));
            }
            if let Some(file) = import {
                if molecule.is_some()
                    || mission.is_some()
                    || output.is_some()
                    || key_file.is_some()
                    || expires_in.is_some()
                    || no_expiry
                {
                    return Err(fault(
                        "--import reads its target and authority from the signed file",
                    ));
                }
                let signed: SignedFile = serde_json::from_slice(&fs::read(file)?)?;
                signed
                    .authorization
                    .validate()
                    .map_err(|_| fault("signed grant is malformed"))?;
                let result = client
                    .harvest_import(&serde_json::to_value(signed)?)
                    .await?;
                render(
                    json_mode,
                    &result,
                    "harvest grant installed; no merge was performed",
                );
                return Ok(());
            }
            let dir = operator_dir(profile_name)?;
            if let Some(file) = sign {
                if molecule.is_some() || mission.is_some() || expires_in.is_some() || no_expiry {
                    return Err(fault("--sign reads its target from the challenge file"));
                }
                let challenge: ChallengeFile = serde_json::from_slice(&fs::read(&file)?)?;
                let key = signer_key(&dir, key_file)?;
                let signed = sign_challenge(&challenge, &key, &dir)?;
                let destination = output
                    .unwrap_or_else(|| PathBuf::from(format!("{}.signed.json", file.display())));
                write_new(&destination, &serde_json::to_vec_pretty(&signed)?)?;
                render(
                    json_mode,
                    &json!({"signed_file": destination}),
                    "harvest grant signed locally; import the signed file to install it",
                );
                return Ok(());
            }
            let (kind, target) = match (molecule, mission) {
                (Some(target), None) => ("molecule", target),
                (None, Some(target)) => ("mission", target),
                _ => return Err(fault("select exactly one of --molecule or --mission")),
            };
            let mut request = serde_json::Map::new();
            request.insert(kind.to_owned(), json!(target));
            if no_expiry {
                request.insert("no_expiry".to_owned(), json!(true));
            } else {
                request.insert(
                    "expires_at".to_owned(),
                    json!(expiry_value(expires_in.as_deref().unwrap_or("1h"))?),
                );
            }
            let request = Value::Object(request);
            let response = client.harvest_challenge(&request).await?;
            let challenge = checked_challenge(&response, &target, kind)?;
            if let Some(file) = export {
                if key_file.is_some() {
                    return Err(fault("--export creates no signature and accepts no key"));
                }
                write_new(&file, &serde_json::to_vec_pretty(&challenge)?)?;
                render(
                    json_mode,
                    &json!({"challenge_file": file}),
                    "harvest challenge exported; no authority was issued",
                );
                return Ok(());
            }
            let key = signer_key(&dir, key_file)?;
            let signed = sign_challenge(&challenge, &key, &dir)?;
            let result = client
                .harvest_import(&serde_json::to_value(signed)?)
                .await?;
            render(
                json_mode,
                &result,
                "harvest grant installed; no merge was performed",
            );
        }
    }
    Ok(())
}
