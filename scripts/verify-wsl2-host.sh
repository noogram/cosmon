#!/usr/bin/env bash
# Reproducible, resumable witness for the Linux user-service release candidate.
set -euo pipefail

phase="${1:-${COSMON_WSL2_PHASE:-}}"
run_id="${COSMON_WSL2_RUN_ID:-candidate}"
state_root="${COSMON_WSL2_STATE_ROOT:-$HOME/.cosmon/wsl2-host-validation}"
run_root="$state_root/runs/$run_id"
checkpoint_dir="$run_root/checkpoints"
log_dir="$run_root/logs"
install_dir="$run_root/bin"
candidate_dir="${COSMON_CANDIDATE_DIR:-}"
identity_repo="${COSMON_WSL2_IDENTITY_REPO:-}"

fail() {
    echo "verify-wsl2-host: $*" >&2
    exit 1
}

usage() {
    cat >&2 <<'EOF'
usage: verify-wsl2-host.sh PHASE

Inside-distribution phases:
  preflight             record platform and prerequisite facts
  published-red         prove the published installer lacks service assets
  candidate-install     install candidate archives through install.sh
  service-baseline      configure and prove the supervisor child and timer
  lifecycle             run one real tackle-to-done lifecycle
  supervisor-crash      kill the supervisor main process and prove recovery
  child-crash           kill its child and prove supervised recovery
  timer                  prove the next timer firing occurs within 90 seconds
  before-logout          checkpoint before the external driver closes the shell
  after-logout           resume after the external driver closes the last shell
  before-distribution    checkpoint before distribution shutdown
  after-distribution     resume after distribution shutdown and relaunch
  before-reboot          checkpoint before host reboot
  after-reboot           resume after host reboot and distribution launch
  before-sleep           start an in-flight detached patrol for the sleep phase
  after-sleep            resume after host sleep and wake
  final                  record the remaining service and state checks

External-driver boundaries (this script never performs them):
  logout, distribution shutdown/relaunch, host reboot/distribution launch,
  and host sleep/resume.

Environment:
  COSMON_WSL2_RUN_ID       stable run name (default: candidate)
  COSMON_CANDIDATE_DIR     directory containing install.sh, SHA256SUMS, archives
  COSMON_WSL2_ADAPTER      configured adapter for the single lifecycle probe
  COSMON_WSL2_MODEL        configured model for the single lifecycle probe
  COSMON_WSL2_IDENTITY_REPO existing repository supplying Git identity when
                            the account has no global user.name/user.email
  COSMON_WSL2_WINDOWS_BOOT_EPOCH Windows boot time as Unix seconds, required
                            for reboot phases when powershell.exe is unavailable
EOF
}

git_identity() {
    local key="$1" value
    value="$(git config --global --get "$key" 2>/dev/null || true)"
    if [[ -z "$value" && -n "$identity_repo" ]]; then
        [[ -d "$identity_repo/.git" ]] \
            || fail "COSMON_WSL2_IDENTITY_REPO is not a Git repository"
        value="$(git -C "$identity_repo" config --local --get "$key" 2>/dev/null || true)"
    fi
    [[ -n "$value" ]] || fail "existing Git $key is required"
    printf '%s\n' "$value"
}

[[ -n "$phase" ]] || { usage; exit 2; }
mkdir -p "$checkpoint_dir" "$log_dir"
if [[ "${COSMON_WSL2_TEST_DIRECT_OUTPUT:-0}" != 1 ]]; then
    exec > >(tee -a "$log_dir/$phase.log") 2>&1
fi

checkpoint() {
    local name="$1"
    printf '%s\n' "$(date --iso-8601=seconds)" >"$checkpoint_dir/$name"
}

distribution_boot_time() {
    local value
    value="$(uptime -s)" || fail "could not read the distribution boot time"
    [[ -n "$value" ]] || fail "distribution boot time is empty"
    printf '%s\n' "$value"
}

windows_boot_epoch() {
    local value="${COSMON_WSL2_WINDOWS_BOOT_EPOCH:-}"
    if [[ -z "$value" ]] && command -v powershell.exe >/dev/null 2>&1; then
        value="$(powershell.exe -NoProfile -NonInteractive -Command \
            '([DateTimeOffset]((Get-CimInstance Win32_OperatingSystem).LastBootUpTime)).ToUnixTimeSeconds()' \
            | tr -d '\r' | tail -n 1)" \
            || fail "could not read the Windows boot time through powershell.exe"
    fi
    [[ "$value" =~ ^[0-9]+$ ]] \
        || fail "Windows boot time is required as COSMON_WSL2_WINDOWS_BOOT_EPOCH when powershell.exe interop is unavailable"
    printf '%s\n' "$value"
}

