#!/usr/bin/env bash
# ship-lpthe.sh — assemble the versioned sovereign-cosmon tarball (static musl
# `cs` + the IP that travels as data: formulas, skills, config, provision +
# backup scripts + MANIFEST) and `scp -J tycho` it to g5:/home/tmp/cosmon/bin/.
# C2 of delib-20260705-7288, D1 distribution model.
#
# The binary is TRANSPORT; the formulas + skills + config are the INTELLECTUAL
# PROPERTY and travel alongside as plain data (architect Q1). g5 unpacks the
# tarball and runs provision.sh to converge an instance.
#
# NETWORK PATH: the Mac reaches g5 only through the tycho jump host. This script
# uses `-J tycho` (ProxyJump) exactly as the C2 brief specifies; ssh config
# already declares Host tycho + Host g5lp (ProxyJump tycho).
#
# Usage:
#   ship-lpthe.sh [--remote USER@HOST] [--jump HOST] [--dest DIR]
#                 [--build] [--dry-run]
#     --remote  target ssh spec.        Default: $USER@g5
#     --jump    ProxyJump host.         Default: tycho
#     --dest    remote dir for tarball. Default: /home/tmp/cosmon/dist
#     --bindir  remote dir for cs.      Default: /home/tmp/cosmon/bin
#     --build   run build-cs-musl.sh first (else expect dist/lpthe/cs present).
#     --dry-run assemble the tarball, print the scp plan, transfer NOTHING.
#
# Exit: 0 shipped (or dry-run planned) · 2 usage · 3 missing artifact · 4 scp failed.
set -euo pipefail

SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SELF/../.." && pwd)"

REMOTE="$USER@g5"
JUMP="tycho"
DEST="/home/tmp/cosmon/dist"
BINDIR="/home/tmp/cosmon/bin"
DIST="$REPO/dist/lpthe"
DO_BUILD=0
DRY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --remote)  REMOTE="$2"; shift 2 ;;
    --jump)    JUMP="$2"; shift 2 ;;
    --dest)    DEST="$2"; shift 2 ;;
    --bindir)  BINDIR="$2"; shift 2 ;;
    --build)   DO_BUILD=1; shift ;;
    --dry-run) DRY=1; shift ;;
    -h|--help) sed -n '1,32p' "$0"; exit 0 ;;
    *) echo "ship-lpthe: unknown arg: $1" >&2; exit 2 ;;
  esac
done

[ "$DO_BUILD" = 1 ] && "$SELF/build-cs-musl.sh" --out "$DIST"
[ -x "$DIST/cs" ] || { echo "ship-lpthe: no built binary at $DIST/cs (run with --build or build-cs-musl.sh first)" >&2; exit 3; }

VER="$(awk -F'"' '/^version[[:space:]]*=/{print $2; exit}' "$REPO/Cargo.toml")"
GITHASH="$(cd "$REPO" && git rev-parse --short HEAD 2>/dev/null || echo unknown)"
PKG="cosmon-lpthe-${VER}-${GITHASH}"
STAGE="$(mktemp -d)/${PKG}"
trap 'rm -rf "$(dirname "$STAGE")"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/share" "$STAGE/scripts"

echo "==> assembling $PKG"
# 1. the binary + its manifest.
cp "$DIST/cs" "$STAGE/bin/cs"; chmod +x "$STAGE/bin/cs"
[ -f "$DIST/MANIFEST.txt" ] && cp "$DIST/MANIFEST.txt" "$STAGE/MANIFEST.txt"
# 2. the IP as data: formulas, skills, config.
cp -R "$REPO/.cosmon/formulas" "$STAGE/share/formulas"
[ -d "$REPO/.cosmon/skills" ] && cp -R "$REPO/.cosmon/skills" "$STAGE/share/skills"
cp "$REPO/.cosmon/config.toml" "$STAGE/share/config.toml"
# agent/persona defs: cosmon personas are baked into formula step prompts (e.g.
# deep-think). The `.claude/commands` are the operator-facing command defs — ship
# them too so the sovereign instance has the same surface.
[ -d "$REPO/.claude/commands" ] && { mkdir -p "$STAGE/share/claude"; cp -R "$REPO/.claude/commands" "$STAGE/share/claude/commands"; }
# 3. the provisioning + backup scripts.
cp "$SELF/provision.sh" "$SELF/cosmon-state-backup.sh" "$STAGE/scripts/"
chmod +x "$STAGE/scripts/"*.sh
[ -f "$SELF/README.md" ] && cp "$SELF/README.md" "$STAGE/README.md"

