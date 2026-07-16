# `scripts/share-telemetry.sh` — the paper airplane

Project a cosmon molecule onto the **share-telemetry surface** — a bundle
carrying only the seven fields the panel agreed are safe to ship. Everything
else stays on disk, REDACTED by name.

## What it does

1. Reads `state.json` for `<molecule_id>` via `cs observe --json`.
2. Projects the seven **PUBLIC** fields (delib-20260419-fe35 §Conv. #6,
   inherited from delib-6bb0 shannon list): `cosmon_version`, `formula_id`,
   `step_durations_ms`, `exit_status`, `error_class`, `energy_bucket`,
   `worker_model_family`.
3. Lists the seven **REDACTED** fields by name with `<REDACTED:<type>>`:
   `molecule_id`, `prompt_content`, `git_sha`, `topic`, `file_paths`,
   `timestamps_absolute`, `variables`.
4. **Gate:** runs `cs doctor leaks --corpus $COSMON_LEAK_CORPUS` on the
   candidate bundle. If the scan flags anything, the script **refuses**.
   *Share = scan-then-emit, atomic, or refuse* (fe35 §Conv. #3).
5. Emits a two-column diff to STDOUT. `--out <path>` writes the JSON
   bundle *after* the scan has passed.

## Usage

    scripts/share-telemetry.sh <molecule_id> --dry-run [--out <path|age:[PUBKEY]>]

`--dry-run` is required — the non-dry-run path ships LATER.

Env: `CS` (binary path), `COSMON_LEAK_CORPUS` (defaults to
`~/.config/cosmon/leak-corpus.toml`), `COSMON_MODEL_FAMILY`,
`COSMON_DEFAULT_RECIPIENT` (defaults to `~/.config/cosmon/default-recipient.age`),
`COSMON_TELEMETRY_OUTGOING` (defaults to `~/cosmon-telemetry/outgoing`).

Exit codes: `0` clean · `2` usage · `3` missing dep · `4` molecule not
found · `5` not a git repo · `6` corpus missing · `7` REFUSED ·
`8` age encryption failed.

## Encryption (`--out age:`)

For ADDL delivery you want the bundle sealed at rest: only the recipient's
age key can open it. The script supports three `--out` shapes:

- `--out <path>` — clear JSON at `<path>` (existing behaviour).
- `--out age:` — encrypt with the default recipient pubkey read from
  `~/.config/cosmon/default-recipient.age`, drop to
  `~/cosmon-telemetry/outgoing/<mol_id>-<UTC-ts>.bundle.age`.
- `--out age:<PUBKEY>` — encrypt with the given age recipient
  (must start with `age1`), drop to the same default outgoing location.

**Atomicity.** The flow is strictly **scan-then-encrypt-then-emit**:
`cs doctor leaks --corpus` runs on the PUBLIC bundle FIRST; only if the
scan passes does the script pipe `bundle_json` through
`age --encrypt --recipient <PUB> --output <drop_path>`. If the scan
refuses, nothing is encrypted and nothing is written — the bundle cannot
leave the machine in any form. Inversion would let a leaky field be
sealed by an attacker's key and then legitimately emitted.

**Decryption.** The recipient decrypts with their age private key:

    age --decrypt -i ~/.config/age/you.key.txt \
      ~/cosmon-telemetry/outgoing/<mol_id>-<ts>.bundle.age

This returns the original JSON bundle (public + redacted fields), byte-
identical to the clear `--out <path>` output.

Dependency: `age` must be on `$PATH` (`brew install age`).

## Sample output

    PUBLIC (ships)                                   | REDACTED (stays local)
    -------------------------------------------------+-----------------------
    cosmon_version: 0.1.0                            | molecule_id: <REDACTED:id>
    formula_id: deep-think                           | prompt_content: <REDACTED:prompt>
    step_durations_ms: [174000,168000,498000]        | git_sha: <REDACTED:sha>
    exit_status: completed                           | topic: <REDACTED:topic>
    error_class: none                                | file_paths: <REDACTED:paths>
    energy_bucket: medium                            | timestamps_absolute: <REDACTED:ts>
    worker_model_family: unknown                     | variables: <REDACTED:vars>

## Why bash, not Rust

Torvalds + Jobs (fe35 §T3): 30 lines of `jq` beat a new Rust verb Tuesday.
The Rust `confidential: Option<bool>` on `MoleculeData` lands *after* tenant_auditor
reads this — non-breaking additive change (tolnay accessor pattern).