record_distribution_boot() {
    local checkpoint_name="$1"
    distribution_boot_time >"$checkpoint_dir/$checkpoint_name.distribution-boot"
}

report_distribution_transition() {
    local before_name="$1" after_name="$2" before after disposition
    before="$(cat "$checkpoint_dir/$before_name.distribution-boot")"
    after="$(cat "$checkpoint_dir/$after_name.distribution-boot")"
    if [[ "$before" == "$after" ]]; then
        disposition=SURVIVED
    else
        disposition=RESTARTED
    fi
    printf 'boundary=%s distribution=%s before=%s after=%s\n' \
        "${after_name#after-}" "$disposition" "$before" "$after"
}

require_checkpoint() {
    [[ -s "$checkpoint_dir/$1" ]] || fail "phase '$1' has not completed"
}

bounded_wait() {
    local seconds="$1" description="$2"
    shift 2
    local deadline=$((SECONDS + seconds))
    while (( SECONDS < deadline )); do
        if "$@"; then return 0; fi
        sleep 1
    done
    fail "timed out after ${seconds}s waiting for ${description}"
}

require_inside_host() {
    [[ "$(uname -s)" == Linux ]] || fail "this phase requires Linux"
    systemctl --user show-environment >/dev/null 2>&1 \
        || fail "the per-user service manager is unreachable"
}

unit_value() {
    systemctl --user show "$1" -p "$2" --value
}

state_is_readable() {
    local path="$1"
    [[ -s "$path" ]] && python3 -m json.tool "$path" >/dev/null
}

timer_firing_count() {
    local path="$run_root/probes/timer-firings"
    if [[ -f "$path" ]]; then
        wc -l <"$path"
    else
        printf '0\n'
    fi
}

fresh_heartbeat() {
    local path="$run_root/probes/heartbeat"
    [[ -s "$path" ]] || return 1
    local now seen
    now="$(date +%s)"
    seen="$(cat "$path")"
    (( now - seen <= 8 ))
}

one_probe_child() {
    local pid_file="$run_root/probes/child.pid" pid
    [[ -s "$pid_file" ]] || return 1
    pid="$(cat "$pid_file")"
    kill -0 "$pid" 2>/dev/null || return 1
    [[ "$(pgrep -fc "$run_root/probes/supervised-child" || true)" -eq 1 ]]
}

services_ready() {
    systemctl --user is-active --quiet cosmon-daemon-supervisor.service \
        && systemctl --user is-active --quiet cosmon-scheduler.timer \
        && fresh_heartbeat && one_probe_child
}

record_service_snapshot() {
    systemctl --user show cosmon-daemon-supervisor.service \
        -p LoadState -p UnitFileState -p ActiveState -p SubState -p MainPID \
        -p NRestarts -p KillMode -p FragmentPath --no-pager
    systemctl --user show cosmon-scheduler.timer \
        -p LoadState -p UnitFileState -p ActiveState -p SubState \
        -p NextElapseUSecRealtime -p FragmentPath --no-pager
    systemctl --user show cosmon-scheduler.service \
        -p LoadState -p ActiveState -p SubState -p MainPID -p KillMode \
        -p FragmentPath --no-pager
}

case "$phase" in
preflight)
    require_inside_host
    [[ "$(id -u)" -ne 0 ]] || fail "run as an ordinary user, not root"
    for tool in git tmux curl tar sha256sum python3; do
        command -v "$tool" >/dev/null || fail "missing prerequisite: $tool"
    done
    command -v systemd-analyze >/dev/null || fail "missing prerequisite: systemd-analyze"
    git_identity user.name >/dev/null
    git_identity user.email >/dev/null
    fs_type="$(findmnt -n -o FSTYPE -T "$HOME")"
    case "$fs_type" in 9p|drvfs) fail "HOME is not on the Linux filesystem ($fs_type)" ;; esac
    [[ ! -e "$HOME/.config/systemd/user/cosmon-daemon-supervisor.service" ]] \
        || fail "supervisor unit already exists; use a clean test account"
    [[ ! -e "$HOME/.config/systemd/user/cosmon-scheduler.service" ]] \
        || fail "scheduler unit already exists; use a clean test account"
    [[ ! -e "$HOME/.config/cosmon/daemons.toml" ]] \
        || fail "daemon config already exists; use a clean test account"
    [[ ! -e "$HOME/.config/cosmon/patrols.toml" ]] \
        || fail "patrol config already exists; use a clean test account"
    {
        uname -a
        cat /etc/os-release
        systemd --version
        systemctl --user --version
        loginctl show-user "$USER" -p Linger -p State
        printf 'home_filesystem=%s\n' "$fs_type"
        git --version
        tmux -V
    } | tee "$run_root/platform.txt"
    grep -q '^Linger=yes$' "$run_root/platform.txt" || fail "linger must be enabled before validation"
    checkpoint preflight
    ;;

