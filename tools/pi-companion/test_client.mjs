// SPDX-License-Identifier: AGPL-3.0-only
/** Executable contract examples, no Pi dependency or credentials. */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, symlink, rm, readFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { Client, Processes, validateTerminal, exactId, safeText } from './client.mjs';
import { register } from './extension.mjs';

const settings = {root: '/example/galaxy', state: '/example/galaxy/.cosmon/state', projectId: 'example-project', fleet: 'default', cs: 'cs'};
const id = 'task-20990101-a001';
const available = () => ({schema_version: 1, kind: 'cosmon.terminal', id, project_root: settings.root, state_dir: settings.state, project_id: settings.projectId, fleet_id: settings.fleet, availability: 'available', capability: 'read_only_ignore_size', identity: {backend: 'future'}, expected: 'opaque token; $(never evaluated)'});

test('v1 accepts additive fields and forwards opaque token byte-for-byte in argv', () => {
  const observation = {...available(), future: true};
  const result = validateTerminal(observation, settings, id);
  assert.equal(result.expected, observation.expected);
  const argv = new Client(settings).attachment(id, result.expected);
  assert.deepEqual(argv, ['cs', '--config', settings.state, 'terminal', id, '--attach', '--expect', observation.expected]);
});

test('every identity, version, kind and capability mismatch is refused', () => {
  for (const [key, value] of Object.entries({schema_version: 2, kind: 'another', id: id + 'x', project_root: '/wrong', state_dir: '/wrong', project_id: 'other', fleet_id: 'other', availability: 'alive', capability: 'interactive', identity: [], expected: ''})) {
    assert.throws(() => validateTerminal({...available(), [key]: value}, settings, id), key);
  }
  for (const version of [true, '1', null, undefined]) assert.throws(() => validateTerminal({...available(), schema_version: version}, settings, id));
});

test('unknown and unavailable remain distinct; capability cannot accompany either', () => {
  for (const availability of ['unknown', 'unavailable']) {
    const value = {...available(), availability, reason: 'explicit_reason'};
    delete value.identity; delete value.expected; delete value.capability;
    assert.deepEqual(validateTerminal(value, settings, id), {availability, reason: 'explicit_reason'});
    for (const key of ['identity', 'expected', 'capability']) assert.throws(() => validateTerminal({...value, [key]: null}, settings, id));
  }
});

test('malformed/failed CLI terminal probe is unknown; unsupported JSON is refused', async () => {
  const c = new Client(settings);
  c.json = async () => { throw Error('failed'); };
  assert.equal((await c.terminal(id)).availability, 'unknown');
  c.json = async () => ({schema_version: 2});
  await assert.rejects(c.terminal(id), /Unsupported/);
});

test('exact detail equality rejects prefix resolution and unavailable is not completion', async () => {
  const c = new Client(settings);
  const observed = {...available(), availability: 'unavailable', reason: 'no_process'};
  delete observed.identity; delete observed.expected; delete observed.capability;
  c.json = async command => command === 'terminal' ? observed : {id: id + 'x', status: 'running'};
  await assert.rejects(c.detail(id), /exact/);
  c.json = async command => command === 'terminal' ? observed : {id, status: 'running'};
  assert.equal((await c.detail(id)).value.status, 'running');
  assert.equal((await c.detail(id)).terminal.availability, 'unavailable');
});

test('list requires v1 and explicit fleet, preserves statuses; history is opt-in', async () => {
  const c = new Client(settings);
  c.json = async () => ({schema_version: 1, molecule_states: [{id, status: 'running', fleet: 'default'}, {id: id + 'b', status: 'completed', fleet: 'default'}, {id: id + 'c', status: 'running', fleet: 'other'}]});
  assert.deepEqual(await c.list(), [{id, status: 'running'}]);
  assert.equal((await c.list(true)).length, 2);
  c.json = async () => ({schema_version: true, molecule_states: []});
  await assert.rejects(c.list(), /ensemble/);
});

test('ids cannot become argv options, empty values or traversal paths', () => {
  for (const value of [undefined, '', '-option', '/absolute', 'with space', '../traversal', '..', 'x\0y', 'x'.repeat(129)]) assert.throws(() => exactId(value));
  assert.equal(exactId(id), id);
});

test('canonical Git checkout and explicit state survive invocation from a linked worktree', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'pi-canonical-'));
  const p = new Processes();
  try {
    await mkdir(path.join(root, '.cosmon/state'), {recursive: true});
    await writeFile(path.join(root, '.cosmon/config.toml'), '[project]\nproject_id="example-project"\n');
    await p.run(['git', 'init', '-q', root], root);
    await p.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--allow-empty', '-qm', 'fixture'], root);
    await p.run(['git', 'worktree', 'add', '-qb', 'linked', path.join(root, 'linked')], root);
    const c = await Client.select({root: path.join(root, 'linked'), projectId: 'example-project'}, p);
    assert.equal(c.settings.root, await (await import('node:fs/promises')).realpath(root));
    assert.deepEqual(c.argv('ensemble'), ['cs', '--config', path.join(c.settings.root, '.cosmon/state'), '--json', 'ensemble']);
    assert.equal(await c.worktree({branch: 'linked'}), path.join(c.settings.root, 'linked'));
    await assert.rejects(c.worktree({branch: 'absent'}), /unique/);
  } finally { p.close(); await rm(root, {recursive: true, force: true}); }
});

