// SPDX-License-Identifier: AGPL-3.0-only
/** Independent public CLI reader; never imports another Cosmon client. */
import { spawn } from 'node:child_process';
import { constants } from 'node:fs';
import { realpath, stat, lstat, readdir, open } from 'node:fs/promises';
import path from 'node:path';

export const deliverables = ['synthesis.md', 'frame.md', 'outcomes.md', 'plan.md', 'report.md', 'result.md'];
export function exactId(id) {
  if (typeof id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9_.:@+-]{0,127}$/.test(id) || id.startsWith('-') || id.startsWith('/')) throw Error('Complete molecule id required');
  return id;
}
/** Bound time, output and descendants; shutdown cancels only owned children. */
export class Processes {
  children = new Set();
  closed = false;
  spawn(argv, cwd, options = {}) {
    if (this.closed) throw Error('Client session is closed');
    const child = spawn(argv[0], argv.slice(1), {cwd, detached: true, stdio: ['ignore', 'pipe', 'pipe'], ...options});
    this.children.add(child);
    child.once('close', () => this.children.delete(child));
    return child;
  }
  stop(child) {
    if (!child.pid || !this.children.has(child)) return;
    try { process.kill(-child.pid, 'SIGTERM'); } catch { /* already exited */ }
    const timer = setTimeout(() => {
      if (this.children.has(child)) { try { process.kill(-child.pid, 'SIGKILL'); } catch { /* already exited */ } }
    }, 700);
    timer.unref();
  }
  close() { this.closed = true; for (const child of this.children) this.stop(child); }
  async run(argv, cwd, timeout = 20000) {
    return new Promise((resolve, reject) => {
      let output = '', error = '', size = 0, refusal;
      const child = this.spawn(argv, cwd);
      const timer = setTimeout(() => { refusal = Error('Command timed out'); this.stop(child); }, timeout);
      const collect = (data, stderr) => {
        size += Buffer.byteLength(data);
        if (size > 4 * 1024 * 1024) { refusal = Error('Command output exceeds bound'); this.stop(child); return; }
        if (stderr) error += data.toString(); else output += data.toString();
      };
      child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
      child.stdout.on('data', data => collect(data, false));
      child.stderr.on('data', data => collect(data, true));
      child.once('error', e => { clearTimeout(timer); reject(Error('Command unavailable: ' + e.code)); });
      child.once('close', code => {
        clearTimeout(timer);
        if (refusal || code !== 0) reject(refusal || Error('Command refused (exit ' + code + ')'));
        else resolve(output);
      });
    });
  }
}

/** Preserve availability, refuse unsupported stamps and exact context mismatches. */
export function validateTerminal(value, settings, id) {
  if (!value || value.schema_version !== 1 || value.kind !== 'cosmon.terminal') throw Error('Unsupported terminal contract');
  const expected = {id, project_root: settings.root, state_dir: settings.state, project_id: settings.projectId, fleet_id: settings.fleet};
  for (const [key, item] of Object.entries(expected)) if (value[key] !== item) throw Error('Terminal context mismatch: ' + key);
  if (!['available', 'unknown', 'unavailable'].includes(value.availability)) throw Error('Invalid terminal availability');
  if (value.availability === 'available') {
    if (value.capability !== 'read_only_ignore_size' || !value.identity || typeof value.identity !== 'object' || Array.isArray(value.identity) || typeof value.expected !== 'string' || !value.expected || value.expected.length > 4096 || value.expected.includes('\0')) throw Error('Unsupported terminal capability');
    return {availability: 'available', expected: value.expected};
  }
  if (typeof value.reason !== 'string' || ['identity', 'expected', 'capability'].some(key => key in value)) throw Error('Invalid non-available observation');
  return {availability: value.availability, reason: value.reason};
}

