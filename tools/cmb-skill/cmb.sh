#!/usr/bin/env bash
# /cmb — Cosmic Microwave Background session-handoff snapshot.
#
# Emits a copy-pasteable Markdown prompt capturing the residual signal of the
# current session — the state-not-on-disk that a fresh successor session needs
# to pick up the work. See ~/.claude/skills/cmb/SKILL.md for the metaphor and
# the distinction from the harness /handoff gesture.
#
# Pure shell. Reads disk + JSONL transcript. Writes nothing.

set -euo pipefail

QUESTION_CAP=5            # max OPEN-question lines emitted
DECISION_CAP=10           # max decision-candidate lines emitted (V0.5)
TAIL_MESSAGES=200         # only scan the last N JSONL records for questions
                          # (keeps long sessions snappy; the tail is where the
                          # unresolved cadence lives)

# V1' (--target / --intent) — closed verb set for cross-galaxy routing.
# `merge` is deliberately absent: merge-before-dispatch (CLAUDE.md ;
# ADR-016) makes `cs done` the only path to fusion, and a cross-galaxy
# `merge` verb would create a bypass.
VALID_INTENTS=(pick-up review extend respond inform none)

TARGET=""                 # --target=<galaxy>  (or positional `to <galaxy>`)
INTENT=""                 # --intent=<verb>    (required when --target is set)

now_iso() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }

# ---------- CLI parsing ----------

