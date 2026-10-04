// 分块上传（docs/FILE_TRANSFER.md §4.2）：每块一个请求，避开远程链路的 30 秒 / 100 MB 单请求限制；
// 409 UPLOAD_OFFSET 时从服务端记录的位置续传；断网、网关超时等错误自动退避重试，重试用完后可以手动“重试”接着传。
// 每个终端一个上传列表（uploadStore(id)），面板关掉后上传继续，标题栏图标上显示进度。
import { ApiError, api, deviceApi, terminalDevice } from './api.js';
import { isRetryable, nextChunk, retryDelay } from './xfer.js';

const MAX_RETRIES = 10;
const STALL_MS = 45000; // 一块传输中这么久没有任何进度：当成断网，中止后重试
const CONCURRENCY = 2;

function abortError() {
  const e = new Error('aborted');
  e.name = 'AbortError';
  return e;
}

const sleep = (ms, signal) =>
  new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(abortError());
    const timer = setTimeout(resolve, ms);
    signal?.addEventListener('abort', () => {
      clearTimeout(timer);
      reject(abortError());
    }, { once: true });
  });

/** 用 XHR 发一块（fetch 拿不到上传进度）。onProgress(已发送字节)。 */
function putChunk(uid, offset, blob, onProgress, signal) {
  const device = uploadDevice.get(uid);
  return new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(abortError());
    const xhr = new XMLHttpRequest();
    let stall;
    const arm = () => {
      clearTimeout(stall);
      stall = setTimeout(() => {
        xhr.abort();
        reject(new ApiError(0, 'CLIENT_UNREACHABLE'));
      }, STALL_MS);
    };
    const done = () => {
      clearTimeout(stall);
      signal?.removeEventListener('abort', onAbort);
    };
    const onAbort = () => {
      done();
      xhr.abort();
      reject(abortError());
    };
    signal?.addEventListener('abort', onAbort, { once: true });
    xhr.open('PUT', `/api/uploads/${encodeURIComponent(uid)}?offset=${offset}`);
    xhr.withCredentials = true;
    xhr.setRequestHeader('Content-Type', 'application/octet-stream');
    xhr.setRequestHeader('X-Looklook-Client', '1');
    if (device) xhr.setRequestHeader('X-LL-Device', device);
    xhr.setRequestHeader('Accept', 'application/json');
    xhr.upload.onprogress = (e) => {
      arm();
      onProgress?.(e.loaded);
    };
    xhr.onload = () => {
      done();
      let data = null;
      try {
        data = JSON.parse(xhr.responseText);
      } catch {
        /* 非 JSON（网关错误页等） */
      }
      if (xhr.status >= 200 && xhr.status < 300 && data) return resolve(data);
      const err = data?.error ?? {};
      reject(new ApiError(xhr.status, err.code ?? (xhr.status >= 500 ? 'CLIENT_UNREACHABLE' : 'INTERNAL'), err.params ?? {}));
    };
    xhr.onerror = () => {
      done();
      reject(new ApiError(0, 'CLIENT_UNREACHABLE'));
    };
    arm();
    xhr.send(blob);
  });
}

/** 与后端交互的部分，测试时可以替换。 */
// 多设备：上传 id 只在建立它的那台电脑上有效，后续分块、完成、取消都发给那台（docs/05 §6）。
const uploadDevice = new Map();
const uploadApi = (uid) => deviceApi(uploadDevice.get(uid));

export const httpTransport = {
  create: async (id, body) => {
    const res = await api.post(`/instances/${id}/uploads`, body);
    const device = terminalDevice(id);
    if (device && res?.id) uploadDevice.set(res.id, device);
    return res;
  },
  put: putChunk,
  finish: async (uid) => {
    const res = await uploadApi(uid).post(`/uploads/${encodeURIComponent(uid)}/finish`);
    uploadDevice.delete(uid);
    return res;
  },
  remove: (uid) => {
    const p = uploadApi(uid).del(`/uploads/${encodeURIComponent(uid)}`);
    uploadDevice.delete(uid);
    return p;
  },
};

/**
 * 传一个文件。task 是可变对象：读取 task.uid / task.offset 以便“重试”时接着传，进度写回 task.sent。
 * onChange() 在状态/进度变化时调用。返回最终路径。
 */
export async function runUpload(task, { instanceId, transport = httpTransport, signal, onChange = () => {}, wait = sleep }) {
  const file = task.file;
  let failures = 0;
  const retry = async (e) => {
    if (!isRetryable(e) || failures >= MAX_RETRIES) throw e;
    failures += 1;
    task.state = 'retrying';
    onChange();
    await wait(e.code === 'UPLOAD_BUSY' ? Math.max(3000, retryDelay(failures)) : retryDelay(failures), signal);
  };
  while (!task.uid) {
    try {
      const res = await transport.create(instanceId, { name: file.name ?? '', size: file.size, dest: task.dest });
      task.uid = res.id;
      task.path = res.path;
      task.chunk = res.chunk_size || 8 * 1024 * 1024;
      task.offset = 0;
    } catch (e) {
      if (signal?.aborted || e.name === 'AbortError') throw abortError();
      await retry(e);
    }
  }
  for (;;) {
    if (signal?.aborted) throw abortError();
    const range = nextChunk(task.offset, file.size, task.chunk);
    task.state = 'uploading';
    onChange();
    try {
      if (!range) {
        const res = await transport.finish(task.uid);
        task.path = res?.path ?? task.path;
        task.sent = file.size;
        return task.path;
      }
      const res = await transport.put(task.uid, range.start, file.slice(range.start, range.end), (n) => {
        task.sent = range.start + n;
        onChange();
      }, signal);
      task.offset = res.received;
      task.sent = res.received;
      failures = 0;
    } catch (e) {
      if (signal?.aborted || e.name === 'AbortError') throw abortError();
      // 服务端收到的字节数和我们以为的不一样（上一块其实已经到了、或者断在半截）：从服务端的位置接着传。
      if ((e.code === 'UPLOAD_OFFSET' || e.code === 'UPLOAD_INCOMPLETE') && Number.isFinite(e.params?.received)) {
        task.offset = e.params.received;
        task.sent = task.offset;
        continue;
      }
      await retry(e);
    }
  }
}

