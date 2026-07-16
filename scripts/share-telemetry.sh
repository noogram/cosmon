#!/usr/bin/env bash
# scripts/share-telemetry.sh — project a cosmon molecule onto the
# 'share-telemetry' surface: scan-then-emit, atomic, or refuse.
#
# Usage:
#   scripts/share-telemetry.sh <molecule_id> --dry-run [--out <path>]
#   scripts/share-telemetry.sh <molecule_id> --dry-run --out age:[<PUBKEY>]
#
# References:
#   delib-20260419-fe35 synthesis §(c) milestone 2, §Convergence #3 (atomicity),
#   §Convergence #6 (7 Pareto-optimal fields, shannon list from delib-6bb0).
#   task-20260419-9332 (milestone 1): `cs doctor leaks --corpus` landed on main.
#   task-20260419-5f67 (milestone 2 base): --dry-run + --out <path>.

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/share-telemetry.sh <molecule_id> --dry-run [--out <path|age:[PUBKEY]>]
       scripts/share-telemetry.sh --help

  <molecule_id>     Cosmon molecule id (e.g. delib-20260419-fe35).
  --dry-run         REQUIRED. Print the PUBLIC/REDACTED two-column diff on
                    STDOUT; emit nothing to disk unless --out is also given.
  --out <path>      Write the CLEAR bundle JSON to <path>.
  --out age:        Encrypt bundle with the default recipient from
                    ~/.config/cosmon/default-recipient.age and drop to
                    ~/cosmon-telemetry/outgoing/<mol_id>-<ts>.bundle.age.
  --out age:<PUB>   Encrypt bundle with the given age recipient key. Drop
                    to the default outgoing location.

Gate: invokes `cs doctor leaks --corpus ${COSMON_LEAK_CORPUS:-~/.config/cosmon/leak-corpus.toml}`
      on the PUBLIC bundle BEFORE printing OR encrypting. Exits non-zero if
      the scan flags a leak. Share = scan-then-encrypt-then-emit, atomic,
      or refuse.

Exit codes:
  0  clean — output printed on STDOUT
  2  usage error (bad args)
  3  missing dependency (jq, awk, cs, age)
  4  molecule not found / state.json missing
  5  not in a git repo (leak scan needs one)
  6  leak corpus not found
  7  REFUSED — leak scan flagged the bundle (atomicity gate)
  8  age encryption failed (missing recipient / age error)
EOF
}

if [[ $# -lt 1 ]]; then usage >&2; exit 2; fi
if [[ $1 == --help || $1 == -h ]]; then usage; exit 0; fi

mol_id=""
dry_run=0
out_path=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --dry-run) dry_run=1; shift ;;
    --out)     out_path="${2:-}"; [[ -z $out_path ]] && { echo "error: --out needs a path" >&2; exit 2; }; shift 2 ;;
    --help|-h) usage; exit 0 ;;
    --)        shift; break ;;
    -*)        echo "error: unknown flag: $1" >&2; usage >&2; exit 2 ;;
    *)         [[ -n $mol_id ]] && { echo "error: multiple molecule ids given" >&2; exit 2; }
               mol_id="$1"; shift ;;
  esac
done

[[ -z $mol_id ]]     && { echo "error: <molecule_id> required" >&2; exit 2; }
[[ $dry_run -eq 0 ]] && { echo "error: --dry-run is required (non-dry-run paths ship LATER)" >&2; exit 2; }

command -v jq  >/dev/null 2>&1 || { echo "error: jq required"  >&2; exit 3; }
command -v awk >/dev/null 2>&1 || { echo "error: awk required" >&2; exit 3; }

# CS binary — operator-overridable. Milestone 1's --corpus flag must be present.
CS="${CS:-}"
if [[ -z $CS ]]; then
  if   [[ -x "$HOME/.cargo/bin/cs" ]]; then CS="$HOME/.cargo/bin/cs"
  elif command -v cs >/dev/null 2>&1;  then CS="$(command -v cs)"
  else echo "error: cs CLI not found (set CS=/path/to/cs)" >&2; exit 3
  fi
fi

# Observe molecule. cs walks up from cwd to find .cosmon.
observe_json="$("$CS" observe "$mol_id" --json 2>/dev/null)" \
  || { echo "error: cs observe $mol_id failed (not found?)" >&2; exit 4; }

