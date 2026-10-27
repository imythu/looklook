// 对话输入框的输入历史：上下键翻、历史按钮里搜（像命令行的 Ctrl+R）。
// Claude Code 用它自己的 history.jsonl（GET chat/prompts，和终端里按上键翻的是同一份）；
// 别的工具用本机浏览器里存着的发过的话（所有会话共用）。都再加上当前对话里你说过的话。

const KEY = 'll-chat-inputs';
const MAX = 300;

/** 本机存着的，最新的在前：[{ text, at }] */
export function readInputs() {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) || '[]');
    return Array.isArray(v) ? v.filter((x) => x && typeof x.text === 'string') : [];
  } catch {
    return [];
  }
}

/** 发出去一句：放到最前面（同样的话只留一条） */
export function rememberInput(text) {
  const s = String(text ?? '').trim();
  if (!s) return;
  try {
    const list = [{ text: s, at: Date.now() }, ...readInputs().filter((x) => x.text !== s)].slice(0, MAX);
    localStorage.setItem(KEY, JSON.stringify(list));
  } catch {
    /* 存不了：只是少了历史 */
  }
}

/**
 * 翻历史用的列表，最新的在前：当前对话里说过的（倒着）在前，然后是 `saved`（[{ text, at, session? }]，最新的在前）；
 * 同样的话只留一条。`items` 是对话条目（用户消息 `k === 'user'`）；`session` 是当前对话的会话号。
 * 返回 [{ text, at?, here }]，`here` 表示是这段对话里的。
 */
export function inputHistory(items, saved = readInputs(), session = null) {
  const seen = new Set();
  const out = [];
  const add = (text, at, here) => {
    const s = String(text ?? '').trim();
    if (!s || seen.has(s)) return;
    seen.add(s);
    out.push({ text: s, at, here });
  };
  const mine = (items ?? []).filter((x) => x?.k === 'user');
  for (let i = mine.length - 1; i >= 0; i--) add(mine[i].text, null, true);
  for (const x of saved) add(x.text, x.at, Boolean(session && x.session === session));
  return out;
}

/** 搜索：每个词都要出现（不分大小写） */
export function searchInputs(list, q) {
  const words = String(q ?? '').toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return list;
  return list.filter((x) => {
    const s = x.text.toLowerCase();
    return words.every((w) => s.includes(w));
  });
}