usage() {
  cat <<'EOF'
Usage: cmb.sh [--target=<galaxy> --intent=<verb>] [to <galaxy>]

Emit a copy-pasteable Markdown handoff snapshot for the current session.

Optional cross-galaxy routing (V1'):
  --target=<galaxy>   gate source-galaxy noise sections (working tree,
                      live workers, tmux, galaxy-local memory) and emit
                      `to: <galaxy>` in the frontmatter. Validated against
                      /srv/cosmon/*/.cosmon/config.toml.
  --intent=<verb>     required when --target is set. Closed verb set:
                        pick-up | review | extend | respond | inform | none
                      `merge` is forbidden (see merge-before-dispatch).
  to <galaxy>         positional prose alias for --target.

Without flags, behaves identically to V0.5 (intra-galaxy snapshot).
EOF
}

parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --target=*)
        TARGET="${1#--target=}"
        shift
        ;;
      --target)
        shift
        if [ $# -eq 0 ]; then
          echo "error: --target requires a value." >&2
          exit 2
        fi
        TARGET="$1"
        shift
        ;;
      --intent=*)
        INTENT="${1#--intent=}"
        shift
        ;;
      --intent)
        shift
        if [ $# -eq 0 ]; then
          echo "error: --intent requires a value." >&2
          exit 2
        fi
        INTENT="$1"
        shift
        ;;
      to)
        shift
        if [ $# -eq 0 ]; then
          echo "error: positional 'to' requires a galaxy name." >&2
          exit 2
        fi
        TARGET="$1"
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        echo "error: unknown argument: $1" >&2
        usage >&2
        exit 2
        ;;
    esac
  done
}

# ---------- known-galaxy discovery (dynamic; glob fallback) ----------

# List cosmon-managed galaxies under /srv/cosmon/. One galaxy per line.
# This is the fallback when no neurion query is wired in; the glob is the
# same source of truth the operator manipulates by `cs init`-ing a new dir.
discover_galaxies() {
  shopt -s nullglob 2>/dev/null || true
  local d
  for d in "$HOME"/galaxies/*/; do
    if [ -f "$d/.cosmon/config.toml" ]; then
      basename "$d"
    fi
  done | sort -u
}

# Naive close-match suggester: pick the known galaxy with the longest shared
# prefix or substring containment with $1. Returns empty if no candidate.
suggest_galaxy() {
  local target="$1"
  local best=""
  local best_score=0
  local g score
  while IFS= read -r g; do
    [ -z "$g" ] && continue
    score=0
    case "$g" in
      "$target"*|*"$target"*) score=$(( ${#target} * 2 )) ;;
    esac
    case "$target" in
      "$g"*|*"$g"*) score=$(( score + ${#g} )) ;;
    esac
    # also tally shared prefix length
    local i=0
    while [ "$i" -lt "${#target}" ] && [ "$i" -lt "${#g}" ] \
          && [ "${target:$i:1}" = "${g:$i:1}" ]; do
      i=$((i + 1))
    done
    score=$(( score + i ))
    if [ "$score" -gt "$best_score" ]; then
      best="$g"
      best_score="$score"
    fi
  done < <(discover_galaxies)
  echo "$best"
}

validate_target() {
  local target="$1"
  local known
  known="$(discover_galaxies)"
  if echo "$known" | /usr/bin/grep -qFx "$target"; then
    return 0
  fi
  echo "error: unknown target galaxy '$target'." >&2
  local suggestion
  suggestion="$(suggest_galaxy "$target")"
  if [ -n "$suggestion" ]; then
    echo "Did you mean: --target=$suggestion ?" >&2
  fi
  echo "Known galaxies (from /srv/cosmon/*/.cosmon/config.toml):" >&2
  while IFS= read -r g; do
    [ -z "$g" ] && continue
    echo "  - $g" >&2
  done <<<"$known"
  exit 2
}

# Print the list of allowed intents to stderr (used by error paths).
print_valid_intents() {
  cat >&2 <<'EOF'
Valid intents:
  pick-up — start work on the snapshot's referenced artefacts
  review  — read and produce feedback
  extend  — add to an existing artefact (delib, ADR, doc)
  respond — reply to an open atomic question
  inform  — context-only, no action expected
  none    — explicit honest exit (no verb in mind, "reste honnête")
EOF
}

validate_intent() {
  local intent="$1"
  case "$intent" in
    pick-up|review|extend|respond|inform|none)
      return 0
      ;;
    merge)
      cat >&2 <<'EOF'
error: intent 'merge' is forbidden.
Reason: merge-before-dispatch (CLAUDE.md / ADR-016) makes `cs done` the
        only path to fusion. A cross-galaxy 'merge' intent would create
        a bypass — the receiver might fuse work that has not been
        validated by the source galaxy's `cs done` gate.
EOF
      print_valid_intents
      exit 2
      ;;
    *)
      echo "error: unknown intent '$intent'." >&2
      print_valid_intents
      exit 2
      ;;
  esac
}

# ---------- galaxy detection (walk-up for .cosmon/config.toml) ----------

find_cosmon_root() {
  local d="$PWD"
  while [ "$d" != "/" ] && [ -n "$d" ]; do
    if [ -f "$d/.cosmon/config.toml" ]; then
      echo "$d"
      return 0
    fi
    d="$(dirname "$d")"
  done
  return 1
}

galaxy_name_from_root() {
  local root="$1"
  # Prefer last path segment, but strip a trailing `.worktrees/<x>` if present.
  local p="$root"
  case "$p" in
    */.worktrees/*) p="$(dirname "$(dirname "$p")")" ;;
  esac
  basename "$p"
}

# ---------- session JSONL discovery (longest-prefix on cwd) ----------

# Returns 0 + path on stdout, or 1 if no candidate found.
find_current_session_jsonl() {
  local pwd_abs="$PWD"
  local best=""
  local best_mtime=0
  local best_cwd_len=0

  shopt -s nullglob 2>/dev/null || true

  # Probe Claude Code projects/ layout (primary).
  local roots=(
    "$HOME/.claude/projects"
    "$HOME/.openclaw/agents"
  )

  for root in "${roots[@]}"; do
    [ -d "$root" ] || continue

    local files=()
    if [ "$root" = "$HOME/.openclaw/agents" ]; then
      # openclaw: ~/.openclaw/agents/<agentId>/sessions/*.jsonl
      for f in "$root"/*/sessions/*.jsonl; do
        [ -f "$f" ] && files+=("$f")
      done
    else
      # claude code: ~/.claude/projects/<encoded-cwd>/*.jsonl
      for f in "$root"/*/*.jsonl; do
        [ -f "$f" ] && files+=("$f")
      done
    fi

    for f in "${files[@]}"; do
      # Extract first cwd from the JSONL (skip lines without cwd).
      local cwd
      cwd="$(jq -rs 'first(.[] | select(.cwd? != null and .cwd != "") | .cwd) // empty' "$f" 2>/dev/null || true)"
      [ -z "$cwd" ] && continue

      # Windows path normalization (Git Bash / MSYS): Claude Code Windows
      # stores $cwd as native `C:\…\dir` in the JSONL, but $PWD here is
      # `/c/…/dir`. Without this, the ancestor test below never matches and
      # session discovery returns empty. cygpath is the MSYS-provided
      # converter; this branch is a no-op on macOS/Linux where neither the
      # pattern nor cygpath are present. (Reported by Bob 2026-05-17.)
      case "$cwd" in
        *\\*|[A-Za-z]:[\\/]*)
          if command -v cygpath >/dev/null 2>&1; then
            cwd="$(cygpath -u "$cwd")"
          fi
          ;;
      esac

      # Ancestor test: $cwd must be == $pwd_abs or a prefix segment of it.
      case "$pwd_abs" in
        "$cwd"|"$cwd"/*) : ;;  # match
        *) continue ;;
      esac

      local mtime
      # GNU stat first (Linux + Git Bash on Windows where BSD `-f` is
      # interpreted as `--file-system` and would silently succeed with
      # garbage, breaking the integer compare below), BSD `-f` fallback for
      # macOS where `-c` errors out cleanly. (Reported by Bob 2026-05-17.)
      mtime="$(stat -c %Y "$f" 2>/dev/null || stat -f %m "$f" 2>/dev/null || echo 0)"
      local cwd_len=${#cwd}

      # Pick the longest cwd-prefix; tiebreak by mtime (most recent wins).
      if [ "$cwd_len" -gt "$best_cwd_len" ] \
         || { [ "$cwd_len" -eq "$best_cwd_len" ] && [ "$mtime" -gt "$best_mtime" ]; }; then
        best="$f"
        best_mtime="$mtime"
        best_cwd_len="$cwd_len"
      fi
    done
  done

  if [ -n "$best" ]; then
    echo "$best"
    return 0
  fi
  return 1
}

# ---------- atomic-question heuristic ----------

# Heuristic: an assistant-text message whose last sentence ends with `?` is
# OPEN unless the next user message is a short affirmative reply
# (oui|non|ok|...). If no user reply at all, mark "no_reply" — the agent likely
# moved on with tool calls. Operator prunes false positives on paste.
extract_open_questions_v2() {
  local file="$1"
  jq -rs --argjson cap "$QUESTION_CAP" '
    def is_short_affirm:
      (gsub("\\s+"; " ") | gsub("^\\s+|\\s+$"; "")) as $s
      | ($s | length) <= 60
        and ($s | test("^(oui|non|ok|okay|👍|👌|d.?accord|yes|no|y|n|sure|nope|c.?est bon|exact|correct|good|fine|go|done|merci|thanks|thx|si|yep|yup|let.?s go|yes please|no thanks|never mind|skip|next|continue|proceed|stop)([.!]|$|\\s)"; "i"));

    # Schema normalizer: .message.content may be a STRING (plain user text)
    # or an ARRAY of {type, text, tool_use_id, …} blocks. Return only the
    # natural-language text, joined.
    def texts_of:
      (.message.content // null) as $c
      | if   $c == null then []
        elif ($c | type) == "string" then [$c]
        elif ($c | type) == "array"  then
          [ $c[] | select(.type? == "text") | .text ]
        else [] end;

    [ .[]
      | select(.type=="user" or .type=="assistant")
      | . as $msg
      | { role: ($msg.message.role // $msg.type),
          ts:   ($msg.timestamp // ""),
          texts: ($msg | texts_of)
        }
    ] as $m
    | [ range(0; ($m | length)) as $i
        | $m[$i] as $cur
        | select($cur.role=="assistant" and ($cur.texts | length > 0))
        | ($cur.texts | join("\n")) as $t
        | select($t | test("\\?\\s*$"))
        # look ahead: scan for next user message with text
        | (reduce range($i+1; ($m | length)) as $j (null;
            if . != null then .
            elif $m[$j].role == "user" and ($m[$j].texts | length > 0) then $m[$j]
            else null
            end)) as $next_user
        # also note: was the very next non-tool message a tool_use from agent?
        # if next_user is null AND we kept seeing only tool calls -> assumed-resolved
        | (if $next_user == null then "no_reply"
           elif (($next_user.texts | join("\n")) | is_short_affirm) then "resolved"
           else "open" end) as $verdict
        | select($verdict == "open" or $verdict == "no_reply")
        # extract trailing question line
        | ($t
            | split("\n")
            | reverse
            | map(select(length > 0))
            | first
            | tostring) as $q
        | { i: $i, ts: $cur.ts, q: $q, verdict: $verdict }
      ]
    | sort_by(-.i)
    | .[0:$cap]
    | reverse
    | .[]
    | "- (\(.verdict)) \(.q)"
  ' "$file" 2>/dev/null || true
}

# ---------- decisions tranchées heuristic (V0.5) ----------

# Heuristic: detect operator one-line verdicts in the conversation transcript.
# Three signal patterns, combined (first match wins to label the candidate):
#   (verb)          user message containing a decision verb in FR/EN
#                   (gardons / on garde / acté / on tranche / we keep /
#                   shipped / let's ship / on retient / ship it / …)
#   (mode-feedback) operator-mode-feedback patterns (don't / never / always /
#                   from now on / désormais / jamais / toujours / à partir de
#                   maintenant / ne fais pas / par défaut / default to)
#   (short-reply)   short user message (<20 words) immediately after an
#                   assistant question, AND not a pure verdict (oui/non/ok)
#
# Output: ranked by recency, capped at DECISION_CAP, one line per candidate
# with timestamp, pattern label, and ≤120-char excerpt. The operator's eye
# prunes false positives at paste time (propose-not-impose).
extract_decision_candidates() {
  local file="$1"
  jq -rs --argjson cap "$DECISION_CAP" '
    def is_short_affirm:
      (gsub("\\s+"; " ") | gsub("^\\s+|\\s+$"; "")) as $s
      | ($s | length) <= 60
        and ($s | test("^(oui|non|ok|okay|👍|👌|d.?accord|yes|no|y|n|sure|nope|c.?est bon|exact|correct|good|fine|go|done|merci|thanks|thx|si|yep|yup|let.?s go|yes please|no thanks|never mind|skip|next|continue|proceed|stop)([.!]|$|\\s)"; "i"));

    # Strong, low-ambiguity decision phrases. Excludes verbs that appear
    # in questions as often as in declaratives ("on fait ça?", "va pour",
    # "we don'\''t"). Operator can paste those manually if needed; V0.5
    # leans toward precision over recall.
    def has_decision_verb:
      test("(?:gardons\\b|on (?:la |le )?garde\\b|on (?:ne )?garde pas|act[eé]\\b|on tranche\\b|c.est tranch[eé]|on retient\\b|we keep\\b|we ship(?:ped)?\\b|let.?s ship|let.?s keep|shipped\\b|ship it\\b|on (?:le )?merge\\b|merge it\\b)"; "i");

    # Mode-feedback patterns. French "toujours"/"jamais" are dropped as
    # bare-form because they double as "still"/"ever" (status reports —
    # "toujours vide", "toujours rien"). Only verb-led forms ("fais
    # toujours / jamais") retain that signal.
    def has_mode_feedback:
      test("(?:don.?t |never |always |from now on\\b|d[eé]sormais\\b|[àa] partir de maintenant\\b|ne fais pas |ne le fais pas|par d[eé]faut\\b|default to |fais (?:toujours|jamais))"; "i");

    def word_count:
      gsub("\\s+"; " ") | gsub("^\\s+|\\s+$"; "") | split(" ") | length;

    def trim120:
      gsub("\\s+"; " ") | gsub("^\\s+|\\s+$"; "")
      | if length > 120 then .[0:117] + "…" else . end;

    # Schema normalizer: .message.content is STRING for plain user text and
    # ARRAY for tool_result / assistant blocks. Surface only natural text.
    def texts_of:
      (.message.content // null) as $c
      | if   $c == null then []
        elif ($c | type) == "string" then [$c]
        elif ($c | type) == "array"  then
          [ $c[] | select(.type? == "text") | .text ]
        else [] end;

    [ .[]
      | select(.type=="user" or .type=="assistant")
      | . as $msg
      | { role: ($msg.message.role // $msg.type),
          ts:   ($msg.timestamp // ""),
          texts: ($msg | texts_of)
        }
    ] as $m
    | [ range(0; ($m | length)) as $i
        | $m[$i] as $cur
        | select($cur.role=="user" and ($cur.texts | length > 0))
        | ($cur.texts | join("\n")) as $t
        # noise filters: slash-commands, system-wrapped tags, image-only,
        # interrupted-tool messages, and the synthetic Caveat banner.
        | select(($t | test("^/[a-z]"; "i")) | not)
        | select(($t | test("^<(?:command-name|local-command|bash-|task-notification)")) | not)
        | select(($t | test("^Caveat: The messages below were generated")) | not)
        | select(($t | test("^\\[Request interrupted")) | not)
        | select(($t | test("^\\[Image #[0-9]+\\]\\s*$")) | not)
        # counter-questions (user message ending in ?) are not decisions
        | select(($t | test("\\?\\s*$")) | not)
        # long pastes (>800 chars) often embed trigger words inside third-
        # party content, not as operator decisions; skip them.
        | select(($t | length) <= 800)
        # detect signal patterns; verb wins over mode-feedback wins over short-reply
        | (if ($t | has_decision_verb) then "verb"
           elif ($t | has_mode_feedback) then "mode-feedback"
           else
             # find previous assistant message
             (reduce range($i-1; -1; -1) as $j (null;
                if . != null then .
                elif $m[$j].role == "assistant" and ($m[$j].texts | length > 0) then $m[$j]
                else null
                end)) as $prev_a
             | (if $prev_a == null then null
                elif (($prev_a.texts | join("\n")) | test("\\?\\s*$") | not) then null
                elif ($t | is_short_affirm) then null
                elif ($t | word_count) >= 20 then null
                elif ($t | word_count) < 3 then null
                else "short-reply" end)
           end) as $pattern
        | select($pattern != null)
        | { i: $i, ts: $cur.ts, q: ($t | trim120), pattern: $pattern }
      ]
    | sort_by(-.i)
    | .[0:$cap]
    | reverse
    | .[]
    | "- (\(.pattern)) [\(.ts)] \(.q)"
  ' "$file" 2>/dev/null || true
}

# ---------- MEMORY.md preferences ----------

emit_memory_index() {
  # When $1 is non-empty, we are in cross-galaxy mode and must drop
  # galaxy-local entries — those whose file name starts with `project_`
  # (the cosmon convention: project_* = project-specific; user_* /
  # feedback_* / reference_* = cross-cutting). The filter is a simple
  # prefix rule, not a structured marker; future operator-prefix index
  # markers can refine this when the convention lands.
  local cross_galaxy="${1:-}"

  # The memory dir is per-project under ~/.claude/projects/<encoded>/memory/MEMORY.md
  # We point to the canonical user-global one if found, else nearest project.
  local mem_path
  for cand in \
      "$HOME/.claude/projects/-Users-you-galaxies-cosmon/memory/MEMORY.md" \
      "$HOME/.claude/CLAUDE.md"; do
    if [ -f "$cand" ]; then
      mem_path="$cand"
      break
    fi
  done
  if [ -n "${mem_path:-}" ]; then
    if [ -n "$cross_galaxy" ]; then
      echo "_Index from \`${mem_path/$HOME/~}\` (cross-galaxy — galaxy-local entries filtered):_"
    else
      echo "_Index from \`${mem_path/$HOME/~}\`:_"
    fi
    echo
    local lines
    # Take only `- [Title](file) — hook` lines (the index format)
    lines="$(/usr/bin/grep -E '^- \[' "$mem_path" 2>/dev/null || true)"
    if [ -n "$cross_galaxy" ] && [ -n "$lines" ]; then
      # Drop entries whose file slug starts with `project_` — those are
      # galaxy-local context the receiver does not need.
      lines="$(echo "$lines" | /usr/bin/grep -v -E '\]\(project_' || true)"
    fi
    if [ -n "$lines" ]; then
      echo "$lines" | head -20
    else
      echo "_(none)_"
    fi
  else
    echo "_(no MEMORY.md found)_"
  fi
}

# ---------- assemble report ----------

main() {
  parse_args "$@"

  # Cross-galaxy validation: --target requires --intent ; both must be valid.
  if [ -n "$TARGET" ]; then
    validate_target "$TARGET"
    if [ -z "$INTENT" ]; then
      echo "error: --intent is required when --target is set." >&2
      echo "       Use --intent=none for an explicit honest exit." >&2
      print_valid_intents
      exit 2
    fi
    validate_intent "$INTENT"
  elif [ -n "$INTENT" ]; then
    # --intent without --target is still validated (cheap defensive check)
    # so an operator that types only --intent gets a clean error rather
    # than a silently-ignored flag.
    validate_intent "$INTENT"
  fi

  local cosmon_root galaxy branch session_jsonl autopilot_off cross_galaxy
  cosmon_root="$(find_cosmon_root || true)"
  galaxy="$( [ -n "$cosmon_root" ] && galaxy_name_from_root "$cosmon_root" || echo "(none)" )"
  branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "(not a git repo)")"
  session_jsonl="$(find_current_session_jsonl || true)"
  autopilot_off="false"
  [ -f "$HOME/.cosmon/autopilot.off" ] && autopilot_off="true"

  # Cross-galaxy mode is opt-in: any --target setting activates section
  # gating, even when target == source (the operator's deliberate gesture
  # of typing --target is the signal, not the comparison).
  cross_galaxy=""
  [ -n "$TARGET" ] && cross_galaxy="1"

  # ---- lede + frontmatter (universal) ----
  cat <<EOF
> ⚠️  **REVIEW BEFORE PASTE** — auto-extracted, may include leaked reasoning.
> Trim every section the successor doesn't need. /cmb is a draft, not a chronicle.

---
generated_at: $(now_iso)
source_session: ${session_jsonl:-(unknown)}
from: ${galaxy}
EOF
  if [ -n "$cross_galaxy" ]; then
    echo "to: ${TARGET}"
    echo "intent: ${INTENT}"
  fi
  cat <<EOF
galaxy: ${galaxy}
working_dir: ${PWD}
branch: ${branch}
autopilot_off: ${autopilot_off}
---

# Session handoff — CMB snapshot

## Where we are

- Working directory: \`${PWD}\`
- Galaxy: **${galaxy}**
EOF
  if [ -n "$cross_galaxy" ]; then
    echo "- Routed to: **${TARGET}** (intent: \`${INTENT}\`)"
  fi
  cat <<EOF
- Branch: \`${branch}\`
- Autopilot kill-switch: \`~/.cosmon/autopilot.off\` ${autopilot_off}

EOF

  # ---- atomic questions ----
  echo "## Atomic questions OPEN (heuristic — review!)"
  echo
  if [ -n "$session_jsonl" ] && [ -f "$session_jsonl" ]; then
    local qs
    qs="$(tail -n "$TAIL_MESSAGES" "$session_jsonl" | extract_open_questions_v2 /dev/stdin || true)"
    if [ -n "$qs" ]; then
      echo "$qs"
    else
      echo "_(none detected — the trail ended without an unresolved \`?\` from the agent)_"
    fi
  else
    echo "_(no session JSONL found — discovery probes \`~/.claude/projects/\` and \`~/.openclaw/agents/\`)_"
  fi
  echo

  # ---- decisions tranchées (V0.5: heuristic candidates) ----
  echo "## Decisions tranchées (candidates — heuristic, review!)"
  echo
  if [ -n "$session_jsonl" ] && [ -f "$session_jsonl" ]; then
    local ds
    ds="$(tail -n "$TAIL_MESSAGES" "$session_jsonl" | extract_decision_candidates /dev/stdin || true)"
    if [ -n "$ds" ]; then
      echo "$ds"
      echo
      echo "_(patterns: \`verb\`=decision verb, \`mode-feedback\`=enduring rule, \`short-reply\`=qualified answer to a question. Prune false positives at paste.)_"
    else
      echo "_(no candidate decisions detected in last ${TAIL_MESSAGES} messages — fill manually if needed)_"
    fi
  else
    echo "_(no session JSONL found — manual fill expected)_"
  fi
  echo

  # ---- runners in flight (gated under cross-galaxy: source-fleet noise) ----
  if [ -n "$cross_galaxy" ]; then
    echo "_(Runners in flight + active tmux omitted — cross-galaxy mode, source-fleet noise.)_"
    echo
  else
  echo "## Runners in flight"
  echo
  if [ -n "$cosmon_root" ] && command -v cs >/dev/null 2>&1; then
    if cs ensemble --json >/tmp/cmb-ensemble.$$.json 2>/dev/null; then
      jq -r '
        . as $root
        | ($root.workers // [])
        | map(select(.molecule_health != "completed" and .molecule_health != "collapsed"))
        as $live
        | if ($live | length) == 0 then
            "_(no live workers in fleet)_"
          else
            (
              "**Tally:** \(($root.molecules.pending // 0)) pending · " +
              "\(($root.molecules.running // 0)) running · " +
              "\(($root.molecules.frozen // 0)) frozen · " +
              "\(($root.molecules.completed // 0)) completed (last cycle).\n\n" +
              "**Live workers (top 10 by recency):**\n" +
              ($live[0:10]
                | map("- `\(.molecule // "(no-molecule)")` — worker `\(.name)` — \(.effective)/\(.live) — $\(.cost | (. * 100 | floor) / 100)")
                | join("\n"))
            )
          end
      ' /tmp/cmb-ensemble.$$.json 2>/dev/null \
        || echo "_(cs ensemble parse failed — JSON shape may have changed)_"
      rm -f /tmp/cmb-ensemble.$$.json
    else
      echo "_(cs ensemble not available)_"
    fi
  else
    echo "_(not in a cosmon-managed worktree — no \`cs\` data)_"
  fi
  echo

  # ---- active tmux ----
  echo "### Active tmux sessions"
  echo
  if command -v tmux >/dev/null 2>&1; then
    local tmux_out
    tmux_out="$(tmux list-sessions -F '- `#{session_name}` — #{?session_attached,attached,detached} — #{session_windows} windows — last activity #{t/p:session_activity}' 2>/dev/null || true)"
    if [ -n "$tmux_out" ]; then
      echo "$tmux_out"
    else
      echo "_(no tmux sessions)_"
    fi
  else
    echo "_(tmux not installed)_"
  fi
  echo
  fi  # end gate: runners + tmux

  # ---- operator preferences ----
  echo "## Operator preferences active in this session"
  echo
  emit_memory_index "$cross_galaxy"
  echo

  # ---- recent commits ----
  echo "## Recent commits (last 5, current branch)"
  echo
  if git rev-parse --git-dir >/dev/null 2>&1; then
    echo '```'
    git log --oneline --first-parent -5 2>/dev/null || echo "(no commits)"
    echo '```'
  else
    echo "_(not a git repo)_"
  fi
  echo

  # ---- git status (gated under cross-galaxy: source-tree noise) ----
  if [ -z "$cross_galaxy" ]; then
    echo "### Working tree"
    echo
    if git rev-parse --git-dir >/dev/null 2>&1; then
      local st
      st="$(git status -sb 2>/dev/null || true)"
      if [ -n "$st" ]; then
        echo '```'
        echo "$st"
        echo '```'
      else
        echo "_(clean)_"
      fi
    else
      echo "_(not a git repo)_"
    fi
    echo
  fi

  # ---- suggested first action ----
  cat <<'EOF'
## Suggested first action for successor

- Read the OPEN atomic questions above; resolve any that are stale orally.
- `cs peek` to see live fleet; the JSON listing above is a stale snapshot.
- If autopilot was on, decide whether the successor should re-engage it.
- Inspect the working tree before any new edit — uncommitted changes belong
  to the previous session's intent.

EOF
}

main "$@"
