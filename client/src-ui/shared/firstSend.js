// 新会话的第一句话（草稿页 Draft.jsx 发出）：会话先建好、马上打开，不等 Claude Code / Codex / OpenCode 起来；
// 等对话空闲了，再走对话输入框的同一个接口发出去（图片也是那时才传到附件目录，上传要会话号）。
// 等的时候对话页先显示这句话和“正在启动…”（ChatView.jsx 的 FirstSend）；没起来时字留着，可以重试或回到草稿。
// 只存在这个页面里：刷新页面就没了（会话照样建好了，只是第一句没发）。
import { useSyncExternalStore } from 'react';

import { api, terminalDevice } from './api.js';
import { runUpload } from './uploader.js';

// 多久还没起来算失败（在等用户选择框时不算）
const WAIT_MS = 180000;
// 对话说已经不会起来了（已经退出、终端没在运行）：连着几次才算，刚建好时可能短暂这样
const GIVE_UP_POLLS = 3;
// 发出去以后对话记录里会有这条：再留一会儿，免得记录到之前空一下
const KEEP_SENT_MS = 15000;
const WAITING_REASONS = ['starting', 'adopting', 'switching'];

const items = new Map();
const subs = new Set();
const emit = () => subs.forEach((f) => f());
const set = (id, patch) => {
  const x = items.get(id);
  if (!x) return;
  items.set(id, { ...x, ...patch });
  emit();
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function subscribe(f) {
  subs.add(f);
  return () => subs.delete(f);
}

/** 这个会话等着发的第一句：{ text, files: [{ file, url }], state: 'waiting' | 'sending' | 'sent' | 'failed', fail }，没有时 null。 */
export function useFirstSend(id) {
  return useSyncExternalStore(subscribe, () => items.get(id) ?? null);
}

/** 排上第一句（files 是 File），马上开始等会话起来。 */
export function queueFirst(id, text, files = []) {
  items.set(id, { text, files: files.map((file) => ({ file, url: URL.createObjectURL(file) })), state: 'waiting', fail: null });
  emit();
  deliver(id);
}

/** 不发了（回到草稿、放进输入框）：返回原来的字和图片。 */
export function dropFirst(id) {
  const x = items.get(id);
  items.delete(id);
  emit();
  x?.files.forEach((f) => URL.revokeObjectURL(f.url));
  return x;
}

/** 重试：会话停了就先启动，再接着等。 */
export async function retryFirst(id) {
  const x = items.get(id);
  if (!x || x.state !== 'failed') return;
  set(id, { state: 'waiting', fail: null });
  if (x.fail?.kind !== 'send') {
    const inst = await api.get(`/instances/${id}`).catch(() => null);
    if (inst && !inst.running) {
      try {
        await api.post(`/instances/${id}/start`);
      } catch (e) {
        set(id, { state: 'failed', fail: { kind: 'send', error: e } });
        return;
      }
    }
  }
  deliver(id);
}

async function deliver(id) {
  const until = Date.now() + WAIT_MS;
  let gone = 0;
  for (;;) {
    if (items.get(id)?.state !== 'waiting') return;
    const [snap, inst] = await Promise.all([api.get(`/instances/${id}/chat`).catch(() => null), api.get(`/instances/${id}`).catch(() => null)]);
    if (items.get(id)?.state !== 'waiting') return;
    const st = snap?.status;
    if (st?.phase === 'idle') break;
    if (inst && !inst.running && inst.error) {
      set(id, { state: 'failed', fail: { kind: 'instance', code: inst.error } });
      return;
    }
    gone = st?.phase === 'detached' && !WAITING_REASONS.includes(st.reason ?? 'starting') ? gone + 1 : 0;
    if (gone >= GIVE_UP_POLLS) {
      set(id, { state: 'failed', fail: { kind: 'detached', reason: st.reason } });
      return;
    }
    // 在等用户选（信任这个文件夹等）：不算超时
    if (st?.phase !== 'tui' && st?.phase !== 'approval' && Date.now() > until) {
      set(id, { state: 'failed', fail: { kind: 'timeout' } });
      return;
    }
    await sleep(1000);
  }
  const x = items.get(id);
  set(id, { state: 'sending' });
  try {
    const images = [];
    for (const f of x.files) images.push(await runUpload({ file: f.file, dest: 'attach' }, { instanceId: id }));
    const r = await api.post(`/instances/${id}/chat/input`, { text: x.text, images });
    if (r?.delivered === false) throw new Error('not_delivered');
    set(id, { state: 'sent' });
    setTimeout(() => items.get(id)?.state === 'sent' && dropFirst(id), KEEP_SENT_MS);
  } catch (e) {
    set(id, { state: 'failed', fail: { kind: 'send', error: e } });
  }
}

// 回到草稿（没起来时“换个类型”）：带着原来的字、图片、项目，草稿页打开时取走
let back = null;

/** 草稿页打开时要放回去的（没有时 null）；用过后 clearDraftBack。分两步是因为严格模式下初始化会跑两次。 */
export const draftBack = () => back;
export function clearDraftBack() {
  back = null;
}

/** 不在这个会话里发了：字和图片交回草稿页，没起来的会话（没用过）删掉，免得列表里多一个。 */
export async function backToDraft(id, inst) {
  const x = dropFirst(id);
  back = {
    device: terminalDevice(id),
    workdir: inst?.container ? '' : inst?.workdir ?? '',
    container: inst?.container ?? null,
    launch: inst?.launch,
    text: x?.text ?? '',
    files: x?.files.map((f) => f.file) ?? [],
  };
  await api.del(`/instances/${id}`).catch(() => {});
}
