#!/bin/sh
# service-bundle.test.sh — offline contract for the opt-in service install.

set -eu

ROOT="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
INSTALLER="$ROOT/infra/install/install.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT INT TERM

make_stub() {
    _path="$1" _name="$2"
    cat >"$_path" <<EOF
#!/bin/sh
case "\${1:-}" in
  --version) echo "${_name} 9.9.9" ;;
  *) exit 0 ;;
esac
EOF
    chmod +x "$_path"
}

make_fixture() {
    _release="$1" _broken="${2:-no}"
    _client="$WORK/client" _service="$WORK/service"
    rm -rf "$_client" "$_service" "$_release"
    mkdir -p "$_client" "$_service/scripts/lib" "$_service/scripts/systemd" \
        "$_service/scripts/launchd" "$_release"
    make_stub "$_client/cs" cs
    make_stub "$_client/cosmon-remote" cosmon-remote
    make_stub "$_service/cosmon-daemon-supervisor" cosmon-daemon-supervisor
    make_stub "$_service/cosmon-scheduler" cosmon-scheduler
    cp "$ROOT/scripts/install-daemon-supervisor.sh" \
        "$ROOT/scripts/install-scheduler.sh" "$_service/scripts/"
    cp "$ROOT/scripts/lib/install-user-service.sh" "$_service/scripts/lib/"
    cp "$ROOT/scripts/systemd/"* "$_service/scripts/systemd/"
    cp "$ROOT/scripts/launchd/com.cosmon.daemon-supervisor.plist" \
        "$ROOT/scripts/launchd/com.cosmon.scheduler.plist" "$_service/scripts/launchd/"
    if [ "$_broken" = yes ]; then
        rm "$_service/scripts/systemd/cosmon-scheduler.timer"
    fi
    _target="x86_64-unknown-linux-musl"
    tar -czf "$_release/cosmon-9.9.9-${_target}.tar.gz" -C "$_client" .
    tar -czf "$_release/cosmon-service-9.9.9-${_target}.tar.gz" -C "$_service" .
    if command -v sha256sum >/dev/null 2>&1; then
        (cd "$_release" && sha256sum cosmon-*.tar.gz >SHA256SUMS)
    else
        (cd "$_release" && shasum -a 256 cosmon-*.tar.gz >SHA256SUMS)
    fi
}

run_install() {
    _home="$1" _release="$2"; shift 2
    HOME="$_home" XDG_CONFIG_HOME="$_home/.config" \
        COSMON_UNAME_S=Linux COSMON_UNAME_M=x86_64 \
        COSMON_RELEASE_BASE_URL="file://${_release}" \
        sh "$INSTALLER" --dir "$_home/bin" "$@"
}

release="$WORK/release"
home="$WORK/home"
make_fixture "$release"
mkdir -p "$home"

# Default remains the two-client-binary install.
run_install "$home" "$release" >/dev/null 2>&1
test -x "$home/bin/cs"
test -x "$home/bin/cosmon-remote"
test ! -e "$home/bin/cosmon-scheduler"
test ! -e "$home/.local/libexec/cosmon/install-scheduler.sh"

# A missing service component is refused before any service file is placed.
rm -rf "$home"; mkdir -p "$home"
make_fixture "$release" yes
if run_install "$home" "$release" --with-services >"$WORK/broken.out" 2>&1; then
    echo "service-bundle.test: incomplete service bundle was accepted" >&2
    exit 1
fi
test ! -e "$home/bin/cs"
test ! -e "$home/bin/cosmon-scheduler"
test ! -e "$home/.local/libexec/cosmon"

# A service archive changed after SHA256SUMS was written is also refused before
# installation; internal completeness checks do not replace archive integrity.
rm -rf "$home"; mkdir -p "$home"
make_fixture "$release"
printf 'corrupt\n' >>"$release/cosmon-service-9.9.9-x86_64-unknown-linux-musl.tar.gz"
if run_install "$home" "$release" --with-services >"$WORK/checksum.out" 2>&1; then
    echo "service-bundle.test: corrupt service bundle was accepted" >&2
    exit 1
fi
grep -qF 'checksum mismatch' "$WORK/checksum.out"
test ! -e "$home/bin/cs"
test ! -e "$home/.local/libexec/cosmon"

# Opt-in installs binaries and a self-contained script/template layout while
# preserving existing operator configuration byte-for-byte.
rm -rf "$home"; mkdir -p "$home/.config/cosmon"
printf '# operator supervisor config\n' >"$home/.config/cosmon/daemons.toml"
printf '# operator scheduler config\n' >"$home/.config/cosmon/patrols.toml"
mkdir -p "$home/.cosmon/state"
printf 'operator state\n' >"$home/.cosmon/state/sentinel"
cp "$home/.config/cosmon/daemons.toml" "$WORK/daemons.before"
cp "$home/.config/cosmon/patrols.toml" "$WORK/patrols.before"
cp "$home/.cosmon/state/sentinel" "$WORK/state.before"
make_fixture "$release"

mock="$WORK/mock-bin"; mkdir -p "$mock"
cat >"$mock/systemctl" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >>"$WORK/systemctl.log"
exit 0
EOF
cat >"$mock/systemd-analyze" <<'EOF'
#!/bin/sh
exit 0
EOF
cat >"$mock/uname" <<'EOF'
#!/bin/sh
echo Linux
EOF
chmod +x "$mock/systemctl" "$mock/systemd-analyze" "$mock/uname"
if ! PATH="$mock:$PATH" run_install "$home" "$release" --with-services \
    >"$WORK/install.out" 2>&1; then
    cat "$WORK/install.out" >&2
    exit 1
fi

for binary in cs cosmon-remote cosmon-daemon-supervisor cosmon-scheduler; do
    test -x "$home/bin/$binary" || { echo "missing binary: $binary" >&2; exit 1; }
    "$home/bin/$binary" --version | grep -q '9.9.9'
done
libexec="$home/.local/libexec/cosmon"
test -x "$libexec/install-daemon-supervisor.sh"
test -x "$libexec/install-scheduler.sh"
test -r "$libexec/lib/install-user-service.sh"
test -r "$libexec/systemd/cosmon-daemon-supervisor.service"
test -r "$libexec/systemd/cosmon-scheduler.service"
test -r "$libexec/systemd/cosmon-scheduler.timer"
cmp "$WORK/daemons.before" "$home/.config/cosmon/daemons.toml"
cmp "$WORK/patrols.before" "$home/.config/cosmon/patrols.toml"
grep -qF "$libexec/install-scheduler.sh status" "$WORK/install.out"
grep -qF "$libexec/install-daemon-supervisor.sh uninstall" "$WORK/install.out"
grep -qF 'enable cosmon-daemon-supervisor.service' "$WORK/systemctl.log"
grep -qF 'enable cosmon-scheduler.timer' "$WORK/systemctl.log"

# Reinstallation is idempotent with respect to operator-owned bytes.
PATH="$mock:$PATH" run_install "$home" "$release" --with-services \
    >/dev/null 2>&1
cmp "$WORK/daemons.before" "$home/.config/cosmon/daemons.toml"
cmp "$WORK/patrols.before" "$home/.config/cosmon/patrols.toml"
cmp "$WORK/state.before" "$home/.cosmon/state/sentinel"

echo "service-bundle.test: opt-in service distribution contract passed"
