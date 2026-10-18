// Claude Code 对话界面（docs/CLAUDE_UI.md §5）的纯逻辑：事件合并、工具卡片摘要、diff、哪些命令要到终端里操作。
// 不依赖 React 和浏览器，`node --test` 直接测。

/** 有对话界面的启动类型（和服务端 instances.rs 的 CHAT_LAUNCHES 一致；Codex 接好后加 'codex'） */
export const CHAT_LAUNCHES = ['claude'];

/** 空的对话状态 */
export const EMPTY = { status: null, items: [], start: 0, total: 0 };

/**
 * 合并服务端事件（SSE）：
 * - snapshot：{ status, items, start, total } 整个替换；
 * - status：只换状态；
 * - items：{ start, items }，从全局第 start 条起新增或更新（同一条 assistant 回复会多次更新）。
 */
export function applyEvent(state, type, data) {
  switch (type) {
    case 'snapshot':
      return { status: data.status ?? null, items: data.items ?? [], start: data.start ?? 0, total: data.total ?? 0 };
    case 'status':
      return { ...state, status: data };
    case 'items': {
      const items = state.items.slice();
      (data.items ?? []).forEach((item, i) => {
        const p = data.start + i - state.start;
        if (p < 0) return;
        if (p < items.length) items[p] = item;
        else items.push(item);
      });
      return { ...state, items, total: Math.max(state.total, state.start + items.length) };
    }
    case 'older': {
      // 往上翻取到的更早的条目
      const older = data.items ?? [];
      return { ...state, items: [...older, ...state.items], start: data.start ?? state.start - older.length };
    }
    default:
      return state;
  }
}

/** tool_use_id → tool_result 条目 */
export function resultsById(items) {
  const m = new Map();
  for (const i of items) if (i.k === 'tool_result') m.set(i.tool_use_id, i);
  return m;
}

/** 列表里出现过的 tool_use id（对应的结果画在卡片里，不单独显示） */
export function toolUseIds(items) {
  const s = new Set();
  for (const i of items) if (i.k === 'assistant') for (const b of i.blocks ?? []) if (b.t === 'tool_use') s.add(b.id);
  return s;
}

const short = (s, n = 120) => {
  const one = String(s ?? '').replace(/\s+/g, ' ').trim();
  return one.length > n ? `${one.slice(0, n - 1)}…` : one;
};
const tail = (p) => String(p ?? '').split(/[\\/]/).filter(Boolean).slice(-2).join('/');

/** 卡片标题后面那一小段：挑最能说明这次调用的参数。认不出的工具取第一个字符串参数。 */
export function toolSummary(name, input = {}) {
  const i = input ?? {};
  switch (name) {
    case 'Bash':
      return short(i.command);
    case 'Read':
    case 'Edit':
    case 'MultiEdit':
    case 'Write':
    case 'NotebookEdit':
      return tail(i.file_path ?? i.notebook_path);
    case 'Grep':
      return short(`${i.pattern ?? ''}${i.path ? ` · ${tail(i.path)}` : ''}`);
    case 'Glob':
      return short(i.pattern);
    case 'WebFetch':
      return short(i.url);
    case 'WebSearch':
      return short(i.query);
    case 'Task':
    case 'Agent':
      return short(i.description ?? i.prompt);
    case 'TodoWrite':
      return `${(i.todos ?? []).filter((x) => x.status === 'completed').length}/${(i.todos ?? []).length}`;
    default: {
      const first = Object.values(i).find((v) => typeof v === 'string' && v.trim());
      return first ? short(first) : '';
    }
  }
}

/**
 * 改动的 diff 行：[{ t: '+' | '-' | ' ' | '@', text }]。
 * 优先用 Claude Code 给的 structuredPatch（toolUseResult），没有就用 old_string / new_string 自己算。
 */
export function diffLines(name, input = {}, extra = null) {
  const patch = extra?.structuredPatch;
  if (Array.isArray(patch) && patch.length) {
    const out = [];
    for (const h of patch) {
      out.push({ t: '@', text: `@@ -${h.oldStart},${h.oldLines} +${h.newStart},${h.newLines} @@` });
      for (const l of h.lines ?? []) {
        const c = l[0];
        out.push({ t: c === '+' || c === '-' ? c : ' ', text: l.slice(1) });
      }
    }
    return out;
  }
  if (name === 'Edit') return lineDiff(input.old_string ?? '', input.new_string ?? '');
  if (name === 'MultiEdit') return (input.edits ?? []).flatMap((e, n) => [...(n ? [{ t: '@', text: '…' }] : []), ...lineDiff(e.old_string ?? '', e.new_string ?? '')]);
  if (name === 'Write') return String(input.content ?? '').split('\n').map((text) => ({ t: '+', text }));
  return [];
}