published-red)
    require_checkpoint preflight
    require_inside_host
    published_tmp="$(mktemp -d /tmp/cosmon-wsl2-published.XXXXXX)"
    trap 'find "$published_tmp" -depth -delete' EXIT
    curl -fsSL https://noogram.org/cosmon/install.sh -o "$published_tmp/install.sh"
    if COSMON_VERSION=v0.7.2 COSMON_INSTALL_DIR="$run_root/published/bin" \
        sh "$published_tmp/install.sh" --with-services >"$run_root/published-red.txt" 2>&1; then
        fail "published v0.7.2 unexpectedly accepted --with-services"
    fi
    grep -Eq 'service bundle|required by --with-services|unknown (argument|option)' \
        "$run_root/published-red.txt" \
        || fail "published failure did not identify the missing service install surface"
    [[ ! -e "$HOME/.config/systemd/user/cosmon-daemon-supervisor.service" ]] \
        || fail "published negative control changed supervisor units"
    [[ ! -e "$HOME/.config/systemd/user/cosmon-scheduler.service" ]] \
        || fail "published negative control changed scheduler units"
    checkpoint published-red
    ;;

candidate-install)
    require_checkpoint published-red
    require_inside_host
    [[ -n "$candidate_dir" ]] || fail "COSMON_CANDIDATE_DIR is required"
    for file in install.sh SHA256SUMS; do
        [[ -r "$candidate_dir/$file" ]] || fail "candidate is missing $file"
    done
    cp "$candidate_dir/SHA256SUMS" "$run_root/candidate-SHA256SUMS"
    [[ -r "$candidate_dir/REVISION" ]] && cp "$candidate_dir/REVISION" "$run_root/candidate-REVISION"
    COSMON_RELEASE_BASE_URL="file://$candidate_dir" \
        COSMON_INSTALL_DIR="$install_dir" \
        sh "$candidate_dir/install.sh" --with-services \
        | tee "$run_root/candidate-install.txt"
    for binary in cs cosmon-remote cosmon-daemon-supervisor cosmon-scheduler; do
        [[ -x "$install_dir/$binary" ]] || fail "candidate did not install $binary"
        "$install_dir/$binary" --version
    done
    systemctl --user is-active --quiet cosmon-daemon-supervisor.service
    systemctl --user is-active --quiet cosmon-scheduler.timer
    checkpoint candidate-install
    ;;

service-baseline)
    require_checkpoint candidate-install
    require_inside_host
    mkdir -p "$run_root/probes"
    cat >"$run_root/probes/supervised-child" <<EOF
#!/bin/sh
while :; do
    printf '%s\n' "\$\$" >"$run_root/probes/child.pid"
    date +%s >"$run_root/probes/heartbeat"
    sleep 2
done
EOF
    cat >"$run_root/probes/timer-probe" <<EOF
#!/bin/sh
date --iso-8601=ns >>"$run_root/probes/timer-firings"
EOF
    cat >"$run_root/probes/sleep-probe" <<EOF
#!/bin/sh
date --iso-8601=ns >"$run_root/probes/sleep-started"
sleep 120
date --iso-8601=ns >"$run_root/probes/sleep-finished"
EOF
    chmod +x "$run_root/probes/supervised-child" "$run_root/probes/timer-probe" \
        "$run_root/probes/sleep-probe"
    cat >"$HOME/.config/cosmon/daemons.toml" <<EOF
[supervisor]
state_file = "~/.cosmon/daemon-supervisor.state.json"
log_file = "~/.cosmon/daemon-supervisor.log"
kill_switch = "~/.cosmon/stand-down.lock"