export class Client {
  constructor(settings, processes = new Processes()) { this.settings = settings; this.processes = processes; }
  static async select(input, processes) {
    if (!input || typeof input.root !== 'string' || !path.isAbsolute(input.root) || typeof input.projectId !== 'string' || !input.projectId || !/^[A-Za-z0-9_-]+$/.test(input.fleet ?? 'default') || typeof (input.cs ?? 'cs') !== 'string' || !(input.cs ?? 'cs') || (input.cs ?? '').startsWith('-')) throw Error('Explicit absolute root, projectId, simple fleet and executable required');
    const candidate = await realpath(input.root);
    const registry = await processes.run(['git', 'worktree', 'list', '--porcelain', '-z'], candidate);
    const first = registry.split('\0\0')[0].split('\0').find(x => x.startsWith('worktree '));
    if (!first) throw Error('No canonical Git worktree');
    const root = await realpath(first.slice(9));
    if (!(await stat(path.join(root, '.cosmon/config.toml'))).isFile()) throw Error('No Cosmon configuration');
    const state = await realpath(path.join(root, '.cosmon/state'));
    return new Client({root, state, projectId: input.projectId, fleet: input.fleet ?? 'default', cs: input.cs ?? 'cs'}, processes);
  }
  argv(...command) { return [this.settings.cs, '--config', this.settings.state, '--json', ...command]; }
  async json(...command) { try { return JSON.parse(await this.processes.run(this.argv(...command), this.settings.root)); } catch (e) { throw Error('Public observation unavailable: ' + e.message); } }
  async terminal(id) {
    exactId(id);
    let value;
    try { value = await this.json('terminal', id); } catch { return {availability: 'unknown', reason: 'public terminal observation unavailable'}; }
    return validateTerminal(value, this.settings, id);
  }
  async list(history = false) {
    const value = await this.json('ensemble');
    if (!value || value.schema_version !== 1 || !Array.isArray(value.molecule_states)) throw Error('Unsupported ensemble contract');
    return value.molecule_states.filter(row => {
      exactId(row?.id);
      if (typeof row.status !== 'string') throw Error('Invalid molecule row');
      return row.fleet === this.settings.fleet && (history || ['pending', 'running', 'frozen'].includes(row.status));
    }).map(({id, status}) => ({id, status}));
  }
  async detail(id) {
    exactId(id);
    // Validate selected project even for non-terminal navigation.
    const terminal = await this.terminal(id);
    const value = await this.json('observe', id);
    if (!value || value.id !== id || typeof value.status !== 'string') throw Error('Observe did not return the exact selected molecule');
    return {value, terminal};
  }
  attachment(id, token) {
    exactId(id);
    return [this.settings.cs, '--config', this.settings.state, 'terminal', id, '--attach', '--expect', token];
  }
  async worktree(detail) {
    if (typeof detail.branch !== 'string' || !detail.branch) throw Error('No observed worktree branch');
    const registry = await this.processes.run(['git', 'worktree', 'list', '--porcelain', '-z'], this.settings.root);
    const matches = registry.split('\0\0').map(record => Object.fromEntries(record.split('\0').filter(f => f.includes(' ')).map(f => [f.slice(0, f.indexOf(' ')), f.slice(f.indexOf(' ') + 1)]))).filter(record => record.branch === 'refs/heads/' + detail.branch);
    if (matches.length !== 1 || !(await stat(matches[0].worktree)).isDirectory()) throw Error('No unique existing worktree');
    return realpath(matches[0].worktree);
  }
  async artifacts(detail) {
    // Conventional deliverables only; state.json and other private store files are never read.
    const root = path.join(this.settings.state, 'fleets', this.settings.fleet, 'molecules', detail.id);
    if (detail.molecule_dir !== root || await realpath(root) !== root || (await lstat(root)).isSymbolicLink()) throw Error('Canonical artifact residence unavailable');
    const candidates = [...deliverables];
    const responses = path.join(root, 'responses');
    try { if (!(await lstat(responses)).isSymbolicLink() && (await stat(responses)).isDirectory()) candidates.push(...(await readdir(responses)).filter(n => n.endsWith('.md')).sort().map(n => 'responses/' + n)); } catch (e) { if (e.code !== 'ENOENT') throw e; }
    const files = [];
    for (const name of candidates) {
      try { const s = await lstat(path.join(root, name)); if (s.isFile() && !s.isSymbolicLink()) files.push(name); } catch (e) { if (e.code !== 'ENOENT') throw e; }
    }
    return {root, files};
  }
  async readArtifact(detail, name) {
    const {root, files} = await this.artifacts(detail);
    if (!files.includes(name)) throw Error('Artifact absent or outside conventional deliverables');
    const filename = path.join(root, name);
    const handle = await open(filename, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const s = await handle.stat();
      if (!s.isFile() || s.size > 1024 * 1024 || await realpath(filename) !== filename) throw Error('Artifact is not a bounded canonical file');
      const buffer = Buffer.alloc(1024 * 1024 + 1);
      const {bytesRead} = await handle.read(buffer, 0, buffer.length, 0);
      if (bytesRead > 1024 * 1024) throw Error('Artifact exceeds 1 MiB');
      return buffer.subarray(0, bytesRead).toString('utf8');
    } finally { await handle.close(); }
  }
}
/** Remove terminal controls from labels, artifacts and output before Pi rendering. */
export function safeText(text) {
  return String(text).replace(/\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)/g, '').replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '').replace(/[\x00-\x08\x0b-\x1f\x7f-\x9f]/g, '');
}
