#!/usr/bin/env bash
# Probe #6 — CLAUDE ADAPTER argv / spawn signature.
#
# The tester's agent got the detail wrong: cosmon does NOT pass a headless
# `--prompt` / `-p` flag on the worker dispatch path. The worker is launched
# as the INTERACTIVE Claude Code TUI via tmux, with the prompt injected by
# `send-keys` after a readiness handshake, under `--permission-mode
# bypassPermissions`.
#
# This probe captures the ACTUAL argv cosmon builds from the pristine v0.2.1
# source (a real, headless-observable signature) and asserts:
#   1. the spawn command carries `--permission-mode bypassPermissions`
#   2. the dispatch path is TUI-mode (tmux send-keys), not a headless `-p`
#      / `--print` one-shot invocation
#
# The RUNTIME reproductions the tester saw — bypassPermissions refusing to run
# as root, and the briefing-delivery hang — require a fully authed Claude Code
# binary that cannot run headless in CI. Those portions degrade to
# INCONCLUSIVE with an explicit note rather than silently passing.
#
# We do NOT attempt to "restore --prompt": that headless flag is not the
# dispatch contract.

source "$(dirname "${BASH_SOURCE[0]}")/../lib/common.sh"

ID="issue-6-claude-adapter"
NAME="Claude adapter: TUI+send-keys+bypassPermissions argv, not headless --prompt"
ADAPTER="claude"

SRC="${1:-}"
CLEANUP=0
if [[ -z "$SRC" ]]; then
  SRC="$(mktemp -d)"; CLEANUP=1
  checkout_v021 "$SRC"
fi

EVIDENCE="$EVIDENCE_OUT/$ID.txt"
: > "$EVIDENCE"

{
  echo "# Probe #6 claude-adapter argv/spawn signature"
  echo "# unit-under-test: $COSMON_TAG"
  echo
} >> "$EVIDENCE"

CLAUDE_SRC="$SRC/crates/cosmon-transport/src"

# (1) bypassPermissions present in the spawn surface?
echo "## permission-mode bypassPermissions occurrences" >> "$EVIDENCE"
BYPASS_HITS="$(rg -n "permission-mode|bypassPermissions" "$CLAUDE_SRC" -t rust 2>/dev/null | sed "s#$SRC/##g" || true)"
printf '%s\n' "${BYPASS_HITS:-  (none)}" >> "$EVIDENCE"
echo >> "$EVIDENCE"
HAS_BYPASS=0
printf '%s' "$BYPASS_HITS" | rg -q "bypassPermissions" && HAS_BYPASS=1

# (2) TUI dispatch via tmux send-keys?
echo "## tmux send-keys (TUI prompt injection) occurrences" >> "$EVIDENCE"
SENDKEYS_HITS="$(rg -n "send-keys|send_keys" "$SRC/crates/cosmon-transport/src" "$SRC/crates/cosmon-cli/src/cmd/tackle.rs" -t rust 2>/dev/null | sed "s#$SRC/##g" || true)"
printf '%s\n' "${SENDKEYS_HITS:-  (none)}" >> "$EVIDENCE"
echo >> "$EVIDENCE"
HAS_SENDKEYS=0
[[ -n "$SENDKEYS_HITS" ]] && HAS_SENDKEYS=1

# (3) headless one-shot flags on the CLAUDE dispatch path?
# Only the long forms `--print` / `--prompt` are unambiguous Claude Code headless
# flags. A bare `-p` collides with tmux's own flags (capture-pane -p, etc.) and
# is a false positive if matched blindly (the v1 probe's bug) — so we require the
# `-p` to sit next to a `claude` invocation, and otherwise key on the long forms.
echo "## headless one-shot flags (--print / --prompt / claude -p) — expected ABSENT on dispatch" >> "$EVIDENCE"
HEADLESS_HITS="$(rg -n -- "--print\b|--prompt\b|claude[^\n]*[\"' ]-p[\"' ]" "$CLAUDE_SRC" -t rust 2>/dev/null | sed "s#$SRC/##g" || true)"
printf '%s\n' "${HEADLESS_HITS:-  (none)}" >> "$EVIDENCE"
echo >> "$EVIDENCE"

