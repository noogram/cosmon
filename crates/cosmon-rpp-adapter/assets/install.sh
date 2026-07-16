#!/bin/sh
# install.sh — cosmon-remote bootstrap (Phase 1, task-20260522-aad5).
#
# Served by the cosmon-server at `GET /install.sh`. Usage:
#
#     curl -fsSL <host>/install.sh | sh
#
# What this script does, and only this (YAGNI):
#   1. detect platform from `uname` → one of macos-{arm64,amd64} /
#      linux-{arm64,amd64};
#   2. download the matching cosmon-remote binary from
#      `<host>/dist/binary/<platform>/cosmon-remote` into
#      `~/.local/bin/cosmon-remote` and chmod +x;
#   3. initialise a persistent profile at
#      `~/.config/cosmon-remote/profiles/<profile-name>.toml` with the
#      four-tuple (sub, aud, oidc-url, noyau) templated by the server.
#      The server emits one `config set` line per non-empty field;
#      empty fields are skipped entirely (no placeholder leakage —
#      the Phase 0 `case-pattern` bug, fixed by moving the
#      conditional server-side).
#   4. drop a non-destructive **pilot-pack** so ANY agentic harness on
#      this machine (codex, opencode, gemini-cli, Claude Code, …) learns
#      to drive `cosmon-remote` in natural language, with zero per-project
#      setup. Three idempotent, never-clobbering artifacts:
#        - `~/.config/cosmon/pilot.AGENTS.md` — the canonical content
#          cosmon OWNS (rewritten on every run; single source of truth);
#        - a fenced *managed block* inside `~/AGENTS.md`, replaced only
#          between its markers (the conda/rbenv pattern — the rest of the
#          user's file is byte-preserved). `AGENTS.md` is the AAIF /
#          Linux-Foundation standard every modern harness reads;
#        - `~/CLAUDE.md` and `~/GEMINI.md` symlinked to `AGENTS.md` (one
#          file, every harness) — never clobbering a real file.
#      Opt out with `--no-pilot-pack` or `COSMON_SKIP_PILOT_PACK=1`.
#      Refresh later, standalone (no binary fetch): `sh install.sh --pilot-pack`.
#      The pilot-pack speaks the REMOTE surface (`do`/`result`/`events`/
#      `converse`, no `done`) because an avatar box usually has only
#      `cosmon-remote`. Origin: ADR-125 (Valence/Aperture) + the
#      `pilot-portability` / `piloting-cosmon-from-any-harness` guides.
#
# Server-side substitutions (placeholders intentionally not spelled
# literally in this comment: a multi-line substitution inside a
# `#`-prefixed block expands to executable shell on lines 2..n,
# which broke v1.3 smoke — only the line carrying the placeholder
# is commented, subsequent expanded lines are not):
#   HOST placeholder              → request base URL (scheme + host)
#   CONFIG_SET_BLOCK placeholder  → multi-line block of cosmon-remote
#                                   config set commands, one per
#                                   non-empty field in the deployment's
#                                   templating config. Empty when
#                                   nothing is configured.
#
# To override the host without reinstalling:
#     curl -fsSL <host>/install.sh | COSMON_HOST=https://autre-host sh
#
# Phase 0 served a justfile; Phase 1 ships a real Rust CLI. The
# justfile endpoint stays mounted (`/dist/justfile`) for backwards
# compatibility — tenants on the old install can keep using `just`.
#
# Aucune autorité n'est embarquée: l'authentification reste entièrement
# côté serveur (JWT OIDC + ACL Tailscale). Ce script ne porte aucun
# secret.
set -eu

COSMON_HOST="${COSMON_HOST:-__COSMON_HOST__}"
BIN_DIR="${COSMON_BIN_DIR:-$HOME/.local/bin}"