test('artifact reads allow conventional bounded regular files; reject symlinks and store files', async () => {
  const root = await mkdtemp(path.join(os.tmpdir(), 'pi-artifact-'));
  try {
    const state = path.join(root, 'state'), residence = path.join(state, 'fleets/default/molecules', id);
    await mkdir(residence, {recursive: true});
    await writeFile(path.join(residence, 'synthesis.md'), 'artifact witness');
    await writeFile(path.join(root, 'outside.md'), 'private');
    await symlink(path.join(root, 'outside.md'), path.join(residence, 'result.md'));
    const c = new Client({...settings, state: await (await import('node:fs/promises')).realpath(state)});
    const detail = {id, molecule_dir: await (await import('node:fs/promises')).realpath(residence)};
    assert.deepEqual((await c.artifacts(detail)).files, ['synthesis.md']);
    assert.equal(await c.readArtifact(detail, 'synthesis.md'), 'artifact witness');
    for (const name of ['state.json', '../outside.md', 'result.md']) await assert.rejects(c.readArtifact(detail, name), /outside/);
    await writeFile(path.join(residence, 'synthesis.md'), 'x'.repeat(1024 * 1024 + 1));
    await assert.rejects(c.readArtifact(detail, 'synthesis.md'), /bounded/);
    await assert.rejects(c.artifacts({...detail, molecule_dir: root}), /residence/);
  } finally { await rm(root, {recursive: true, force: true}); }
});

test('subprocess argv is literal; time/output bounds and shutdown reap owned children', async () => {
  const p = new Processes();
  try {
    assert.equal(await p.run([process.execPath, '-e', 'process.stdout.write(process.argv[1])', ';$(literal)'], os.tmpdir()), ';$(literal)');
    await assert.rejects(p.run([process.execPath, '-e', 'setInterval(()=>{},1000)'], os.tmpdir(), 80), /timed out/);
    await assert.rejects(p.run([process.execPath, '-e', 'process.stdout.write("x".repeat(5*1024*1024))'], os.tmpdir()), /bound/);
    const child = p.spawn([process.execPath, '-e', 'setInterval(()=>{},1000)'], os.tmpdir());
    const stopped = new Promise(resolve => child.on('close', resolve));
    p.close(); await stopped;
    assert.equal(p.children.size, 0);
    assert.throws(() => p.spawn(['false'], os.tmpdir()), /closed/);
  } finally { p.close(); }
});

test('native registration is inert, exposes only commands and refuses headless commands', async () => {
  const commands = new Map(), events = new Map(), notices = [];
  const pi = {registerCommand(name, command) {commands.set(name, command);}, on(name, fn) {events.set(name, fn);}};
  register(pi, {});
  assert.equal(commands.size, 8);
  assert.deepEqual([...events.keys()].sort(), ['session_shutdown', 'session_start']);
  await commands.get('cosmon-output').handler('', {mode: 'rpc', ui: {notify: (...args) => notices.push(args)}});
  assert.match(notices[0][0], /interactive/);
  events.get('session_shutdown')(); events.get('session_shutdown')();
  // Source guards protect side-effect boundaries that mock registration cannot witness.
  const source = await readFile(new URL('./extension.mjs', import.meta.url), 'utf8');
  for (const forbidden of ['sendUserMessage', 'sendMessage(', 'registerTool', 'writeFile', 'setEditorText', 'setActiveTools']) assert.ok(!source.includes(forbidden), forbidden);
});

test('terminal controls in labels/artifacts cannot escape into Pi renderer', () => {
  assert.equal(safeText('\x1b]52;c;secret\x07\x1b[31mhello\x1b[0m\x00\r\n'), 'hello\n');
});

test('real PTY bridge preserves refusal diagnostics without traceback or worker input', async () => {
  const p = new Processes();
  const child = p.spawn(['python3', new URL('./pty_bridge.py', import.meta.url).pathname, 'python3', '-c', 'import sys; print("terminal unavailable identity_changed",flush=True);sys.exit(1)'], os.tmpdir(), {stdio: ['pipe', 'pipe', 'pipe']});
  let output = '', errors = '';
  child.stdout.on('data', data => {output += data;});
  child.stderr.on('data', data => {errors += data;});
  const timer = setTimeout(() => p.stop(child), 4000);
  try {
    const code = await new Promise(resolve => child.once('close', resolve));
    assert.equal(code, 1);
    assert.equal(errors, '');
    assert.match(JSON.parse(output.trim()).data, /identity_changed/);
  } finally { clearTimeout(timer); p.close(); }
});
