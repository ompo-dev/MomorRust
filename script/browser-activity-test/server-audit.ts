import type { IncomingMessage, ServerResponse } from 'node:http';
import type { Plugin, PreviewServer, ViteDevServer } from 'vite';

interface Heartbeat { lastMonotonicAt: number; seenAt: number; maxGapMs: number; count: number }

export function serverAudit(): Plugin {
  function configure(server: ViteDevServer | PreviewServer) {
    const sessions = new Map<string, Heartbeat>();
    server.middlewares.use('/api/activity-heartbeat', (request, response) => {
      handle(request, response).catch(() => {
        if (!response.headersSent) response.writeHead(500, { 'Content-Type': 'application/json' });
        response.end('{"error":"heartbeat unavailable"}');
      });
    });
    async function handle(request: IncomingMessage, response: ServerResponse) {
      response.setHeader('Cache-Control', 'no-store');
      response.setHeader('Content-Type', 'application/json');
      const address = server.httpServer?.address();
      let origin: URL | null = null;
      try { origin = new URL(request.headers.origin ?? ''); } catch { origin = null; }
      if (!origin || origin.protocol !== 'http:' || !['localhost', '127.0.0.1'].includes(origin.hostname)
        || typeof address !== 'object' || !address || origin.port !== String(address.port)) {
        response.writeHead(403); response.end('{"error":"origin rejected"}'); return;
      }
      if (request.method !== 'POST') { response.writeHead(405); response.end('{"error":"POST required"}'); return; }
      if (!request.headers['content-type']?.startsWith('application/json')) {
        response.writeHead(415); response.end('{"error":"JSON required"}'); return;
      }
      const chunks: Buffer[] = [];
      let length = 0;
      for await (const chunk of request) {
        const buffer = Buffer.from(chunk);
        length += buffer.length;
        if (length > 4096) { response.writeHead(413); response.end('{"error":"body too large"}'); return; }
        chunks.push(buffer);
      }
      let body: unknown;
      try { body = JSON.parse(Buffer.concat(chunks).toString('utf8')); }
      catch { response.writeHead(400); response.end('{"error":"invalid JSON"}'); return; }
      const id = body && typeof body === 'object' ? (body as { id?: unknown }).id : null;
      if (typeof id !== 'string' || !/^[\w.-]{1,160}$/.test(id)) {
        response.writeHead(400); response.end('{"error":"invalid id"}'); return;
      }
      const now = Number(process.hrtime.bigint() / 1000000n);
      const seenAt = Date.now();
      for (const [key, value] of sessions) if (seenAt - value.seenAt > 1800000) sessions.delete(key);
      const previous = sessions.get(id);
      const intervalMs = previous ? Math.max(0, now - previous.lastMonotonicAt) : null;
      const heartbeat = { lastMonotonicAt: now, seenAt, maxGapMs: Math.max(previous?.maxGapMs ?? 0, intervalMs ?? 0), count: (previous?.count ?? 0) + 1 };
      sessions.set(id, heartbeat);
      if (sessions.size > 128) {
        const oldest = sessions.keys().next().value;
        if (oldest !== undefined) sessions.delete(oldest);
      }
      response.end(JSON.stringify({ protocol: 'momor-heartbeat-v1', intervalMs,
        maxGapMs: heartbeat.maxGapMs, count: heartbeat.count, seenAt }));
    }
  }
  return { name: 'momor-local-activity-audit', configureServer: configure, configurePreviewServer: configure };
}