say()  { printf '\033[1;36m▸\033[0m %s\n' "$1"; }
ok()   { printf '\033[1;32m✓\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33m!\033[0m %s\n' "$1" >&2; }
die()  { printf '\033[1;31m✗\033[0m %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# Profile name derived from the host: strip scheme + trailing slash,
# replace dots/colons/slashes with dashes.
# e.g. `https://tenant-demo.tailnet0.ts.net` → `tenant-demo-tailnet0-ts-net`.
# `sed -E` (POSIX 2024 ERE) — NOT the GNU-only BRE `\?` — so the same
# pipeline behaves identically under BSD sed (macOS) and GNU sed.
derive_profile_name() {
    printf '%s' "$1" | sed -E -e 's|^https?://||' -e 's|/$||' -e 's|[/.:]|-|g'
}

# Self-test hook: `sh install.sh --derive-profile-name <host>` prints
# the derived profile name and exits — lets the BSD/GNU sed portability
# gate exercise the exact pipeline shipped in this script instead of a
# copy of it.
if [ "${1:-}" = "--derive-profile-name" ]; then
    derive_profile_name "${2:?usage: install.sh --derive-profile-name <host>}"
    printf '\n'
    exit 0
fi

# ── Pilot-pack — make this machine pilotable by any harness ──────────
# Markers that fence the cosmon-owned region inside a user's AGENTS.md.
# They are STABLE (never templated) so a refresh can find and replace
# exactly this block, leaving every other line of the file untouched.
PILOT_PACK_BEGIN='# >>> cosmon pilot-pack >>>'
PILOT_PACK_END='# <<< cosmon pilot-pack <<<'

# The canonical pilot-pack body — the REMOTE surface (avatar boxes carry
# `cosmon-remote`, not `cs`). Quoted heredoc: no expansion, the content
# ships byte-for-byte. The markers are part of the body so the same file
# is reused verbatim whether we create, append, or replace.
pilot_pack_content() {
    cat <<'PILOT_PACK_EOF'
# >>> cosmon pilot-pack >>>
## Piloting a remote cosmon avatar from this machine

This machine talks to a **cosmon** fleet — a stateless system that gives AI
workers a persistent identity and a typed lifecycle. You drive every piece of
work through ONE binary already on your PATH: `cosmon-remote` (alias `cosmon`).
Never ssh, never docker exec — everything goes through it, the same way you
drive `git` from the shell. There is no MCP and no plugin to install.

### The cycle (memorise this order)

    cosmon-remote auth login --email you@example.com     # connect the worker badge (ONCE)
    cosmon-remote do <formula> --topic "…" --kind <k>    # nucleate + tackle + follow  [costs credit]
    cosmon-remote molecule result <id>                   # fetch the deliverable
    cosmon-remote molecule list                          # state of your molecules

`do` is the headline verb: in one gesture it creates the work, spawns one
worker, and follows it until the work reaches a terminal state.

### Hard rules — violating these wastes credit or confuses the fleet

- Only `do` and `molecule tackle` burn credit. Reads (`list`, `get`, `result`,
  `events`, `quota`, `converse`) are cheap.
- Read live quota with `cosmon-remote quota` — never a memorised number.
- There is NO `done` / `kill` / `evolve` / `run` on this client. Teardown is
  server-side; the absence is a deliberate refusal (the §8p frozen surface).
  Do not look for them.
- `--json` is on every command. Parse JSON, not human prose.
- Output is files, not messages: read the deliverable with `molecule result`;
  do not try to "message" a molecule.

### Discover the rest yourself

    cosmon-remote --help            # the verb tree
    man cosmon-remote               # the full reference (generated, drift-proof)
    cosmon-remote doctor            # when something breaks

Drift-proof source of this block: ~/.config/cosmon/pilot.AGENTS.md
Refresh it any time with: sh install.sh --pilot-pack
# <<< cosmon pilot-pack <<<
PILOT_PACK_EOF
}

# Idempotent, non-destructive drop. Safe to run any number of times; the
# user's own AGENTS.md content outside the markers is never touched.
pilot_pack_drop() {
    pp_cfg_dir="$HOME/.config/cosmon"
    pp_canon="$pp_cfg_dir/pilot.AGENTS.md"
    mkdir -p "$pp_cfg_dir"
    # cosmon OWNS this file — rewrite it wholesale (single source of truth).
    pilot_pack_content > "$pp_canon"
    ok "pilot-pack écrit → $pp_canon"

    pp_agents="$HOME/AGENTS.md"
    if [ -f "$pp_agents" ] && grep -Fq "$PILOT_PACK_BEGIN" "$pp_agents"; then
        # Replace ONLY the managed region, in place, preserving position
        # and every surrounding line byte-for-byte. awk emits the fresh
        # canonical block (which carries its own markers) where the old
        # BEGIN line was, then drops the old region up to and including
        # END.
        pp_tmp="$(mktemp)"
        awk -v b="$PILOT_PACK_BEGIN" -v e="$PILOT_PACK_END" -v blk="$pp_canon" '
            function emit_block(   line) {
                while ((getline line < blk) > 0) print line
                close(blk)
            }
            $0 == b { emit_block(); inblk = 1; next }
            inblk && $0 == e { inblk = 0; next }
            inblk { next }
            { print }
        ' "$pp_agents" > "$pp_tmp" && mv "$pp_tmp" "$pp_agents"
        ok "bloc pilot-pack rafraîchi dans $pp_agents (reste du fichier intact)"
    elif [ -f "$pp_agents" ]; then
        # File exists, no managed block yet — append, never clobber.
        { printf '\n'; cat "$pp_canon"; } >> "$pp_agents"
        ok "bloc pilot-pack ajouté à $pp_agents (contenu existant préservé)"
    else
        # No file — create one with a human-facing header above the block.
        {
            printf '# Agent instructions for this machine (AAIF AGENTS.md standard).\n'
            printf '# The block below is managed by cosmon — edit only OUTSIDE the markers.\n\n'
            cat "$pp_canon"
        } > "$pp_agents"
        ok "$pp_agents créé avec le bloc pilot-pack"
    fi

    # One file, every harness: symlink the harness-specific names to
    # AGENTS.md. Never clobber a real file (same guard as the `cosmon`
    # alias above): only link when absent or already our own symlink.
    for pp_name in CLAUDE.md GEMINI.md; do
        pp_link="$HOME/$pp_name"
        if [ ! -e "$pp_link" ] || [ "$(readlink "$pp_link" 2>/dev/null || true)" = "AGENTS.md" ]; then
            ln -sf AGENTS.md "$pp_link"
            say "lien posé: $pp_link → AGENTS.md"
        else
            warn "$pp_link existe déjà (fichier réel) — lien non posé"
        fi
    done

    say "par projet, colle cette ligne dans le AGENTS.md du dépôt : @$pp_canon"
}

# Standalone refresh hook: `sh install.sh --pilot-pack` re-drops the
# pilot-pack only (no binary fetch, no profile, no host needed). The
# install-time path calls the same function, so the two never drift.
if [ "${1:-}" = "--pilot-pack" ]; then
    pilot_pack_drop
    exit 0
fi

# Opt-out: `--no-pilot-pack` flag OR COSMON_SKIP_PILOT_PACK=1 in the env
# (works with `curl … | COSMON_SKIP_PILOT_PACK=1 sh`).
SKIP_PILOT_PACK="${COSMON_SKIP_PILOT_PACK:-}"
if [ "${1:-}" = "--no-pilot-pack" ]; then
    SKIP_PILOT_PACK=1
fi

[ -n "$COSMON_HOST" ] || die "COSMON_HOST vide — relancez avec COSMON_HOST=https://<host> sh"
have curl || die "curl introuvable — installez curl puis relancez"

# ── 1. Detect platform ───────────────────────────────────────────────
os="$(uname -s)"
arch="$(uname -m)"
case "$os/$arch" in
    Darwin/arm64)               PLATFORM=macos-arm64 ;;
    Darwin/x86_64)              PLATFORM=macos-amd64 ;;
    Linux/aarch64|Linux/arm64)  PLATFORM=linux-arm64 ;;
    Linux/x86_64|Linux/amd64)   PLATFORM=linux-amd64 ;;
    *) die "platform non supportée: $os/$arch" ;;
