// node --test src-ui/shared/xfer.test.mjs（npm test）
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import vm from 'node:vm';

import { ApiError } from './api.js';
import { UploadStore, runUpload } from './uploader.js';
import { formatBytes, isShellCommand, joinPaths, nextChunk, quotePath, routeFor, transferAction } from './xfer.js';

test('quotePath: POSIX', () => {
  assert.equal(quotePath('/home/a/x.png', 'linux'), '/home/a/x.png');
  assert.equal(quotePath('/home/a/my shot.png', 'macos'), "'/home/a/my shot.png'");
  assert.equal(quotePath("/tmp/it's.png", 'linux'), "'/tmp/it'\\''s.png'");
  assert.equal(quotePath('/tmp/a(1).png', 'linux'), "'/tmp/a(1).png'");
  assert.equal(quotePath('/tmp/中文.png', 'linux'), "'/tmp/中文.png'");
  assert.equal(quotePath('~/x', 'linux'), "'~/x'");
});

test('quotePath: Windows', () => {
  assert.equal(quotePath('C:\\Users\\a\\x.png', 'windows'), 'C:\\Users\\a\\x.png');
  assert.equal(quotePath('C:\\Users\\a b\\x (1).png', 'windows'), '"C:\\Users\\a b\\x (1).png"');
  assert.equal(quotePath('C:\\a&b.txt', 'windows'), '"C:\\a&b.txt"');
});

test('joinPaths', () => {
  assert.equal(joinPaths(['/a/b.png', '/a/c d.png'], 'linux'), "/a/b.png '/a/c d.png'");
});

test('routeFor follows §3.3', () => {
  assert.deepEqual(routeFor('paste', { ai: true }), { dest: 'attach', insert: true });
  assert.deepEqual(routeFor('drop', { ai: false }), { dest: 'cwd', insert: true });
  assert.deepEqual(routeFor('pick', { ai: true }), { dest: 'attach', insert: true });
  assert.deepEqual(routeFor('pick', { ai: true, aiMode: 'cwd' }), { dest: 'cwd', insert: false });
  assert.deepEqual(routeFor('pick', { ai: false }), { dest: 'cwd', insert: false });
  assert.deepEqual(routeFor('pick', { ai: false, insertChecked: true }), { dest: 'cwd', insert: true });
  assert.deepEqual(routeFor('rz', { ai: true }), { dest: 'rz', insert: false });
});

test('formatBytes / nextChunk', () => {
  assert.equal(formatBytes(512), '512 B');
  assert.equal(formatBytes(1.2 * 1024 * 1024), '1.2 MB');
  assert.deepEqual(nextChunk(0, 10, 4), { start: 0, end: 4 });
  assert.deepEqual(nextChunk(8, 10, 4), { start: 8, end: 10 });
  assert.equal(nextChunk(10, 10, 4), null);
});

test('transferAction', () => {
  assert.equal(transferAction('rz', null), null);
  assert.deepEqual(transferAction('rz', { kind: 'rz', local: true, pid: 9, cwd: '/x' }).key, 'rz:9');
  assert.equal(transferAction('sz', { kind: 'sz', local: true, pid: 3, files: [] }).mode, 'sz');
  assert.equal(transferAction('rz', { kind: 'unknown', local: false, command: 'ssh' }).mode, 'remote');
  // 前台已经回到 shell：横幅是重绘出来的旧内容
  assert.equal(transferAction('rz', { kind: 'unknown', local: false, command: 'zsh' }), null);
  assert.equal(transferAction('rz', { kind: 'unknown', local: false, command: '-bash' }), null);
  assert.ok(isShellCommand('/bin/fish'));
  assert.ok(!isShellCommand('ssh'));
});

// ---------- 分块上传 ----------
function fakeFile(size, name = 'a.bin') {
  return { name, size, slice: (s, e) => ({ s, e, size: e - s }) };
}

