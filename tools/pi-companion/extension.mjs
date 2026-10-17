// SPDX-License-Identifier: AGPL-3.0-only
/** Native Pi commands; no model tools, automatic turns or startup I/O. */
import { fileURLToPath } from 'node:url';
import { Client, Processes, exactId, safeText } from './client.mjs';

/** Factory is exported separately so contract tests require no Pi installation. */
export function register(pi, {truncateToWidth, matchesKey, Key}) {
  let processes = new Processes(), client, selected, busy = false, closeView;
  const shutdown = () => { closeView?.(); processes.close(); client = undefined; selected = undefined; };
  pi.on('session_shutdown', shutdown);
  pi.on('session_start', () => { if (processes.closed) processes = new Processes(); });

  async function screen(ctx, title, text) {
    return ctx.ui.custom((tui, _theme, _keys, done) => {
      let offset = 0;
      const lines = safeText(text).split('\n');
      const finish = () => done();
      closeView = finish;
      return {
        render(width) { return [title, 'q/Esc/Ctrl-C close | arrows scroll', ...lines.slice(offset, offset + Math.max(1, tui.terminal.rows - 5))].map(line => truncateToWidth(safeText(line), width)); },
        invalidate() {},
        handleInput(data) {
          if (data === 'q' || matchesKey(data, Key.escape) || matchesKey(data, Key.ctrl('c'))) finish();
          if (matchesKey(data, Key.down)) offset = Math.min(Math.max(0, lines.length - 1), offset + 1);
          if (matchesKey(data, Key.up)) offset = Math.max(0, offset - 1);
          tui.requestRender();
        },
        dispose() { if (closeView === finish) closeView = undefined; },
      };
    });
  }
  const requireClient = () => { if (!client) throw Error('Select an explicit project with /cosmon-project first'); return client; };
  const requireId = args => exactId(args.trim() || selected);
  const command = (name, description, handler) => pi.registerCommand(name, {
    description,
    async handler(args, ctx) {
      if (ctx.mode !== 'tui') { ctx.ui.notify('Cosmon commands require Pi interactive TUI mode', 'warning'); return; }
      if (busy) { ctx.ui.notify('Close the current Cosmon view first', 'warning'); return; }
      busy = true;
      try { await handler(args, ctx); } catch (error) { ctx.ui.notify('Cosmon: ' + safeText(error.message), 'error'); }
      finally { busy = false; }
    },
  });

  command('cosmon-project', 'Select an explicit Cosmon project (JSON or dialogs)', async (args, ctx) => {
    let settings;
    if (args.trim()) settings = JSON.parse(args);
    else {
      const root = await ctx.ui.input('Cosmon project: absolute Git checkout path');
      if (root === undefined) return;
      const projectId = await ctx.ui.input('Cosmon project_id from the chosen project configuration');
      if (projectId === undefined) return;
      settings = {root, projectId};
    }
    const next = await Client.select(settings, processes);
    client = next; selected = undefined;
    ctx.ui.notify('Cosmon project selected: ' + safeText(next.settings.root) + ' [' + safeText(next.settings.projectId) + ']', 'info');
  });
  async function choose(args, ctx) {
    const c = requireClient();
    if (args.trim() && args.trim() !== 'all') throw Error('Use /cosmon-list or /cosmon-list all');
    const rows = await c.list(args.trim() === 'all');
    if (!rows.length) { ctx.ui.notify('No molecules in the selected fleet/filter', 'info'); return; }
    const choices = rows.map(row => row.id + ' [' + safeText(row.status) + ']');
    const choice = await ctx.ui.select('Cosmon molecules: ' + safeText(c.settings.projectId), choices);
    if (choice === undefined) return;
    const index = choices.indexOf(choice);
    if (index < 0) throw Error('Selection is no longer available');
    await details(rows[index].id, ctx);
  }
  async function details(id, ctx) {
    const {value, terminal} = await requireClient().detail(id);
    selected = id;
    await screen(ctx, 'Cosmon molecule ' + id, 'Status: ' + value.status + '\nWorker terminal: ' + terminal.availability + (terminal.reason ? ' (' + terminal.reason + ')' : '') + '\n\n/cosmon-output | /cosmon-artifacts | /cosmon-worktree\n\n' + JSON.stringify({id: value.id, status: value.status, branch: value.branch, steps: value.completed_steps, current_step: value.current_step, total_steps: value.total_steps}, null, 2));
  }
  command('cosmon', 'Select a molecule in the explicitly selected project', choose);
  command('cosmon-list', 'List/select unfinished molecules; all includes history', choose);
  command('cosmon-select', 'Select one complete molecule id', async (args, ctx) => details(exactId(args.trim()), ctx));
  command('cosmon-details', 'Show selected molecule details', async (args, ctx) => details(requireId(args), ctx));
  command('cosmon-worktree', 'Show verified existing worktree path for navigation', async (args, ctx) => {
    const c = requireClient(), id = requireId(args);
    const {value} = await c.detail(id);
    await screen(ctx, 'Cosmon worktree ' + id, await c.worktree(value) + '\n\nExisting path only. Navigate using your terminal/editor; no shell is launched.');
  });
  command('cosmon-artifacts', 'Select a conventional Markdown artifact to read', async (args, ctx) => {
    const c = requireClient(), id = requireId(args);
    const {value} = await c.detail(id);
    const {files} = await c.artifacts(value);
    if (!files.length) throw Error('No conventional Markdown deliverables available');
    const choice = await ctx.ui.select('Cosmon artifacts ' + id, files);
    if (choice === undefined) return;
    // Re-observe before reading, never retain residence from a previous view.
    const fresh = await c.detail(id);
    await screen(ctx, 'Cosmon artifact (read-only): ' + safeText(choice), await c.readArtifact(fresh.value, choice));
  });
  command('cosmon-output', 'Open an output-only view of the selected worker', async (args, ctx) => {
    const c = requireClient(), id = requireId(args);
    const terminal = await c.terminal(id);
    if (terminal.availability !== 'available') throw Error('Terminal ' + terminal.availability + ': ' + terminal.reason);
    await ctx.ui.custom((tui, _theme, _keys, done) => {
      let frame = 'Opening public read-only attachment…', pending = '', raw = '', disposed = false;
      const bridge = fileURLToPath(new URL('./pty_bridge.py', import.meta.url));
      const child = processes.spawn(['python3', bridge, ...c.attachment(id, terminal.expected)], c.settings.root, {stdio: ['pipe', 'pipe', 'pipe']});
      const finish = () => done();
      closeView = finish;
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', data => {
        pending += data;
        if (pending.length > 1024 * 1024) { frame = 'Output frame exceeds bound'; processes.stop(child); pending = ''; }
        let newline;
        while ((newline = pending.indexOf('\n')) >= 0) {
          const line = pending.slice(0, newline); pending = pending.slice(newline + 1);
          try {
            const text = JSON.parse(line).data;
            if (typeof text !== 'string') throw Error('Invalid bridge frame');
            raw = (raw + text).slice(-256 * 1024);
            const clear = raw.lastIndexOf('\x1b[H\x1b[2J');
            if (clear >= 0) raw = raw.slice(clear + 7);
            frame = safeText(raw);
          } catch { frame = 'Invalid output bridge response'; processes.stop(child); }
        }
        if (!disposed) tui.requestRender();
      });
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', data => { frame = (frame + '\nOutput bridge: ' + safeText(data)).slice(-256 * 1024); if (!disposed) tui.requestRender(); });
      child.on('error', () => { frame = 'Python output bridge unavailable'; if (!disposed) tui.requestRender(); });
      child.on('close', code => { frame += '\n[view ended: ' + code + '; reopen for fresh observation]'; if (!disposed) tui.requestRender(); });
      return {
        render(width) { return ['Cosmon output (read-only): ' + id, 'q/Esc/Ctrl-C close | keyboard and size stay local', ...frame.split('\n').slice(0, Math.max(1, tui.terminal.rows - 5))].map(line => truncateToWidth(safeText(line), width)); },
        invalidate() {},
        handleInput(data) { if (data === 'q' || matchesKey(data, Key.escape) || matchesKey(data, Key.ctrl('c'))) finish(); },
        dispose() { disposed = true; child.stdin.end(); processes.stop(child); if (closeView === finish) closeView = undefined; },
      };
    });
  });
}