[[daemon]]
name = "wsl2-heartbeat-witness"
binary = "$run_root/probes/supervised-child"
args = []
throttle_seconds = 1
enabled = true
EOF
    cat >"$HOME/.config/cosmon/patrols.toml" <<EOF
[scheduler]
state_file = "~/.cosmon/scheduler.state.json"
log_file = "~/.cosmon/scheduler.log"
kill_switch = "~/.cosmon/stand-down.lock"
tick_interval_seconds = 60

[[patrol]]
name = "wsl2-timer-witness"
interval_seconds = 1
command = ["$run_root/probes/timer-probe"]
dispatch = "wait"
enabled = true
EOF
    COSMON_SUPERVISOR_BIN_DIR="$install_dir" \
        "$HOME/.local/libexec/cosmon/install-daemon-supervisor.sh" reload
    COSMON_SCHEDULER_BIN_DIR="$install_dir" \
        "$HOME/.local/libexec/cosmon/install-scheduler.sh" reload
    bounded_wait 20 "one supervised child with a fresh heartbeat" services_ready
    state_is_readable "$HOME/.cosmon/daemon-supervisor.state.json" \
        || fail "supervisor state is not readable JSON"
    record_service_snapshot | tee "$run_root/service-baseline.txt"
    checkpoint service-baseline
    ;;

lifecycle)
    require_checkpoint service-baseline
    require_inside_host
    adapter="${COSMON_WSL2_ADAPTER:-}"
    model="${COSMON_WSL2_MODEL:-}"
    [[ -n "$adapter" ]] || fail "COSMON_WSL2_ADAPTER is required"
    [[ -n "$model" ]] || fail "COSMON_WSL2_MODEL is required"
    export GIT_AUTHOR_NAME="$(git_identity user.name)"
    export GIT_AUTHOR_EMAIL="$(git_identity user.email)"
    export GIT_COMMITTER_NAME="$GIT_AUTHOR_NAME"
    export GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
    repo="$run_root/lifecycle-repository"
    [[ ! -e "$repo" ]] || fail "lifecycle repository already exists; choose a new run id"
    mkdir -p "$repo"
    git -C "$repo" init -b main
    printf '# Candidate lifecycle witness\n' >"$repo/README.md"
    git -C "$repo" add README.md
    git -C "$repo" commit -s -m 'test: initialize candidate lifecycle witness'
    (
        cd "$repo"
        PATH="$install_dir:$HOME/.local/bin:$PATH" cs init .
        PATH="$install_dir:$HOME/.local/bin:$PATH" cs nucleate task-work --json \
            --var 'topic=Create wsl2-candidate-witness.txt containing exactly candidate-witness followed by a newline, commit the change with sign-off, then complete the molecule. Do not harvest or run done.' \
            >"$run_root/lifecycle-nucleate.json"
    )
    molecule="$(grep -Eo 'task-[0-9]{8}-[[:alnum:]]{4}' "$run_root/lifecycle-nucleate.json" | head -1)"
    [[ -n "$molecule" ]] || fail "could not read molecule id from nucleate output"
    printf '%s\n' "$molecule" >"$run_root/lifecycle-molecule"
    (
        cd "$repo"
        PATH="$install_dir:$HOME/.local/bin:$PATH" cs tackle "$molecule" \
            --adapter "$adapter" --model "$model" --json \
            >"$run_root/lifecycle-tackle.json"
    )
    session="$(python3 -c '
import json, sys
print(json.loads(sys.stdin.readline())["tmux_session"])
' <"$run_root/lifecycle-tackle.json")"
    socket="$(python3 -c '