function fakeServer({ chunk = 4, failPuts = [], dropAfterWrite = [] } = {}) {
  const s = { received: 0, puts: [], created: 0, finished: false, removed: false };
  let n = 0;
  s.transport = {
    create: async (_id, body) => {
      s.created += 1;
      s.size = body.size;
      return { id: 'u_1', path: `/dst/${body.name}`, chunk_size: chunk };
    },
    put: async (_uid, offset, blob, onProgress) => {
      n += 1;
      s.puts.push(offset);
      if (failPuts.includes(n)) throw new ApiError(0, 'CLIENT_UNREACHABLE');
      if (offset !== s.received) throw new ApiError(409, 'UPLOAD_OFFSET', { received: s.received });
      s.received += blob.size;
      onProgress(blob.size);
      // 写进去了但响应丢了（断网）：客户端会重发同一块，服务端回 409 带正确位置
      if (dropAfterWrite.includes(n)) throw new ApiError(0, 'CLIENT_UNREACHABLE');
      return { received: s.received };
    },
    finish: async () => {
      if (s.received !== s.size) throw new ApiError(409, 'UPLOAD_INCOMPLETE', { received: s.received });
      s.finished = true;
      return { path: '/dst/final.bin' };
    },
    remove: async () => {
      s.removed = true;
    },
  };
  return s;
}

const noWait = async () => {};

test('runUpload: chunks in order and finishes', async () => {
  const srv = fakeServer();
  const task = { file: fakeFile(10), dest: 'cwd', sent: 0 };
  const path = await runUpload(task, { instanceId: 'x', transport: srv.transport, wait: noWait });
  assert.equal(path, '/dst/final.bin');
  assert.deepEqual(srv.puts, [0, 4, 8]);
  assert.ok(srv.finished);
  assert.equal(task.sent, 10);
});

test('runUpload: empty file goes straight to finish', async () => {
  const srv = fakeServer();
  await runUpload({ file: fakeFile(0), dest: 'attach', sent: 0 }, { instanceId: 'x', transport: srv.transport, wait: noWait });
  assert.deepEqual(srv.puts, []);
  assert.ok(srv.finished);
});

test('runUpload: retries network errors and resumes from UPLOAD_OFFSET', async () => {
  const srv = fakeServer({ failPuts: [2], dropAfterWrite: [3] });
  const waits = [];
  await runUpload({ file: fakeFile(10), dest: 'cwd', sent: 0 }, { instanceId: 'x', transport: srv.transport, wait: async (ms) => waits.push(ms) });
  // 1: 0 ok; 2: 4 network fail; 3: 4 written but response lost; 4: 4 again → 409 received=8; 5: 8 ok
  assert.deepEqual(srv.puts, [0, 4, 4, 4, 8]);
  assert.equal(waits.length, 2);
  assert.ok(srv.finished);
});

test('runUpload: gives up on non-retryable errors', async () => {
  const srv = fakeServer();
  srv.transport.create = async () => {
    throw new ApiError(403, 'UPLOAD_DIR_UNWRITABLE');
  };
  await assert.rejects(runUpload({ file: fakeFile(3), dest: 'cwd', sent: 0 }, { instanceId: 'x', transport: srv.transport, wait: noWait }), { code: 'UPLOAD_DIR_UNWRITABLE' });
});

test('UploadStore: batch callback and cancel', async () => {
  const srv = fakeServer({ chunk: 100 });
  const store = new UploadStore('x', srv.transport);
  const done = await new Promise((resolve) => store.add([fakeFile(5, 'a'), fakeFile(6, 'b'), fakeFile(7, 'c')], { dest: 'cwd', onDone: (fresh) => resolve(fresh) }));
  assert.equal(done.length, 3);
  assert.equal(store.progress().active, 0);
  assert.ok(store.getSnapshot().every((t) => t.state === 'done'));
});