mol_dir="$(jq -r '.molecule_dir // empty' <<<"$observe_json")"
[[ -n $mol_dir && -d $mol_dir ]] \
  || { echo "error: molecule_dir not found for $mol_id" >&2; exit 4; }

state_json="$mol_dir/state.json"
[[ -f $state_json ]] \
  || { echo "error: state.json missing at $state_json" >&2; exit 4; }

# Cosmon version — best-effort.
cosmon_version="$("$CS" --version 2>/dev/null | awk '{print $NF}')"
[[ -z $cosmon_version ]] && cosmon_version="unknown"

repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[[ -z $repo_root ]] && { echo "error: not in a git repo (doctor leaks needs one)" >&2; exit 5; }

# ── 7 Pareto-optimal PUBLIC fields ──────────────────────────────────────────
# Verbatim from delib-20260419-fe35 synthesis §Convergence #6 (godel I17,
# inherited from delib-6bb0 shannon list):
#   cosmon_version · formula_id · step_durations_ms · exit_status ·
#   error_class · energy_bucket · worker_model_family
public_json="$(jq --arg cv "$cosmon_version" '
  # Parse RFC-3339 timestamps to seconds since epoch. jq 1.6 fromdateiso8601
  # only accepts second precision with a trailing "Z", so we strip fractional
  # seconds and normalize "+00:00" → "Z". We lose sub-second resolution (fine
  # here — deltas are minutes/hours), and multiply by 1000 to report ms.
  def to_ms:
    sub("\\.[0-9]+"; "")
    | sub("\\+00:00$"; "Z")
    | fromdateiso8601 * 1000;

  def durations:
    (.briefing_seals // [])
    | map(.sealed_at | to_ms)
    | . as $t
    | if (length) <= 1 then []
      else [ range(1; length) | ($t[.] - $t[.-1]) ] end;

  def error_class:
    if   .status == "completed" then "none"
    elif .status == "collapsed" then
         (.collapse_reason // "" | ascii_downcase
          | if   test("leak")    then "leak"
            elif test("timeout") then "timeout"
            elif test("stuck")   then "stuck"
            else                     "collapsed" end)
    elif .status == "failed"    then "worker_error"
    else                              "unknown" end;

  def energy_bucket:
    .energy_bucket // (
      ((.briefing_seals // []) | length) as $n
      | if   $n <= 2 then "low"
        elif $n <= 5 then "medium"
        else               "high" end);

  def model_family:
    .worker_model_family // ($ENV.COSMON_MODEL_FAMILY // "unknown");

  {
    cosmon_version:      $cv,
    formula_id:          (.formula_id // "unknown"),
    step_durations_ms:   durations,
    exit_status:         (.status // "unknown"),
    error_class:         error_class,
    energy_bucket:       energy_bucket,
    worker_model_family: model_family
  }
' "$state_json")"

# 7 REDACTED fields — name present, content replaced by <REDACTED:<type>>.
# Per fe35 §(c): prompt_content, molecule_id, git_sha, topic, file_paths,
# timestamps_absolute, variables.
redacted_json='{
  "molecule_id":        "<REDACTED:id>",
  "prompt_content":     "<REDACTED:prompt>",
  "git_sha":            "<REDACTED:sha>",
  "topic":              "<REDACTED:topic>",
  "file_paths":         "<REDACTED:paths>",
  "timestamps_absolute":"<REDACTED:ts>",
  "variables":          "<REDACTED:vars>"
}'

bundle_json="$(jq -n --argjson p "$public_json" --argjson r "$redacted_json" \
  '{public: $p, redacted: $r}')"

# ── Gate: scan-then-emit, atomic, or refuse ─────────────────────────────────
# Write the candidate bundle into the repo so `cs doctor leaks --path` can
# address it; `--include-untracked` picks up the scratch file without tracking.
scratch_dir="$repo_root/.cosmon/scratch"
mkdir -p "$scratch_dir"
scratch_file="$scratch_dir/share-telemetry-bundle-$$.json"
cleanup() { rm -f "$scratch_file"; }
trap cleanup EXIT

printf '%s\n' "$bundle_json" > "$scratch_file"
rel_scratch="${scratch_file#"$repo_root"/}"

corpus="${COSMON_LEAK_CORPUS:-$HOME/.config/cosmon/leak-corpus.toml}"
if [[ ! -f $corpus ]]; then
  echo "error: leak corpus not found at $corpus"       >&2
  echo "       set COSMON_LEAK_CORPUS or create one"   >&2
  exit 6
fi

if scan_out="$("$CS" doctor leaks \
                 --corpus "$corpus" \
                 --path   "$rel_scratch" \
                 --include-untracked 2>&1)"; then
  scan_rc=0
else
  scan_rc=$?
fi

if [[ $scan_rc -ne 0 ]]; then
  {
    echo "REFUSED: cs doctor leaks flagged the share-telemetry bundle (exit $scan_rc)."
    echo "         Share = scan-then-emit, atomic. Nothing leaves this machine."
    echo "         --- scanner output ---"
    printf '%s\n' "$scan_out"
  } >&2
  exit 7
fi

# ── Clean — emit the two-column diff to STDOUT ──────────────────────────────
pub_lines="$(jq -r 'to_entries[] | "\(.key): \(.value | tostring)"' <<<"$public_json")"
red_lines="$(jq -r 'to_entries[] | "\(.key): \(.value)"'            <<<"$redacted_json")"

W=48
printf '%-*s | %s\n' "$W" "PUBLIC (ships)" "REDACTED (stays local)"
bar="$(printf '%*s' "$W" '' | tr ' ' '-')"
printf '%s-+-%s\n' "$bar" "$bar"
paste <(printf '%s\n' "$pub_lines") <(printf '%s\n' "$red_lines") \
  | awk -F'\t' -v W="$W" '{
      l = $1; r = ($2 == "" ? "" : $2)
      if (length(l) > W) l = substr(l, 1, W)
      printf "%-*s | %s\n", W, l, r
    }'

if [[ -n $out_path ]]; then
  if [[ $out_path == age:* ]]; then
    # Encrypted drop. scan-then-encrypt-then-emit: the scan above already
    # passed before we reach this branch, so any age failure here leaves
    # nothing behind on disk (age --output writes atomically via temp+rename).
    command -v age >/dev/null 2>&1 \
      || { echo "error: age binary required for --out age: (brew install age)" >&2; exit 3; }

    pubkey="${out_path#age:}"
    if [[ -z $pubkey ]]; then
      default_recipient="${COSMON_DEFAULT_RECIPIENT:-$HOME/.config/cosmon/default-recipient.age}"
      [[ -f $default_recipient ]] \
        || { echo "error: default recipient not found at $default_recipient" >&2; exit 8; }
      # File may contain a single pubkey line or be comment-decorated — pick
      # the first non-empty, non-# line.
      pubkey="$(awk 'NF && $1 !~ /^#/ { print; exit }' "$default_recipient")"
      [[ -n $pubkey ]] \
        || { echo "error: no recipient key in $default_recipient" >&2; exit 8; }
    fi
    [[ $pubkey == age1* ]] \
      || { echo "error: invalid age recipient '$pubkey' (must start with age1)" >&2; exit 8; }

    ts="$(date -u +%Y%m%dT%H%M%SZ)"
    drop_dir="${COSMON_TELEMETRY_OUTGOING:-$HOME/cosmon-telemetry/outgoing}"
    mkdir -p "$drop_dir"
    drop_path="$drop_dir/${mol_id}-${ts}.bundle.age"

    if ! printf '%s\n' "$bundle_json" \
         | age --encrypt --recipient "$pubkey" --output "$drop_path" 2>/tmp/share-telemetry-age.err
    then
      {
        echo "error: age encryption failed"
        sed 's/^/       /' /tmp/share-telemetry-age.err
      } >&2
      rm -f "$drop_path" /tmp/share-telemetry-age.err
      exit 8
    fi
    rm -f /tmp/share-telemetry-age.err
    echo "bundle sealed: $drop_path" >&2
    echo "recipient:     $pubkey"    >&2
  else
    printf '%s\n' "$bundle_json" > "$out_path"
    echo "bundle written: $out_path" >&2
  fi
fi
