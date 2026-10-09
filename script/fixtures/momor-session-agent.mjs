// Offline ACP fixture for restart and attachment tests. No model or network calls.
import { createInterface } from 'node:readline';
import { randomUUID } from 'node:crypto';
import { readFileSync, writeFileSync, renameSync } from 'node:fs';

const storage = process.argv[2];
if (!storage) throw new Error('Provide an isolated test session JSON path');
let sessions = {};
try { sessions = JSON.parse(readFileSync(storage, 'utf8')); }
catch (error) { if (error.code !== 'ENOENT') throw error; }

function send(message) { process.stdout.write(JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n'); }
function update(sessionId, sessionUpdate, content) {
  send({ method: 'session/update', params: { sessionId, update: { sessionUpdate, content } } });
}
function save() {
  writeFileSync(storage + '.tmp', JSON.stringify(sessions));
  renameSync(storage + '.tmp', storage);
}
const input = createInterface({ input: process.stdin });
input.on('line', line => {
  let request;
  try {
    request = JSON.parse(line);
    if (request.id == null) return;
    const { id, method, params = {} } = request;
    let result = {};
    if (method === 'initialize') {
      result = { protocolVersion: params.protocolVersion, agentInfo: { name: 'Momor Session Test', version: '1' },
        agentCapabilities: { loadSession: true, promptCapabilities: { embeddedContext: false, image: true } }, authMethods: [] };
    } else if (method === 'session/new') {
      const sessionId = randomUUID();
      sessions[sessionId] = [];
      save();
      result = { sessionId };
    } else if (method === 'session/prompt') {
      const messages = sessions[params.sessionId];
      if (!messages) throw new Error('Unknown test session');
      messages.push({ role: 'user', content: params.prompt });
      const content = { type: 'text', text: 'Conversa de teste preservada. Anexo recebido pelo agente local.' };
      messages.push({ role: 'assistant', content: [content] });
      save();
      send({ method: 'session/update', params: { sessionId: params.sessionId,
        update: { sessionUpdate: 'session_info_update', title: 'Momor persistence regression' } } });
      update(params.sessionId, 'agent_message_chunk', content);
      result = { stopReason: 'end_turn' };
    } else if (method === 'session/load') {
      const messages = sessions[params.sessionId];
      if (!messages) throw new Error('Unknown saved test session');
      for (const message of messages) {
        for (const content of message.content) {
          if (content.type === 'text' && content.text.startsWith('Momor browser')) continue;
          update(params.sessionId, message.role === 'user' ? 'user_message_chunk' : 'agent_message_chunk', content);
        }
      }
    } else if (method !== 'authenticate' && method !== 'session/cancel') {
      send({ id, error: { code: -32601, message: `Unknown test method: ${method}` } });
      return;
    }
    send({ id, result });
  } catch (error) {
    process.stderr.write(String(error.stack ?? error) + '\n');
    if (request?.id != null) send({ id: request.id, error: { code: -32603, message: String(error.message) } });
  }
});
