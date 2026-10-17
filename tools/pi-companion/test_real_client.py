#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Opt-in real Pi TUI + public CLI + witnessed dummy worker acceptance.

Invoked by owned_pi_client_acceptance. Requires COSMON_PI_EXEC pointing to a
pinned test-only installation; writes private receipts outside the repository.
No provider credentials, model calls, user panes or global configuration.
"""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import select
import signal
import struct
import subprocess
import tempfile
import termios
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--cs', required=True)
    parser.add_argument('--tmux-socket', required=True)
    args = parser.parse_args()
    args.repo = args.repo.resolve()
    pi = os.environ['COSMON_PI_EXEC']
    receipts = Path(os.environ.get('COSMON_CLIENT_RECEIPTS') or tempfile.mkdtemp(prefix='pi-receipts-'))
    receipts.mkdir(parents=True, exist_ok=True)
    receipt = {'checks': [], 'commands': [], 'ui': 'real Pi interactive TUI in owned PTY; rendered text captured, no visual GUI claim', 'model_calls': 'no model/provider configured; slash commands only'}
    id_ = 'task-20990101-a001'
    state = args.repo / '.cosmon/state'
    process = None
    master = None
    transcript = bytearray()
    session_no = 0

    def run(argv, check=True):
        result = subprocess.run(list(map(str, argv)), cwd=args.repo, capture_output=True, text=True, timeout=25)
        receipt['commands'].append({'argv': list(map(str, argv)), 'exit': result.returncode, 'stdout': result.stdout, 'stderr': result.stderr})
        if check:
            assert result.returncode == 0, result.stderr + result.stdout
        return result.stdout

    def check(name, value):
        assert value, name
        receipt['checks'].append(name)

    def worker(*argv):
        return run(['tmux', '-L', args.tmux_socket, *argv])

    def observation():
        return json.loads(run([args.cs, '--config', state, '--json', 'terminal', id_]))

    def process_rows():
        text = subprocess.run(['ps', '-axo', 'pid=,ppid='], capture_output=True, text=True, timeout=5).stdout
        return {int(fields[0]): (int(fields[1]), '') for line in text.splitlines() if len(fields := line.split()) == 2}

    def owned_reader_pids():
        rows = process_rows()
        descendants = {process.pid}
        while True:
            added = {pid for pid, (parent, _) in rows.items() if parent in descendants}
            if added <= descendants:
                break
            descendants |= added
        ids = descendants - {process.pid}
        text = subprocess.run(['ps', '-p', ','.join(map(str, ids)), '-o', 'pid=,comm='], capture_output=True, text=True, timeout=5).stdout if ids else ''
        readers = {int(fields[0]): fields[1] for line in text.splitlines() if len(fields := line.strip().split(None, 1)) == 2}
        receipt.setdefault('owned_readers', []).append(readers)
        return set(readers)

    def hashes():
        return {str(p.relative_to(state)): hashlib.sha256(p.read_bytes()).hexdigest() for p in state.rglob('*') if p.is_file()}

    def drain(seconds=.1):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if master is not None and select.select([master], [], [], .05)[0]:
                try:
                    data = os.read(master, 65536)
                except OSError:
                    break
                if not data:
                    break
                transcript.extend(data)

    def plain(data):
        value = data.decode(errors='replace')
        value = re.sub(r'\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)', '', value)
        return re.sub(r'\x1b\[[0-?]*[ -/]*[@-~]', '', value)

    def wait(name, predicate, seconds=12):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            drain(.1)
            if predicate():
                return
        raise AssertionError('bounded acceptance wait expired: ' + name)

    def send(text):
        assert process and process.poll() is None, 'Pi exited before command'
        os.write(master, text.encode())

    def command(text, expected):
        start = len(transcript)
        send(text + '\r')
        wait(text, lambda: expected in plain(transcript[start:]))
        return start

    def close_screen():
        send('q')
        drain(.4)

    def start():
        nonlocal process, master, transcript, session_no
        session_no += 1
        transcript = bytearray()
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))
        # Deliberately inherit no provider credentials; HOME is unchanged.
        env = {k: os.environ[k] for k in ('PATH', 'HOME', 'LANG', 'SHELL', 'TMPDIR') if k in os.environ}
        env.update(TERM='xterm-256color', PI_CODING_AGENT_DIR=str(receipts / ('agent-' + str(session_no))), PI_OFFLINE='1', PI_SKIP_VERSION_CHECK='1', PI_TELEMETRY='0')
        argv = [pi, '--no-session', '--no-tools', '--no-extensions', '--no-mcp', '--no-skills', '--no-prompt-templates', '--no-themes', '--no-context-files', '--no-approve', '--offline', '-e', str(Path(os.environ.get('COSMON_PI_PACKAGE') or Path(__file__).resolve().parent))]
        process = subprocess.Popen(argv, cwd=args.repo, env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        receipt.setdefault('pi_processes', []).append({'pid': process.pid, 'argv': argv})
        wait('Pi startup', lambda: 'No models available' in plain(transcript))

    def stop():
        nonlocal process, master
        if process:
            if process.poll() is None:
                process.send_signal(signal.SIGTERM)
                try:
                    wait('Pi graceful shutdown', lambda: process.poll() is not None, 6)
                except BaseException:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
                    receipt.setdefault('forced_cleanup', []).append(process.pid)
            (receipts / ('pi-tui-' + str(session_no) + '.raw')).write_bytes(transcript)
            (receipts / ('pi-tui-' + str(session_no) + '.txt')).write_text(plain(transcript))
            process = None
        if master is not None:
            os.close(master)
            master = None

    try:
        receipt['pi_version'] = run([pi, '--version']).strip()
        check('exact pinned Pi runtime version', receipt['pi_version'] == '1.1.0')
        initial = observation()
        check('real dummy public v1 available witness', initial['availability'] == 'available' and initial['schema_version'] == 1)
        pane = '%' + str(initial['identity']['pane_id'])
        size = worker('display-message', '-p', '-t', pane, '#{pane_width}x#{pane_height}')
        environment = worker('show-environment', '-t', '=fixture')
        before = hashes()
        # Test-only CLI seam returns a real public observation, then forces the
        # race before the extension executes its attachment. No production hook.
        shim = args.repo / 'test-cs.py'
        race = args.repo / 'race-once'
        argv_log = receipts / 'extension-argv.jsonl'
        shim.write_text('#!/usr/bin/env python3\nimport json,subprocess,sys,os\nfrom pathlib import Path\n' +
            'with open(' + repr(str(argv_log)) + ',"a") as f: f.write(json.dumps(sys.argv[1:])+"\\n")\n' +
            'if "--attach" in sys.argv: os.execv(' + repr(args.cs) + ',[' + repr(args.cs) + ']+sys.argv[1:])\n' +
            'p=subprocess.run([' + repr(args.cs) + ']+sys.argv[1:],capture_output=True)\n' +
            'race=Path(' + repr(str(race)) + ')\n' +
            'if "--json" in sys.argv and "terminal" in sys.argv and race.exists():\n race.unlink()\n subprocess.run(["tmux","-L",' + repr(args.tmux_socket) + ',"respawn-pane","-k","-t",' + repr(pane) + ',"exec python3 worker.py"],check=True)\n' +
            'sys.stdout.buffer.write(p.stdout);sys.stderr.buffer.write(p.stderr);sys.exit(p.returncode)\n')
        shim.chmod(0o700)
        selection = '/cosmon-project ' + json.dumps({'root': str(args.repo), 'projectId': 'example-project', 'cs': str(shim)})
        start()
        check('no subprocess at extension startup', not argv_log.exists())
        command(selection, 'Cosmon project selected')
        check('actual Pi extension load and native command execution', 'Cosmon project selected' in plain(transcript))
        command('/cosmon-list', 'Cosmon molecules: example-project')
        start_at = len(transcript)
        send('\r')
        wait('native molecule selection', lambda: 'Cosmon molecule ' + id_ in plain(transcript[start_at:]))
        check('real Pi native list/select/details', 'Status: running' in plain(transcript[start_at:]))
        close_screen()
        command('/cosmon-worktree', str(args.repo))
        check('worktree navigation displays verified existing path', 'Navigate using your terminal/editor' in plain(transcript))
        close_screen()
        command('/cosmon-artifacts', 'Cosmon artifacts ' + id_)
        start_at = len(transcript)
        send('\r')
        wait('artifact reader', lambda: 'owned artifact witness' in plain(transcript[start_at:]))
        check('real Pi artifact reader', 'Cosmon artifact (read-only)' in plain(transcript[start_at:]))
        close_screen()
        command('/cosmon-output', 'WORKER-READONLY-WITNESS')
        clients = worker('list-clients', '-F', '#{client_flags}\t#{client_session}')
        check('actual Pi view has readonly ignore-size control client', bool(clients.strip()) and all(all(flag in line for flag in ('read-only', 'ignore-size', 'control-mode')) and line.endswith('fixture') for line in clients.splitlines()))
        send('INPUT-MUST-NOT-REACH-WORKER\r')
        drain(.3)
        check('arbitrary Pi view input never reaches worker', not (args.repo / 'worker-input').exists())
        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 22, 75, 0, 0))
        process.send_signal(signal.SIGWINCH)
        drain(.4)
        check('Pi resize preserves worker dimensions', worker('display-message', '-p', '-t', pane, '#{pane_width}x#{pane_height}') == size)
        check('Pi view preserves worker environment', worker('show-environment', '-t', '=fixture') == environment)
        readers = owned_reader_pids()
        check('output bridge is an owned Pi descendant', bool(readers))
        close_screen()
        wait('view close reaps bridge', lambda: not readers.intersection(process_rows()))
        check('view close reaps all owned reader descendants', not readers.intersection(process_rows()))
        wait('view close detaches client', lambda: not worker('list-clients', '-F', '#{client_flags}').strip())
        check('view close preserves worker incarnation', observation()['expected'] == initial['expected'])
        command('/cosmon-output', 'WORKER-READONLY-WITNESS')
        readers = owned_reader_pids()
        stop()
        wait('shutdown reaps bridge', lambda: not readers.intersection(process_rows()))
        check('Pi shutdown reaps all owned reader descendants', bool(readers) and not readers.intersection(process_rows()))
        wait('shutdown detaches owned client', lambda: not worker('list-clients', '-F', '#{client_flags}').strip())
        check('Pi shutdown preserves worker incarnation', observation()['expected'] == initial['expected'])
        check('Pi shutdown completed gracefully', not receipt.get('forced_cleanup'))
        start()
        command(selection, 'Cosmon project selected')
        race.write_text('armed')
        command('/cosmon-output ' + id_, 'view ended: 1')
        check('stale incarnation refused in actual Pi output path', '"reason":"identity_changed"' in plain(transcript) and not worker('list-clients', '-F', '#{client_flags}').strip())
        close_screen()
        command('/cosmon-output ' + id_, 'Terminal unavailable')
        check('new probe preserves unavailable after replacement', observation()['availability'] == 'unavailable')
        stop()
        calls = [json.loads(line) for line in argv_log.read_text().splitlines()]
        check('only public read commands and guarded attachment invoked', all(call[0:2] == ['--config', str(state)] and (call[2] == '--json' and call[3] in ('ensemble', 'observe', 'terminal') or call[2] == 'terminal' and call[4:6] == ['--attach', '--expect']) for call in calls))
        check('opaque token supplied unchanged by production extension', any(call[-1] == initial['expected'] and '--attach' in call for call in calls))
        after = hashes()
        diffs = {name: {'before': before.get(name), 'after': after.get(name)} for name in before.keys() | after.keys() if before.get(name) != after.get(name)}
        receipt['state_diffs'] = diffs
        check('lifecycle process and artifact bytes preserved', all(name == 'instrumentation/authz.jsonl' for name in diffs))
        receipt.update(result='passed', initial_terminal=initial, state_hashes=before, worker_size=size, extension_calls=calls)
    except BaseException as error:
        receipt.update(result='failed', error=repr(error))
        raise
    finally:
        stop()
        (receipts / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps({'checks': len(receipt['checks']), 'receipt': str(receipts / 'receipt.json')}))


if __name__ == '__main__':
    main()
