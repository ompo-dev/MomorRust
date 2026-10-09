// Run against a separate test instance; never navigate the user's normal tabs.
// node script/test-momor-browser.mjs --exe <momor.exe> --target <target_id>
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';

const options = new Map();
for (let index = 2; index < process.argv.length; index += 2) {
  options.set(process.argv[index], process.argv[index + 1]);
}
const executable = options.get('--exe');
const targetId = options.get('--target');
const schemaOnly = options.get('--schema-only') === 'true';
assert(executable && (targetId || schemaOnly), 'Provide --exe and --target for an isolated Momor test instance');
if (!schemaOnly) {
  const port = Number(process.env.MOMOR_BROWSER_CDP_PORT);
  assert(port >= 1024 && port <= 65535 && port !== 9224 && port !== 9222,
    'Set MOMOR_BROWSER_CDP_PORT to the isolated test instance port, never the normal user browser port');
}
const servers = [];
async function fixtureServer() {
  const server = createServer(async (request, response) => {
    const name = new URL(request.url, 'http://localhost').pathname.endsWith('momor-browser-frame.html')
      ? 'momor-browser-frame.html' : 'momor-browser-regression.html';
    try {
      response.setHeader('Content-Type', 'text/html');
      response.end(await readFile(new URL(`./fixtures/${name}`, import.meta.url)));
    } catch (error) { response.statusCode = 500; response.end(String(error)); }
  });
  await new Promise(resolve => server.listen(0, resolve));
  servers.push(server);
  return server.address().port;
}
const bridge = spawn(executable, ['--browser-mcp'], { stdio: ['pipe', 'pipe', 'inherit'] });
const pending = new Map();
let nextId = 0;
const lines = createInterface({ input: bridge.stdout });
lines.on('line', line => {
  const response = JSON.parse(line);
  pending.get(response.id)?.resolve(response);
  pending.delete(response.id);
});
bridge.on('exit', code => {
  for (const { reject } of pending.values()) reject(new Error(`MCP exited with ${code}`));
  pending.clear();
});
async function request(method, params = {}) {
  const id = ++nextId;
  const response = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`MCP timed out: ${method}`)); }, 20000);
    pending.set(id, {
      resolve: value => { clearTimeout(timer); resolve(value); },
      reject: error => { clearTimeout(timer); reject(error); },
    });
    bridge.stdin.write(JSON.stringify({ jsonrpc: '2.0', id, method, params }) + '\n');
  });
  assert(!response.error, JSON.stringify(response.error));
  return response.result;
}
async function call(name, args = {}) {
  const result = await request('tools/call', { name, arguments: { target_id: targetId, ...args } });
  assert(!result.isError, JSON.stringify(result));
  return result.content.find(item => item.type === 'text')?.text;
}
async function expectToolError(name, args, message) {
  const result = await request('tools/call', { name, arguments: { target_id: targetId, ...args } });
  assert(result.isError, `${name} should be blocked`);
  assert(result.content.some(item => item.text?.includes(message)), JSON.stringify(result));
}
async function inspectUntil(name, args, predicate) {
  for (let attempt = 0; attempt < 40; attempt++) {
    const result = JSON.parse(await call(name, args));
    if (predicate(result)) return result;
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  throw new Error(`Timed out waiting for ${name}`);
}
try {
  const initialized = await request('initialize', { protocolVersion: '2024-11-05', capabilities: {}, clientInfo: { name: 'momor-regression', version: '1' } });
  assert.equal(initialized.serverInfo.name, 'momor-browser');
  assert(initialized.instructions.includes('browser_tabs'));
  const { tools } = await request('tools/list');
  assert(tools.some(tool => tool.name === 'browser_tabs'));
  for (const tool of tools) assert(tool.inputSchema.properties.target_id);
  assert(tools.find(tool => tool.name === 'browser_dom').inputSchema.properties.frame_id);
  assert.equal(tools.find(tool => tool.name === 'browser_state').annotations.readOnlyHint, true);
  assert.equal(tools.find(tool => tool.name === 'browser_evaluate').annotations.readOnlyHint, false);
  if (schemaOnly) {
    await expectToolError('browser_evaluate', { expression: 'location.reload()' }, 'Inspection-only');
    await expectToolError('browser_navigate', { url: 'about:blank' }, 'Inspection-only');
    await call('browser_set_mode', { mode: 'actions' });
    await call('browser_set_mode', { mode: 'inspection_only' });
    await expectToolError('browser_click', { ref: 'obsolete' }, 'Inspection-only');
    console.log(`PASS: MCP initialization, ${tools.length} tools, frame schemas, read-only annotations, inspection-only guard and mode switching (no browser activity changed)`);
  } else {
  const tabs = JSON.parse(await call('browser_tabs'));
  assert(tabs.some(tab => tab.target_id === targetId));
  await expectToolError('browser_fill', { ref: 'obsolete', value: 'blocked' }, 'Inspection-only');
  await call('browser_set_mode', { mode: 'actions' });
  const mainPort = await fixtureServer();
  const framePort = await fixtureServer();
  const frameUrl = `http://localhost:${framePort}/momor-browser-frame.html`;
  const fixtureUrl = `http://127.0.0.1:${mainPort}/?frame=${encodeURIComponent(frameUrl)}`;
  await call('browser_navigate', { url: fixtureUrl, allow_navigation: true });
  const page = await inspectUntil('browser_accessibility', {}, result => result.title === 'Momor Browser Regression');
  assert.equal(page.title, 'Momor Browser Regression');
  const input = page.interactive.find(element => element.name === 'Test input');
  assert(input);
  await call('browser_fill', { ref: input.ref, value: 'typed through Momor MCP' });
  await call('browser_press', { key: 'Enter' });
  assert((await call('browser_read')).includes('typed through Momor MCP'));
  await expectToolError('browser_fill', { ref: input.ref, value: 'wrong' }, 'stale browser ref');
  const buttonPage = JSON.parse(await call('browser_accessibility'));
  await call('browser_click', { ref: buttonPage.interactive.find(element => element.name === 'Test button').ref });
  assert((await call('browser_read')).includes('button clicked'));
  const linkPage = JSON.parse(await call('browser_accessibility'));
  await call('browser_click', { ref: linkPage.interactive.find(element => element.name === 'Custom link').ref });
  assert((await call('browser_read')).includes('custom link clicked'));
  await call('browser_evaluate', { expression: "history.pushState({}, '', '#spa'); document.title = 'Momor SPA regression'; true" });
  const updated = JSON.parse(await call('browser_accessibility'));
  assert(updated.url.endsWith('#spa'));
  assert.equal(updated.title, 'Momor SPA regression');
  await expectToolError('browser_navigate', { url: 'about:blank' }, 'existing user activity');
  const frames = await inspectUntil('browser_frames', {}, result => result.some(frame => frame.url === frameUrl));
  const frameId = frames.find(frame => frame.url === frameUrl).frame_id;
  const embedded = JSON.parse(await call('browser_accessibility', { frame_id: frameId }));
  assert.equal(embedded.title, 'Embedded Momor Form');
  const frameInput = embedded.interactive.find(element => element.name === 'Frame input');
  await call('browser_fill', { frame_id: frameId, ref: frameInput.ref, value: 'frame answer' });
  const frameState = JSON.parse(await call('browser_state', { frame_id: frameId }));
  assert.equal(frameState.forms.find(element => element.name === 'Frame input').value, 'frame answer');
  await call('browser_press', { key: 'Enter' });
  assert((await call('browser_read', { frame_id: frameId })).includes('Question 2'));
  await call('browser_set_mode', { mode: 'inspection_only' });
  await expectToolError('browser_evaluate', { expression: 'location.reload()' }, 'Inspection-only');
  await call('browser_set_mode', { mode: 'actions' });
  const video = JSON.parse(await call('browser_evaluate', { expression: "({visibility:document.visibilityState, time:document.querySelector('video').currentTime, paused:document.querySelector('video').paused, frames:window.framesDrawn, timeOrigin:performance.timeOrigin})" }));
  assert.equal(video.visibility, 'visible');
  assert.equal(video.paused, false);
  assert(video.frames > 0);
  console.log('PASS: MCP discovery, tab selection, DOM refs, stale refs rejected, fill, Enter, clicks, SPA URL/title, cross-origin frame, inspection-only and navigation guards, live video');
  console.log(JSON.stringify(video));
  }
} finally {
  bridge.stdin.end();
  lines.close();
  if (bridge.exitCode === null) bridge.kill();
  for (const server of servers) server.closeAllConnections();
  await Promise.all(servers.map(server => new Promise(resolve => server.close(resolve))));
}