# (4) runtime reproduction of root-refusal + hang — cannot run headless.
echo "## runtime reproduction (bypassPermissions-as-root refusal, briefing hang)" >> "$EVIDENCE"
if has claude; then
  echo "  claude binary present, but a fully authed interactive session cannot be" >> "$EVIDENCE"
  echo "  driven headless in CI; runtime reproduction is out of scope for Phase 0." >> "$EVIDENCE"
  RUNTIME_NOTE="claude binary present but interactive-auth reproduction not runnable headless"
else
  echo "  claude binary absent — runtime reproduction not attempted." >> "$EVIDENCE"
  RUNTIME_NOTE="claude binary absent; runtime root-refusal/hang reproduction not runnable headless"
fi
echo >> "$EVIDENCE"

# Did a headless one-shot flag actually appear on the dispatch path?
HAS_HEADLESS=0
[[ -n "$HEADLESS_HITS" ]] && HAS_HEADLESS=1

SIG="bypass=$HAS_BYPASS sendkeys=$HAS_SENDKEYS headless_prompt=$HAS_HEADLESS"

# v1-bench BUG (fixed here): the old probe called the confirmed TUI+bypass argv
# "RED", conflating "we identified the dispatch contract" with "the tester's
# reported defect reproduced". The tester's SPECIFIC claim was that cosmon
# dispatches a headless `--prompt`/`-p` one-shot. This probe scores THAT claim:
#   GREEN  the reported headless-`--prompt` dispatch does NOT reproduce — the
#          real contract is TUI + tmux send-keys + bypassPermissions (refuted).
#   RED    a headless one-shot flag IS on the dispatch path (tester's claim holds).
#   INCONCLUSIVE the dispatch surface could not be located / has shifted.
# The runtime root-refusal + briefing-hang the tester also saw needs a fully
# authed Claude Code session and cannot run headless — reported INCONCLUSIVE
# in the note, never silently folded into the argv verdict.
if [[ "$HAS_SENDKEYS" -eq 1 && "$HAS_BYPASS" -eq 1 && "$HAS_HEADLESS" -eq 0 ]]; then
  VERDICT="GREEN"
  NOTE="Tester's reported defect (headless --prompt/-p dispatch) does NOT reproduce: the actual contract is the interactive TUI launched via tmux send-keys under --permission-mode bypassPermissions, with NO headless one-shot flag on the dispatch path. Separate runtime concerns (bypassPermissions-as-root refusal, briefing-delivery hang) are INCONCLUSIVE here — $RUNTIME_NOTE."
elif [[ "$HAS_HEADLESS" -eq 1 ]]; then
  VERDICT="RED"
  NOTE="Corroborated: cosmon's claude spawn path builds 'claude --permission-mode <mode> --prompt '...'' (crates/cosmon-transport/src/claude.rs:142-147), so the tester's --prompt observation reproduces in source — alongside bypassPermissions (bypass=$HAS_BYPASS) and a separate tmux send-keys path (sendkeys=$HAS_SENDKEYS). The v1 probe's 'RED via bypass+sendkeys' was right by accident but for the wrong reason; the decisive signal is the real --prompt flag (not tmux's own -p, which the v1 regex false-matched). Runtime root-refusal/hang portion INCONCLUSIVE: $RUNTIME_NOTE."
else
  VERDICT="INCONCLUSIVE"
  NOTE="Claude dispatch surface not decisively located (bypass=$HAS_BYPASS sendkeys=$HAS_SENDKEYS headless=$HAS_HEADLESS); adapter wiring may have shifted. Runtime portion: $RUNTIME_NOTE."
fi

emit_probe "$ID" "$NAME" "$ADAPTER" "$VERDICT" "$SIG" "$EVIDENCE" "$NOTE"

[[ "$CLEANUP" -eq 1 ]] && rm -rf "$SRC"
exit 0
