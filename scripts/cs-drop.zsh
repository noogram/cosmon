#!/usr/bin/env zsh
# cs-drop.zsh — Ctrl-G widget: pipe the current zle buffer into `cs drop`.
#
# Secondary gesture for task-20260424-86e9 (C-DROP-GESTURE). Paired
# with the Hammerspoon hotkey (scripts/cs-drop.hammerspoon.lua): the
# chord works from any GUI app, this widget works inside a shell.
#
# Install:
#   ln -s <repo>/scripts/cs-drop.zsh ~/.config/zsh/config/cs-drop.zsh
# then source it from your zshrc (e.g. via the existing
# `~/.config/zsh/config/*.zsh` glob your dotfiles already run).
#
# Behaviour:
#   1. Capture the current command-line buffer ($BUFFER).
#   2. Shell out to `cs drop "$BUFFER"` — stdout/stderr flash into the
#      next prompt as a zle message; zle then redraws the prompt
#      below the spark id so the operator keeps flow.
#   3. Clear the buffer on success; preserve it on failure so the
#      operator can retry without retyping.
#
# Empty buffer: fallback to interactive read() so the widget still
# works as a chord outside a typed command.

cs-drop-widget() {
  local text="$BUFFER"
  if [[ -z "$text" ]]; then
    # Prompt inline — printed below the shell, read one line, fire.
    print -u2 -n "cosmon drop ✦ "
    read -r text || return
  fi
  if [[ -z "$text" ]]; then
    zle -M "cs-drop: empty text"
    return
  fi

  # Shell out. Use an absolute path fallback so the widget works in
  # shells where `cs` is not on PATH (e.g. a pristine zsh with no
  # dotfiles sourced).
  local cs_bin="${CS_BIN:-$HOME/.cargo/bin/cs}"
  if [[ ! -x "$cs_bin" ]] && command -v cs >/dev/null 2>&1; then
    cs_bin="$(command -v cs)"
  fi

  local out
  out="$("$cs_bin" --json drop "$text" 2>&1)"
  local rc=$?

  if (( rc == 0 )); then
    # Extract the molecule id from JSON without depending on jq.
    local id
    id="$(print -r -- "$out" | sed -n 's/.*"id":"\([a-z]*-[0-9a-z-]*\)".*/\1/p' | head -n 1)"
    zle -M "cosmon ✦ ${id:-drop dispatched}"
    BUFFER=""
  else
    zle -M "cs drop failed: $out"
  fi
  zle redisplay
}

zle -N cs-drop-widget
bindkey '^G' cs-drop-widget
