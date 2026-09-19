#!/usr/bin/env node
// Static file server for the HTML UI in mock mode (replaces `python -m http.server`).
//
//   node tools/ui-serve.mjs [--port 8123] [--host 127.0.0.1] [--root ui] [--quiet]
//   → http://127.0.0.1:8123/sidebar/          (mock mode is implied outside sta://)
//
// - Correct MIME types for everything under ui/ (module scripts need `text/javascript`; Python's
//   server takes them from the Windows registry, which can say `text/plain` and break every page).
// - `Cache-Control: no-store` and `X-Content-Type-Options: nosniff`, like the sta:// scheme.
// - A directory without a trailing slash redirects to it (relative URLs resolve like in the app);
//   a directory serves its index.html. Paths can't escape the root.
// - Handles concurrent requests; `close()` also drops idle keep-alive connections.
//
// Also used as a module by tools/ui-shot.mjs: `startStaticServer({root, host, port, onResponse})`.

import http from 'node:http';
import { createReadStream } from 'node:fs';
import { stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

export const MIME_TYPES = Object.freeze({
  '.html': 'text/html; charset=utf-8',
  '.htm': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.map': 'application/json; charset=utf-8',
  '.txt': 'text/plain; charset=utf-8',
  '.md': 'text/markdown; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.avif': 'image/avif',
  '.ico': 'image/x-icon',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.otf': 'font/otf',
  '.wasm': 'application/wasm',
});

export const mimeTypeOf = (file) => MIME_TYPES[path.extname(file).toLowerCase()] ?? 'application/octet-stream';

const COMMON_HEADERS = { 'Cache-Control': 'no-store', 'X-Content-Type-Options': 'nosniff' };

/**
 * Serve `root` over HTTP.
 * @param {{root: string, host?: string, port?: number, onResponse?: (info: {method: string, url: string, status: number}) => void}} options
 *   `port` 0 (default) picks a free port. `onResponse` is called once per request.
 * @returns {Promise<{server: http.Server, host: string, port: number, origin: string, close: () => Promise<void>}>}
 */
export async function startStaticServer({ root, host = '127.0.0.1', port = 0, onResponse } = {}) {
  const base = path.resolve(root);
  if (!(await stat(base)).isDirectory()) throw new Error(`not a directory: ${base}`);

  const server = http.createServer((req, res) => {
    res.once('finish', () => onResponse?.({ method: req.method, url: req.url, status: res.statusCode }));
    serve(req, res, base).catch((e) => {
      if (res.headersSent) res.destroy();
      else sendText(req, res, 500, `Internal Server Error: ${e.message}`);
    });
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, host, () => {
      server.off('error', reject);
      resolve();
    });
  });
  const actual = server.address().port;
  return {
    server,
    host,
    port: actual,
    origin: `http://${host}:${actual}`,
    close: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
        server.closeAllConnections?.();
      }),
  };
}

function sendText(req, res, status, message, headers = {}) {
  const body = `${status} ${message}\n`;
  res.writeHead(status, { ...COMMON_HEADERS, 'Content-Type': 'text/plain; charset=utf-8', 'Content-Length': Buffer.byteLength(body), ...headers });
  res.end(req.method === 'HEAD' ? undefined : body);
}

async function serve(req, res, base) {
  if (req.method !== 'GET' && req.method !== 'HEAD') return sendText(req, res, 405, 'Method Not Allowed', { Allow: 'GET, HEAD' });
  let url;
  let pathname;
  try {
    url = new URL(req.url, 'http://localhost');
    pathname = decodeURIComponent(url.pathname);
  } catch {
    return sendText(req, res, 400, 'Bad Request');
  }
  if (pathname.includes('\0')) return sendText(req, res, 400, 'Bad Request');
  const requested = path.resolve(base, `.${pathname}`);
  if (requested !== base && !requested.startsWith(base + path.sep)) return sendText(req, res, 403, 'Forbidden');

  let file = requested;
  let info = await stat(file).catch(() => null);
  if (info?.isDirectory()) {
    if (!url.pathname.endsWith('/')) return sendText(req, res, 301, 'Moved Permanently', { Location: `${url.pathname}/${url.search}` });
    file = path.join(requested, 'index.html');
    info = await stat(file).catch(() => null);
  }
  if (!info?.isFile()) return sendText(req, res, 404, 'Not Found');

  const headers = { ...COMMON_HEADERS, 'Content-Type': mimeTypeOf(file), 'Content-Length': info.size };
  if (req.method === 'HEAD') {
    res.writeHead(200, headers);
    return res.end();
  }
  await new Promise((resolve, reject) => {
    const stream = createReadStream(file);
    stream.once('error', reject);
    stream.once('open', () => {
      res.writeHead(200, headers);
      stream.pipe(res);
    });
    res.once('close', resolve);
  });
}

// ------------------------------------------------------------------------------------ CLI

const isMain = Boolean(process.argv[1]) && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href;
if (isMain) {
  const args = process.argv.slice(2);
  const opt = (name, fallback) => {
    const i = args.indexOf(`--${name}`);
    return i >= 0 && i + 1 < args.length ? args[i + 1] : fallback;
  };
  const quiet = args.includes('--quiet');
  const root = path.resolve(opt('root', path.join(path.dirname(fileURLToPath(import.meta.url)), '..', 'ui')));
  const { origin, close } = await startStaticServer({
    root,
    host: opt('host', '127.0.0.1'),
    port: Number(opt('port', '8123')),
    onResponse: quiet ? undefined : ({ method, url, status }) => console.log(`${status} ${method} ${url}`),
  });
  console.log(`serving ${root} at ${origin}/  (e.g. ${origin}/sidebar/?mock, ${origin}/_gallery/)`);
  const stop = () => close().then(() => process.exit(0));
  process.on('SIGINT', stop);
  process.on('SIGTERM', stop);
}