/** 行级 diff（最长公共子序列）。太大时不细算，整段删、整段加。 */
export function lineDiff(a, b) {
  const x = String(a).split('\n');
  const y = String(b).split('\n');
  if (x.length * y.length > 250_000) return [...x.map((text) => ({ t: '-', text })), ...y.map((text) => ({ t: '+', text }))];
  const n = x.length;
  const m = y.length;
  const dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
  for (let i = n - 1; i >= 0; i--) for (let j = m - 1; j >= 0; j--) dp[i][j] = x[i] === y[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const out = [];
  let i = 0;
  let j = 0;
  while (i < n && j < m) {
    if (x[i] === y[j]) {
      out.push({ t: ' ', text: x[i] });
      i++;
      j++;
    } else if (dp[i + 1][j] >= dp[i][j + 1]) out.push({ t: '-', text: x[i++] });
    else out.push({ t: '+', text: y[j++] });
  }
  while (i < n) out.push({ t: '-', text: x[i++] });
  while (j < m) out.push({ t: '+', text: y[j++] });
  return out;
}

/** 增删行数 */
export function diffStat(lines) {
  let add = 0;
  let del = 0;
  for (const l of lines) {
    if (l.t === '+') add++;
    else if (l.t === '-') del++;
  }
  return { add, del };
}

/** 最新的一份待办（TodoWrite），没有时为 null */
export function latestTodos(items) {
  for (let i = items.length - 1; i >= 0; i--) {
    const it = items[i];
    if (it.k !== 'assistant') continue;
    for (let j = (it.blocks ?? []).length - 1; j >= 0; j--) {
      const b = it.blocks[j];
      if (b.t === 'tool_use' && b.name === 'TodoWrite' && Array.isArray(b.input?.todos)) return b.input.todos;
    }
  }
  return null;
}

// 会打开 TUI 交互界面（选择器、设置、登录……）的斜杠命令：送出后自动展开终端。
// 只影响“要不要自动展开终端”，命令照样能用；清单过时也不会坏事（另有抓屏检测）。
export const TUI_COMMANDS = ['/model', '/config', '/settings', '/login', '/logout', '/resume', '/permissions', '/mcp', '/agents', '/hooks', '/memory', '/rewind', '/status', '/doctor', '/plugin', '/plugins', '/theme', '/output-style', '/vim', '/terminal-setup', '/ide', '/install-github-app', '/export', '/add-dir', '/bashes', '/tasks', '/usage', '/upgrade', '/statusline'];

export function opensTui(text) {
  const cmd = String(text ?? '').trim().split(/\s/)[0].toLowerCase();
  return TUI_COMMANDS.includes(cmd);
}

/** 内置的常用命令（输入 / 时提示）；tui：会打开终端 */
export const COMMANDS = [
  ['/compact', false],
  ['/clear', false],
  ['/cost', false],
  ['/context', false],
  ['/review', false],
  ['/init', false],
  ['/model', true],
  ['/resume', true],
  ['/rewind', true],
  ['/permissions', true],
  ['/mcp', true],
  ['/config', true],
  ['/login', true],
];

/** AskUserQuestion 的答案：{ 问题原文: 选项 label }；多选用 ", " 连起来，自己填的文字直接作为答案。 */
export function questionAnswers(questions, picked) {
  const out = {};
  (questions ?? []).forEach((q, n) => {
    const p = picked[n] ?? {};
    const labels = [...(p.labels ?? [])];
    if (p.other?.trim()) labels.push(p.other.trim());
    if (labels.length) out[q.question] = labels.join(', ');
  });
  return out;
}

/** 内容里的文字（工具结果的完整原始记录里取） */
export function contentText(content) {
  if (typeof content === 'string') return content;
  if (Array.isArray(content)) return content.filter((b) => b?.type === 'text').map((b) => b.text).join('\n');
  return '';
}
