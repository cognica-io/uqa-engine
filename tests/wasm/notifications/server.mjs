//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

// Loopback-only protocol peer for real-browser Fetch and CORS acceptance.
import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { resolve, extname, sep } from "node:path";
import { gzipSync } from "node:zlib";

const root = fileURLToPath(new URL("../../../", import.meta.url));
const fixture = JSON.parse(await readFile(new URL("../../../crates/uqa-client/tests/fixtures/notifications-v1.json", import.meta.url)));
const streams = new Set(), sockets = new Set(), waiting = new Set();
let mode = "normal", requests = [], preflights = [], closed = 0, redirects = 0;
let peerOrigin, pageOrigin;
const wire = (kind, value) => "event: " + kind + "\ndata: " + JSON.stringify(value) + "\n\n";
const data = (text) => JSON.parse(text.split("data: ")[1]);
const json = (response, value) => { if (!response.headersSent) response.setHeader("content-type", "application/json"); response.end(JSON.stringify(value)); };
const counters = () => ({ requests, preflights, closed, redirects, active: streams.size });
function notify() { for (const wake of [...waiting]) wake(); }
function waitFor(predicate, response) {
  return new Promise((done) => {
    const abandon = () => { waiting.delete(wake); done(false); };
    const wake = () => { if (predicate()) { waiting.delete(wake); response.removeListener("close", abandon); done(true); } };
    response.once("close", abandon); waiting.add(wake); wake();
  });
}
async function body(request) {
  let result = "";
  for await (const chunk of request) {
    result += chunk;
    if (result.length > 65536) throw new Error("oversized test request");
  }
  return result;
}
function track(response) {
  streams.add(response);
  response.once("close", () => { streams.delete(response); closed += 1; notify(); });
}
function identity(attempt) {
  const ready = data(fixture.ready), notification = data(fixture.notification);
  if (mode === "reconnect" && attempt > 1) {
    ready.stream_id = notification.stream_id = "af71050f-1c18-4e52-9c3e-1ec1ccf1e902";
    ready.request_id = notification.request_id = "request_2";
  }
  return { ready, notification };
}
function start(response, value, notification = true) {
  response.write(wire("ready", value.ready));
  if (notification) response.write(wire("notification", value.notification));
  if (mode !== "idle") {
    const timer = setInterval(() => response.write(": keepalive\n\n"), 100);
    response.once("close", () => clearInterval(timer));
  }
}
async function peer(request, response) {
  response.setHeader("access-control-allow-origin", pageOrigin);
  response.setHeader("access-control-allow-methods", "POST");
  response.setHeader("access-control-allow-headers", "authorization, content-type");
  response.setHeader("access-control-expose-headers", mode === "hidden-identity" ? "content-encoding" : "x-request-id, retry-after, content-encoding");
  response.setHeader("cache-control", "no-store, no-transform");
  if (request.method === "OPTIONS") {
    preflights.push({ method: request.headers["access-control-request-method"], headers: request.headers["access-control-request-headers"] });
    response.statusCode = 204; response.end(); return;
  }
  const text = await body(request);
  requests.push({ path: request.url, body: text, authorizationOK: request.headers.authorization === "Bearer browser-fixture",
    cookie: request.headers.cookie ?? null, referrer: request.headers.referer ?? null });
  track(response); notify();
  if (request.url !== "/v1/notifications/subscribe") { response.statusCode = 404; response.end(); return; }
  if (mode === "headers-pending") return;
  if (mode === "redirect") { response.writeHead(307, { location: pageOrigin + "/unexpected-redirect" }); response.end(); return; }
  if (mode === "unsupported") { response.statusCode = 404; response.end(); return; }
  if (mode === "retry-pending" || (mode === "backoff" && requests.length > 1)) {
    response.writeHead(503, { "content-type": "application/json", "x-request-id": fixture.request_id, "retry-after": "1" });
    json(response, { error: { code: "NOTIFICATION_SOURCE_UNAVAILABLE", message: "private fixture diagnostic" }, request_id: fixture.request_id }); return;
  }
  const value = identity(requests.length);
  response.writeHead(200, { "content-type": "text/event-stream; charset=utf-8", "x-request-id": value.ready.request_id,
    "content-encoding": mode === "gzip" ? "gzip" : "identity", "set-cookie": "response_cookie=forbidden; Path=/" });
  response.flushHeaders();
  if (mode === "ready-pending") return;
  if (mode === "malformed") { response.write(new Uint8Array([0xff, 10, 10])); return; }
  if (mode === "gzip") { response.end(gzipSync(wire("ready", value.ready))); return; }
  start(response, value, mode !== "idle" && mode !== "broadcast");
}
async function control(request, response) {
  const command = JSON.parse(await body(request));
  if (command.action === "configure") {
    if (streams.size) throw new Error("the previous fixture still owns a stream");
    mode = command.mode; requests = []; preflights = []; closed = 0; redirects = 0;
    json(response, { origin: peerOrigin, fixture });
  } else if (command.action === "wait") {
    const matched = await waitFor(() => (command.closed === undefined || closed >= command.closed)
      && (command.requests === undefined || requests.length >= command.requests), response);
    if (matched) json(response, counters());
  } else if (command.action === "ready") {
    for (const stream of streams) start(stream, identity(1));
    json(response, counters());
  } else if (command.action === "drop") {
    for (const stream of streams) stream.destroy();
    json(response, counters());
  } else if (command.action === "broadcast") {
    for (const stream of streams) stream.write(command.wire);
    json(response, counters());
  } else json(response, counters());
}
async function page(request, response) {
  if (request.url === "/favicon.ico") { response.statusCode = 204; response.end(); return; }
  if (request.url === "/__notification_control" && request.method === "POST") return control(request, response);
  if (request.url === "/unexpected-redirect") { redirects += 1; response.end(); return; }
  const path = resolve(root, "." + new URL(request.url, pageOrigin).pathname);
  if (!path.startsWith(root.endsWith(sep) ? root : root + sep) || !(await stat(path)).isFile()) {
    response.statusCode = 404; response.end(); return;
  }
  const types = { ".mjs": "text/javascript", ".js": "text/javascript", ".json": "application/json", ".html": "text/html", ".wasm": "application/wasm" };
  response.setHeader("content-type", types[extname(path)] ?? "application/octet-stream");
  response.setHeader("cache-control", "no-store");
  response.end(await readFile(path));
}
function serve(handler) {
  const server = createServer((request, response) => {
    Promise.resolve(handler(request, response)).catch((error) => { console.error(error); response.destroy(); });
  });
  server.on("connection", (socket) => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); });
  return server;
}
const peerServer = serve(peer), pageServer = serve(page);
await new Promise((done) => peerServer.listen(0, "127.0.0.1", done));
await new Promise((done) => pageServer.listen(0, "127.0.0.1", done));
peerOrigin = "http://127.0.0.1:" + peerServer.address().port;
pageOrigin = "http://127.0.0.1:" + pageServer.address().port;
console.log(pageOrigin + "/tests/wasm/notifications/browser.html");
for (const signal of ["SIGINT", "SIGTERM"]) process.once(signal, () => {
  for (const stream of streams) stream.destroy();
  for (const socket of sockets) socket.destroy();
  peerServer.close(); pageServer.close();
});