import json, shlex, sys
args = shlex.split(json.loads(sys.stdin.readline())["attach"])
print(args[args.index("-L") + 1])
' <"$run_root/lifecycle-tackle.json")"
    lifecycle_completed() {
        status="$(
            cd "$repo"
            PATH="$install_dir:$HOME/.local/bin:$PATH" cs observe "$molecule" --json \
                | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", ""))'
        )"
        case "$status" in
            completed) return 0 ;;
            collapsed|failed) fail "lifecycle worker entered terminal status: $status" ;;
            *) return 1 ;;
        esac
    }
    bounded_wait 900 "lifecycle worker completion" lifecycle_completed
    (
        cd "$repo"
        PATH="$install_dir:$HOME/.local/bin:$PATH" cs observe "$molecule" --json \
            >"$run_root/lifecycle-observe-completed.json"
    )
    worker_head="$(git -C "$repo" rev-parse "feat/$molecule")"
    worktree="$(git -C "$repo" worktree list --porcelain | awk -v branch="refs/heads/feat/$molecule" '
        $1 == "worktree" { path=$2 }
        $1 == "branch" && $2 == branch { print path }
    ')"
    [[ -n "$worktree" && -f "$worktree/wsl2-candidate-witness.txt" ]] \
        || fail "worker artifact was not recorded in its worktree"
    [[ "$(cat "$worktree/wsl2-candidate-witness.txt")" == candidate-witness ]] \
        || fail "worker artifact content is wrong"
    (
        cd "$repo"
        PATH="$install_dir:$HOME/.local/bin:$PATH" cs "done" "$molecule" \
            | tee "$run_root/lifecycle-done.txt"
    )
    git -C "$repo" merge-base --is-ancestor "$worker_head" main \
        || fail "worker commit is not an ancestor of main after done"
    [[ "$(cat "$repo/wsl2-candidate-witness.txt")" == candidate-witness ]] \
        || fail "merged artifact content is wrong"
    [[ ! -e "$worktree" ]] || fail "worker worktree remains after done"
    git -C "$repo" worktree list --porcelain | grep -qF "refs/heads/feat/$molecule" \
        && fail "worker branch still has a registered worktree"
    git -C "$repo" show-ref --verify --quiet "refs/heads/feat/$molecule" \
        && fail "worker branch remains after done"
    tmux -L "$socket" has-session -t "=$session" 2>/dev/null \
        && fail "worker session remains after done"
    printf 'molecule=%s\nworker_head=%s\nmain_head=%s\n' \
        "$molecule" "$worker_head" "$(git -C "$repo" rev-parse main)" \
        >"$run_root/lifecycle-result.txt"
    checkpoint lifecycle
    ;;

supervisor-crash)
    require_checkpoint service-baseline
    require_inside_host
    old_main="$(unit_value cosmon-daemon-supervisor.service MainPID)"
    old_child="$(cat "$run_root/probes/child.pid")"
    systemctl --user kill --kill-who=main --signal=KILL cosmon-daemon-supervisor.service
    replaced() {
        [[ "$(unit_value cosmon-daemon-supervisor.service MainPID)" != "$old_main" ]] \
            && [[ "$(cat "$run_root/probes/child.pid" 2>/dev/null || true)" != "$old_child" ]] \
            && services_ready
    }
    bounded_wait 25 "supervisor and child replacement" replaced
    kill -0 "$old_child" 2>/dev/null && fail "old child survived supervisor crash"
    record_service_snapshot | tee "$run_root/supervisor-crash.txt"
    checkpoint supervisor-crash
    ;;

child-crash)
    require_checkpoint supervisor-crash
    require_inside_host
    old_child="$(cat "$run_root/probes/child.pid")"
    kill -KILL "$old_child"
    child_replaced() {
        [[ "$(cat "$run_root/probes/child.pid" 2>/dev/null || true)" != "$old_child" ]] \
            && services_ready
    }
    bounded_wait 15 "supervised child replacement" child_replaced
    record_service_snapshot | tee "$run_root/child-crash.txt"
    checkpoint child-crash
    ;;

timer)
    require_checkpoint service-baseline
    require_inside_host
    before="$(timer_firing_count)"
    timer_fired() {
        [[ "$(timer_firing_count)" -gt "$before" ]]
    }
    bounded_wait 90 "the next scheduler timer firing" timer_fired
    state_is_readable "$HOME/.cosmon/scheduler.state.json" \
        || fail "scheduler state is not readable JSON"
    record_service_snapshot | tee "$run_root/timer.txt"
    checkpoint timer
    ;;

before-logout|before-distribution|before-reboot)
    require_checkpoint service-baseline
    require_inside_host
    boundary="${phase#before-}"
    services_ready || fail "services are not ready before $boundary"
    record_distribution_boot "$phase"
    if [[ "$phase" == before-reboot ]]; then
        windows_boot_epoch >"$checkpoint_dir/before-reboot.windows-boot-epoch"
    fi
    checkpoint "$phase"
    case "$phase" in
        before-logout)
            echo "External driver: close every distribution shell, wait 30 seconds, then reconnect and run phase after-logout."
            ;;
        before-distribution)
            echo "External driver: shut down the distribution, relaunch it explicitly, then run phase after-distribution."
            ;;
        before-reboot)
            echo "External driver: reboot the host, launch the distribution explicitly, then run phase after-reboot."
            ;;
    esac
    ;;