// ---------- term-inject.js ----------
function loadInject({ files = true } = {}) {
  const posted = [];
  const listeners = {};
  class FakeWS {
    constructor(url) {
      this.url = url;
      this.l = {};
      FakeWS.last = this;
    }
    addEventListener(t, f) {
      (this.l[t] ||= []).push(f);
    }
    send() {}
    emit(text) {
      const bytes = new TextEncoder().encode(`0${text}`);
      for (const f of this.l.message || []) f({ data: bytes.buffer });
    }
  }
  const el = () => ({ style: {}, appendChild() {}, remove() {} });
  const win = {
    WebSocket: FakeWS,
    frameElement: { looklookFiles: files },
    parent: { postMessage: (m) => posted.push(m) },
    addEventListener: (t, f, capture) => (listeners[t] ||= []).push({ f, capture }),
    setTimeout: () => 0,
  };
  const ctx = {
    window: win,
    document: { createElement: el, documentElement: el(), fonts: { load: async () => {} } },
    location: { origin: 'http://x' },
    localStorage: { getItem: () => null },
    navigator: {},
    setTimeout: () => 0,
    TextDecoder,
    TextEncoder,
    Uint8Array,
    ArrayBuffer,
    Date,
    Array,
    String,
    Boolean,
  };
  vm.createContext(ctx);
  vm.runInContext(readFileSync(new URL('../../src/term-inject.js', import.meta.url), 'utf8'), ctx);
  const ws = new win.WebSocket('ws://x/i/abc/ws');
  return { ws: FakeWS.last ?? ws, posted, listeners };
}

test('term-inject: detects rz/sz banners once, across chunk boundaries', () => {
  const { ws, posted } = loadInject();
  ws.emit('hello\r\n');
  ws.emit('rz waiting to rec');
  ws.emit('eive.**B0100000023be50\r\n');
  ws.emit('**B0100000023be50\r\n'); // rz 重发 ZRINIT：静默期内不重复通知
  ws.emit('$ sz a.log\r\nrz\r**\x18B00000000000000\r\n');
  assert.deepEqual(JSON.parse(JSON.stringify(posted)), [
    { type: 'looklook:transfer', kind: 'rz' },
    { type: 'looklook:transfer', kind: 'sz' },
  ]);
});

test('term-inject: plain output does not trigger', () => {
  const { ws, posted } = loadInject();
  ws.emit('ls -la\r\n**bold** B0100 text\r\n');
  assert.deepEqual(posted, []);
});

function fakeEvent(dt) {
  const e = { dataTransfer: dt, clipboardData: dt, prevented: false, stopped: false };
  e.preventDefault = () => (e.prevented = true);
  e.stopImmediatePropagation = () => (e.stopped = true);
  e.stopPropagation = () => {};
  return e;
}
const fire = (listeners, type, e) => listeners[type].forEach((l) => l.capture && l.f(e));

test('term-inject: paste with files is intercepted, text paste is not', () => {
  const { posted, listeners } = loadInject();
  const file = { name: 'a.png', size: 1 };
  const withFile = fakeEvent({ types: ['Files'], items: [{ kind: 'file', getAsFile: () => file }], files: [file] });
  fire(listeners, 'paste', withFile);
  assert.ok(withFile.prevented && withFile.stopped);
  const text = fakeEvent({ types: ['text/plain'], items: [{ kind: 'string' }], files: [] });
  fire(listeners, 'paste', text);
  assert.ok(!text.prevented);
  assert.equal(posted.length, 1);
  assert.equal(posted[0].type, 'looklook:files');
  assert.equal(posted[0].source, 'paste');
  assert.equal(posted[0].files[0], file);
});

test('term-inject: drop is intercepted only when the console asks for it', () => {
  const file = { name: 'a.png', size: 1 };
  const dt = () => ({ types: ['Files'], items: [{ kind: 'file', getAsFile: () => file }], files: [file] });
  const a = loadInject();
  fire(a.listeners, 'dragenter', fakeEvent(dt()));
  const drop = fakeEvent(dt());
  fire(a.listeners, 'drop', drop);
  assert.ok(drop.stopped);
  assert.deepEqual(a.posted.map((m) => m.type), ['looklook:dragging', 'looklook:dragging', 'looklook:files']);
  assert.deepEqual(a.posted.slice(0, 2).map((m) => m.on), [true, false]);
  const b = loadInject({ files: false });
  const drop2 = fakeEvent(dt());
  fire(b.listeners, 'drop', drop2);
  assert.ok(!drop2.prevented);
  assert.deepEqual(b.posted, []);
});