# counts for the log
NF="$(find "$STAGE/share/formulas" -name '*.toml' | wc -l | tr -d ' ')"
echo "    bin/cs, $NF formulas, skills, config.toml, provision.sh, cosmon-state-backup.sh"

TARBALL="$REPO/dist/${PKG}.tar.gz"
mkdir -p "$REPO/dist"
# COPYFILE_DISABLE stops macOS bsdtar from embedding ._* AppleDouble xattr
# headers that make GNU tar on g5 warn "unknown extended header keyword".
COPYFILE_DISABLE=1 tar -czf "$TARBALL" -C "$(dirname "$STAGE")" "$PKG"
TB_B3="$(b3sum "$TARBALL" 2>/dev/null | awk '{print $1}')" || TB_B3="(b3sum absent)"
TB_SZ="$(wc -c < "$TARBALL" | tr -d ' ')"
echo "==> packed $TARBALL ($TB_SZ bytes)"
echo "    tarball BLAKE3 = $TB_B3"

REMOTE_TARBALL="$DEST/$(basename "$TARBALL")"
if [ "$DRY" = 1 ]; then
  echo "==> DRY-RUN — would transfer via jump '$JUMP':"
  echo "    ssh  -J $JUMP $REMOTE 'mkdir -p $DEST $BINDIR'"
  echo "    scp  -J $JUMP $TARBALL $REMOTE:$REMOTE_TARBALL"
  echo "    ssh  -J $JUMP $REMOTE 'tar -xzf $REMOTE_TARBALL -C $DEST && \\"
  echo "         install -m755 $DEST/$PKG/bin/cs $BINDIR/cs'"
  echo "    then on g5:  $DEST/$PKG/scripts/provision.sh --prefix $DEST/$PKG"
  exit 0
fi

echo "==> transferring to $REMOTE via -J $JUMP"
ssh -J "$JUMP" "$REMOTE" "mkdir -p '$DEST' '$BINDIR'" \
  || { echo "ship-lpthe: remote mkdir failed (is g5 reachable via $JUMP?)" >&2; exit 4; }
scp -J "$JUMP" "$TARBALL" "$REMOTE:$REMOTE_TARBALL" \
  || { echo "ship-lpthe: scp failed" >&2; exit 4; }
# Unpack + install the binary into the canonical /home/tmp/cosmon/bin, and
# verify the BLAKE3 seal on the far side.
ssh -J "$JUMP" "$REMOTE" "
  set -e
  tar -xzf '$REMOTE_TARBALL' -C '$DEST'
  install -m755 '$DEST/$PKG/bin/cs' '$BINDIR/cs'
  if command -v b3sum >/dev/null 2>&1; then
    got=\$(b3sum '$BINDIR/cs' | awk '{print \$1}')
    want=\$(awk -F'= *' '/^cs_blake3/{print \$2}' '$DEST/$PKG/MANIFEST.txt' | tr -d ' ')
    [ \"\$got\" = \"\$want\" ] && echo \"remote BLAKE3 seal OK (\$got)\" || { echo \"remote BLAKE3 MISMATCH want=\$want got=\$got\" >&2; exit 5; }
  fi
  echo 'installed:' && '$BINDIR/cs' --version
" || { echo "ship-lpthe: remote unpack/verify failed" >&2; exit 4; }

echo "==> shipped. On g5, converge with:"
echo "    $DEST/$PKG/scripts/provision.sh --prefix $DEST/$PKG"