after-logout|after-distribution|after-reboot)
    previous="${phase#after-}"
    require_checkpoint "before-$previous"
    require_checkpoint service-baseline
    require_inside_host
    record_distribution_boot "$phase"
    if [[ "$phase" == after-reboot ]]; then
        before_host_boot="$(cat "$checkpoint_dir/before-reboot.windows-boot-epoch")"
        after_host_boot="$(windows_boot_epoch)"
        printf '%s\n' "$after_host_boot" >"$checkpoint_dir/after-reboot.windows-boot-epoch"
        (( after_host_boot > before_host_boot )) \
            || fail "host did not reboot: Windows boot time did not advance"
    fi
    bounded_wait 25 "services after $previous" services_ready
    state_is_readable "$HOME/.cosmon/daemon-supervisor.state.json" \
        || fail "supervisor state is unreadable after $previous"
    record_service_snapshot | tee "$run_root/$phase.txt"
    report_distribution_transition "before-$previous" "$phase" \
        | tee -a "$run_root/$phase.txt"
    checkpoint "$phase"
    ;;

before-sleep)
    require_checkpoint service-baseline
    require_inside_host
    rm -f "$run_root/probes/sleep-started" "$run_root/probes/sleep-finished"
    cat >>"$HOME/.config/cosmon/patrols.toml" <<EOF

[[patrol]]
name = "wsl2-sleep-witness"
interval_seconds = 1
command = ["$run_root/probes/sleep-probe"]
dispatch = "detached"
enabled = true
EOF
    COSMON_SCHEDULER_BIN_DIR="$install_dir" \
        "$HOME/.local/libexec/cosmon/install-scheduler.sh" reload
    systemctl --user start cosmon-scheduler.service
    bounded_wait 10 "detached sleep probe start" test -s "$run_root/probes/sleep-started"
    record_distribution_boot before-sleep
    stat -c %Y "$run_root/probes/sleep-started" \
        >"$checkpoint_dir/before-sleep.probe-start-epoch"
    checkpoint before-sleep
    echo "External driver: keep a WSL client attached, sleep the host now, resume it, then run phase after-sleep."
    ;;

after-sleep)
    require_checkpoint before-sleep
    require_inside_host
    record_distribution_boot after-sleep
    before_distribution_boot="$(cat "$checkpoint_dir/before-sleep.distribution-boot")"
    after_distribution_boot="$(cat "$checkpoint_dir/after-sleep.distribution-boot")"
    [[ "$before_distribution_boot" == "$after_distribution_boot" ]] \
        || fail "distribution stopped before or during sleep"
    probe_start_epoch="$(cat "$checkpoint_dir/before-sleep.probe-start-epoch")"
    now_epoch="$(date +%s)"
    remaining=$((probe_start_epoch + 150 - now_epoch))
    (( remaining > 0 )) \
        || fail "in-flight patrol probe completion budget expired before after-sleep"
    bounded_wait "$remaining" "in-flight patrol completion after sleep" \
        test -s "$run_root/probes/sleep-finished"
    bounded_wait 25 "services after sleep" services_ready
    state_is_readable "$HOME/.cosmon/scheduler.state.json" \
        || fail "scheduler state is unreadable after sleep"
    record_service_snapshot | tee "$run_root/after-sleep.txt"
    report_distribution_transition before-sleep after-sleep \
        | tee -a "$run_root/after-sleep.txt"
    checkpoint after-sleep
    ;;

final)
    require_checkpoint lifecycle
    require_checkpoint supervisor-crash
    require_checkpoint child-crash
    require_checkpoint timer
    missing=()
    for required in after-logout after-distribution after-reboot after-sleep; do
        [[ -s "$checkpoint_dir/$required" ]] || missing+=("$required")
    done
    ((${#missing[@]} == 0)) \
        || fail "missing required external checkpoints: ${missing[*]}"
    require_inside_host
    services_ready || fail "services are not running at final handoff"
    record_service_snapshot | tee "$run_root/final.txt"
    find "$checkpoint_dir" -maxdepth 1 -type f -print | sort
    checkpoint final
    ;;

-h|--help|help)
    usage
    ;;

*)
    usage
    fail "unknown phase: $phase"
    ;;
esac