esac
say "platform: $PLATFORM"

# ── 2. Download binary ───────────────────────────────────────────────
mkdir -p "$BIN_DIR"
BIN_PATH="$BIN_DIR/cosmon-remote"
URL="$COSMON_HOST/dist/binary/$PLATFORM/cosmon-remote"
say "téléchargement: $URL → $BIN_PATH"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
# No `--proto '=https' --tlsv1.2`: tenant deployments run over both
# HTTPS (public Tailscale Serve) and HTTP (loopback / Tailscale-only
# internal endpoint such as the AWS Tenant-Demo VM at http://127.0.0.1:8443).
# Confidentiality and authority are enforced by Tailscale ACLs +
# VPN segmentation + the JWT layer above this script, not by curl's
# scheme guard. Keeping the scheme free unblocks `curl <host>/install.sh
# | sh` on every supported topology.
curl -fsSL "$URL" -o "$tmp" \
    || die "échec du téléchargement de $URL"
[ -s "$tmp" ] || die "binaire vide — le serveur a-t-il les binaires pré-construits ?"
mv "$tmp" "$BIN_PATH"
chmod +x "$BIN_PATH"
trap - EXIT

# ── 2b. Pose the `cosmon` alias (avatar-surface A2, additive) ───────
# Same binary, product-facing name; help renders under the invoked
# name. Never clobbers a foreign `cosmon` binary.
ALIAS_PATH="$BIN_DIR/cosmon"
if [ ! -e "$ALIAS_PATH" ] || [ "$(readlink "$ALIAS_PATH" 2>/dev/null || true)" = "cosmon-remote" ]; then
    ln -sf cosmon-remote "$ALIAS_PATH"
    say "alias posé: $ALIAS_PATH → cosmon-remote"
else
    warn "un 'cosmon' étranger existe déjà ($ALIAS_PATH) — alias non posé"
fi

