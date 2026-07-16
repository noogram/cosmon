#!/usr/bin/env bash
# init-cluster-config.sh — seed a first-cut `~/.config/cosmon/cluster.toml`
# for the local device.
#
# The script inspects `tailscale ip -4` and `tailscale status --json`
# (when available) to fill in the primary host's Tailscale IP and
# hostname, scans `/srv/cosmon/*/` to list installed galaxies, and
# writes a commented TOML seed. The operator can then edit the file
# with `$EDITOR` or `cs cluster edit` to add surfaces, collaborators,
# etc.
#
# This script is a **fallback** for bare machines where `cs` is not yet
# installed. Once `cs` is on PATH, prefer `cs cluster edit` which seeds
# the same template via the Rust helper.
#
# Usage:
#   init-cluster-config.sh                      # seed default path
#   init-cluster-config.sh --path /tmp/x.toml   # custom location
#   init-cluster-config.sh --force              # overwrite if present
#
# Exit codes:
#   0  seeded (or no-op because file already exists without --force)
#   1  usage error
#   2  could not determine host details
#
# See ADR-066 for the design.

set -eu -o pipefail

PATH_OVERRIDE=""
FORCE=0
NAME="you-local"
OWNER="${USER:-you}"
while [ $# -gt 0 ]; do
    case "$1" in
        --path)
            PATH_OVERRIDE="$2"
            shift 2
            ;;
        --force)
            FORCE=1
            shift
            ;;
        --name)
            NAME="$2"
            shift 2
            ;;
        --owner)
            OWNER="$2"
            shift 2
            ;;
        -h|--help)
            sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "unknown flag: $1" >&2
            exit 1
            ;;
    esac
done

CLUSTER_PATH="${PATH_OVERRIDE:-${COSMON_CLUSTER_CONFIG:-$HOME/.config/cosmon/cluster.toml}}"

if [ -e "$CLUSTER_PATH" ] && [ "$FORCE" -ne 1 ]; then
    echo "cluster config already exists at $CLUSTER_PATH (use --force to overwrite)" >&2
    exit 0
fi

mkdir -p "$(dirname "$CLUSTER_PATH")"

TAILSCALE_IP=""
TAILSCALE_HOSTNAME=""
TAILNET_DOMAIN=""

if command -v tailscale >/dev/null 2>&1; then
    TAILSCALE_IP="$(tailscale ip -4 2>/dev/null | head -n1 || true)"
    if command -v jq >/dev/null 2>&1; then
        STATUS_JSON="$(tailscale status --json 2>/dev/null || echo '{}')"
        TAILSCALE_HOSTNAME="$(echo "$STATUS_JSON" | jq -r '.Self.HostName // ""')"
        TAILNET_DOMAIN="$(echo "$STATUS_JSON" | jq -r '.MagicDNSSuffix // ""')"
    fi
fi

if [ -z "$TAILSCALE_HOSTNAME" ]; then
    TAILSCALE_HOSTNAME="$(hostname -s 2>/dev/null || hostname)"
fi

# TOML table keys must not contain whitespace or special chars; slugify
# to lowercase alphanumerics + dashes for the key while keeping the
# original string in `tailscale_hostname`.
HOST_KEY="$(echo "$TAILSCALE_HOSTNAME" | tr '[:upper:]' '[:lower:]' | tr -c 'a-z0-9-' '-' | sed 's/-\{2,\}/-/g' | sed 's/^-//;s/-$//')"
if [ -z "$HOST_KEY" ]; then
    HOST_KEY="primary"
fi

NOW="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

GALAXIES_NOTE=""
if [ -d "$HOME/galaxies" ]; then
    FOUND="$(find "$HOME/galaxies" -maxdepth 2 -name ".cosmon" -type d 2>/dev/null | wc -l | tr -d ' ')"
    if [ "${FOUND:-0}" -gt 0 ]; then
        GALAXIES_NOTE="# Detected $FOUND galaxy/galaxies under \$HOME/galaxies/"
    fi
fi

IP_OUT="${TAILSCALE_IP:-<TO_FILL>}"

cat >"$CLUSTER_PATH" <<EOF
# ~/.config/cosmon/cluster.toml — cosmon cluster topology (ADR-066)
#
# Seeded by scripts/init-cluster-config.sh on $NOW.
# $GALAXIES_NOTE
#
# Edit any <TO_FILL> placeholder before first use.

schema_version = 1

[cluster]
name = "$NAME"
owner_nucleon_id = "$OWNER"
tailnet_domain = "$TAILNET_DOMAIN"
updated_at = "$NOW"

[host.$HOST_KEY]
tailscale_ip = "$IP_OUT"
tailscale_hostname = "$TAILSCALE_HOSTNAME"
role = "primary"

[surfaces.cs_api]
host = "$HOST_KEY"
port = 4222
launchagent = "dev.noogram.cosmon.cs-api"

[surfaces.matrix_echo_tick]
host = "$HOST_KEY"
launchagent = "dev.noogram.cosmon.matrix-tick"
# References only — credentials live in the file below, never here.
credentials_file = "~/.config/cosmon-matrix-tick/credentials.toml"
room_id = "<TO_FILL>"

[apps]
mac_pilot_bundle_id = "dev.noogram.cosmon.mac-pilot"
ios_pilot_bundle_id = "dev.noogram.cosmon.ios-pilot"
EOF

chmod 0600 "$CLUSTER_PATH"
echo "seeded $CLUSTER_PATH"
echo "next: \$EDITOR $CLUSTER_PATH   (or: cs cluster edit)"