let seq = 0;

/** 一个终端的上传列表。snapshot 每次变化都换新数组，供 useSyncExternalStore 使用。 */
export class UploadStore {
  constructor(instanceId, transport = httpTransport) {
    this.instanceId = instanceId;
    this.transport = transport;
    this.tasks = [];
    this.snapshot = [];
    this.listeners = new Set();
    this.subscribe = this.subscribe.bind(this);
    this.getSnapshot = this.getSnapshot.bind(this);
  }

  subscribe(fn) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  getSnapshot() {
    return this.snapshot;
  }

  emit() {
    this.snapshot = this.tasks.map((t) => ({ key: t.key, name: t.name, size: t.size, sent: t.sent, state: t.state, path: t.path, error: t.error, dest: t.dest }));
    for (const fn of this.listeners) fn();
  }

  /** 加入一批文件；全部结束（成功/失败/取消）后调用 onDone(新完成的 task 列表, 这批全部 task)。返回这批的 key。 */
  add(files, { dest, onDone }) {
    const batch = { tasks: [], onDone, settled: false };
    for (const file of files) {
      const task = { key: `u${++seq}`, file, name: file.name || 'paste.png', size: file.size, sent: 0, state: 'queued', dest, batch, path: null, error: null };
      batch.tasks.push(task);
      this.tasks.push(task);
    }
    this.emit();
    this.pump();
    return batch.tasks.map((t) => t.key);
  }

  pump() {
    const running = this.tasks.filter((t) => t.state === 'uploading' || t.state === 'retrying').length;
    let free = CONCURRENCY - running;
    for (const task of this.tasks) {
      if (free <= 0) break;
      if (task.state !== 'queued') continue;
      free -= 1;
      this.start(task);
    }
  }

  start(task) {
    task.ctrl = new AbortController();
    task.state = 'uploading';
    task.error = null;
    this.emit();
    runUpload(task, { instanceId: this.instanceId, transport: this.transport, signal: task.ctrl.signal, onChange: () => this.emit() })
      .then(() => {
        task.state = 'done';
        task.file = null; // 释放内存（剪贴板截图等）
      })
      .catch((e) => {
        if (task.state === 'canceled') return;
        task.state = 'error';
        task.error = e;
      })
      .finally(() => {
        this.emit();
        this.settle(task.batch);
        this.pump();
      });
  }

  settle(batch) {
    if (batch.settled || batch.tasks.some((t) => !['done', 'error', 'canceled'].includes(t.state))) return;
    batch.settled = true;
    // 失败后手动重试成功的文件会再结算一次：只报告这次新完成的。
    const fresh = batch.tasks.filter((t) => t.state === 'done' && !t.reported);
    for (const t of fresh) t.reported = true;
    batch.onDone?.(fresh, batch.tasks);
  }

  find(key) {
    return this.tasks.find((t) => t.key === key);
  }

  cancel(key) {
    const task = this.find(key);
    if (!task || task.state === 'done' || task.state === 'canceled') return;
    task.state = 'canceled';
    task.ctrl?.abort();
    if (task.uid) this.transport.remove(task.uid).catch(() => {});
    task.file = null;
    this.emit();
    this.settle(task.batch);
    this.pump();
  }

  /** 失败的文件接着传（已经建好的上传会从服务端的进度继续）。 */
  retry(key) {
    const task = this.find(key);
    if (!task || task.state !== 'error' || !task.file) return;
    task.state = 'queued';
    task.batch.settled = false;
    this.emit();
    this.pump();
  }

  cancelWhere(pred) {
    for (const t of this.tasks) if (pred(t)) this.cancel(t.key);
  }

  clearFinished() {
    this.tasks = this.tasks.filter((t) => !['done', 'error', 'canceled'].includes(t.state));
    this.emit();
  }

  /** 进行中的数量与总体进度（0–1）。 */
  progress() {
    const active = this.tasks.filter((t) => ['queued', 'uploading', 'retrying'].includes(t.state));
    const total = active.reduce((s, t) => s + t.size, 0);
    const sent = active.reduce((s, t) => s + Math.min(t.sent, t.size), 0);
    return { active: active.length, ratio: total ? sent / total : 0 };
  }
}

const stores = new Map();

export function uploadStore(instanceId) {
  if (!stores.has(instanceId)) stores.set(instanceId, new UploadStore(instanceId));
  return stores.get(instanceId);
}