# ── 2c. Pose the man page (smithy C5, task-20260614-b807) ─────────
# The binary is its own man-page source: the hidden `__man-page`
# subcommand renders the live clap tree through `clap_mangen` to
# stdout (one source, two readers — see cosmon-remote/src/main.rs
# print_man_page). We pose that output directly, so the installed
# page can never drift from the installed binary.
#
# Location: `~/.local/share/man/man1/`. This is NOT an arbitrary
# choice — `man` derives its search path from PATH (`manpath`): for
# each `…/bin` on PATH it also probes the sibling `…/share/man`.
# Because the binary lands in `$BIN_DIR` (default `~/.local/bin`),
# `$BIN_DIR/../share/man` is searched automatically on both macOS
# (BSD man) and Linux (man-db) — no MANPATH export needed.
MAN_DIR="${COSMON_MAN_DIR:-$(dirname "$BIN_DIR")/share/man/man1}"
if mkdir -p "$MAN_DIR" 2>/dev/null \
   && "$BIN_PATH" __man-page > "$MAN_DIR/cosmon-remote.1" 2>/dev/null \
   && [ -s "$MAN_DIR/cosmon-remote.1" ]; then
    # Alias page for the product-facing name. The rendered page always
    # carries the canonical `.TH cosmon-remote` header (see
    # print_man_page); the symlink just lets `man cosmon` resolve it.
    ln -sf cosmon-remote.1 "$MAN_DIR/cosmon.1"
    # Refresh the index if the host maintains one (man-db); BSD man
    # needs no index. Never fail the install on a missing tool.
    if have mandb; then
        mandb -q "$(dirname "$MAN_DIR")" >/dev/null 2>&1 || true
    fi
    ok "page man posée → $MAN_DIR/cosmon-remote.1 (man cosmon-remote)"
else
    warn "page man non posée — \`$BIN_PATH __man-page\` a échoué (non bloquant)"
fi

# ── 2b. PATH — no crutch (smithy C1, jobs geste 2) ────────────────
# Either BIN_DIR is already on the PATH (nothing to say), or this
# script configures the user's shell itself and announces it once, in
# green, AFTER the fact. Telling the user to edit their rc file is
# never a precondition we hand back to them.
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        rc_file=""
        case "${SHELL:-}" in
            */zsh)  rc_file="$HOME/.zshrc" ;;
            */bash) rc_file="$HOME/.bashrc" ;;
            *)      rc_file="$HOME/.profile" ;;
        esac
        path_line="export PATH=\"$BIN_DIR:\$PATH\""
        if [ -f "$rc_file" ] && grep -F "$path_line" "$rc_file" >/dev/null 2>&1; then
            : # already configured by a previous install — idempotent
        else
            {
                printf '\n# cosmon-remote (ajouté par install.sh)\n'
                printf '%s\n' "$path_line"
            } >> "$rc_file"
        fi
        PATH="$BIN_DIR:$PATH"
        export PATH
        ok "PATH configuré dans $rc_file — actif dans tout nouveau terminal"
        ;;
esac

# ── 3. Persist initial profile ──────────────────────────────────────
PROFILE="${COSMON_PROFILE_NAME:-}"
if [ -z "$PROFILE" ]; then
    PROFILE="$(derive_profile_name "$COSMON_HOST")"
fi
say "profile: $PROFILE"

"$BIN_PATH" config init "$PROFILE" "$COSMON_HOST" \
    || die "cosmon-remote config init failed"

# Server-templated block of `config set` commands. One line per
# non-empty deployment field; empty when nothing is configured.
__COSMON_CONFIG_SET_BLOCK__

# ── 4. Doctor — named green/red checks, run BY the installer ────────
# Each red line names its repair command; the install itself never
# fails on a red check (the binary and profile are in place, the
# remaining steps are the user's — doctor just tells them which).
ok "cosmon-remote installé → $BIN_PATH"
ok "profil persisté → ~/.config/cosmon-remote/profiles/$PROFILE.toml"
say "vérification de l'installation (doctor) :"
"$BIN_PATH" --profile "$PROFILE" doctor \
    || warn "des checks sont rouges — chaque ligne ✗ ci-dessus nomme sa commande de réparation"

# ── 5. Pilot-pack — teach every harness on this box to drive cosmon ──
if [ -z "$SKIP_PILOT_PACK" ]; then
    say "pilot-pack : ton avatar devient pilotable par tout harness (codex, opencode, gemini-cli, Claude Code) :"
    pilot_pack_drop
else
    say "pilot-pack ignoré (--no-pilot-pack / COSMON_SKIP_PILOT_PACK)"
fi

cat <<EOF

Prêt. Depuis n'importe quel répertoire :

    # connecte le worker Claude (une fois) — remplace l'email d'exemple par le tien, tel quel, sans chevrons :
    cosmon-remote auth login --email toi@exemple.fr
    cosmon-remote molecule nucleate task-work --topic "..." --kind task
    cosmon-remote molecule tackle <id>                 # lance le travail (coûte du crédit)
    cosmon-remote molecule result <id>                 # récupère le livrable

Quand quelque chose casse : cosmon-remote doctor
Pour basculer le profile par défaut : cosmon-remote config use $PROFILE
Pour rafraîchir le pilot-pack plus tard : sh install.sh --pilot-pack
EOF
