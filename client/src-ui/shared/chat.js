// Claude Code 对话界面（docs/CLAUDE_UI.md §5）的纯逻辑：事件合并、工具卡片摘要、diff、哪些命令要到终端里操作。
// 不依赖 React 和浏览器，`node --test` 直接测。

/** 有对话界面的启动类型（和服务端 instances.rs 的 CHAT_LAUNCHES 一致） */
export const CHAT_LAUNCHES = ['claude', 'codex'];

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

/**
 * 输入 / 时列出的内置命令。`action`：界面自己处理、不送进 Claude Code 的——
 * `model` 打开模型菜单，`history` 打开“以前的对话”页，`usage` / `tasks` / `config` 打开右侧面板的那一栏。
 * 其余照原样送进去；会打开选择框的（/rewind 之类）由界面显示成弹窗。
 */
export const COMMANDS = [
  ['/compact'],
  ['/clear'],
  ['/model', 'model'],
  ['/resume', 'history'],
  ['/rewind'],
  ['/context', 'usage'],
  ['/cost', 'usage'],
  ['/usage', 'usage'],
  ['/status', 'usage'],
  ['/tasks', 'tasks'],
  ['/permissions', 'config'],
  ['/mcp', 'config'],
  ['/review'],
  ['/init'],
];

/** 只和终端外观有关的命令：不在菜单里列出（自己打出来照样送进去） */
export const TERMINAL_ONLY = ['/theme', '/vim', '/terminal-setup', '/statusline', '/ide'];

/** 送出的文字是不是界面自己处理的命令：返回 action，否则 null */
export function slashAction(text) {
  const s = String(text ?? '').trim();
  const cmd = s.split(/\s/)[0].toLowerCase();
  // 带参数的照常送（例如 /model sonnet）
  if (s !== cmd) return null;
  return COMMANDS.find(([c]) => c === cmd)?.[1] ?? null;
}

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

// ---------------- 同类工具调用合成一组 ----------------

// 同一类的连续调用合成一组（默认收起，点开看每一次）。看文件和搜索算一类（Claude Code 自己也这样合并显示）。
// 待办、提问、计划不合并：它们各自有专门的显示。
const TOOL_CLASS = {
  Read: 'explore',
  NotebookRead: 'explore',
  Grep: 'explore',
  Glob: 'explore',
  LS: 'explore',
  Edit: 'edit',
  MultiEdit: 'edit',
  Write: 'edit',
  NotebookEdit: 'edit',
  Bash: 'bash',
  BashOutput: 'bash',
  KillShell: 'bash',
  KillBash: 'bash',
  WebFetch: 'web',
  WebSearch: 'web',
  Task: 'agent',
  Agent: 'agent',
};
const NO_GROUP = new Set(['TodoWrite', 'AskUserQuestion', 'ExitPlanMode']);

/** 工具的类别（合并用）；不合并的返回 null。别的工具（MCP 等）按名字各成一类。 */
export function toolClass(name) {
  if (NO_GROUP.has(name)) return null;
  return TOOL_CLASS[name] ?? `tool:${name}`;
}

/**
 * 把条目展开成要显示的行：assistant 回复拆成文字、思考、工具调用，连续的同类工具调用合成一组。
 * 结果已经画在工具卡片里的 tool_result（`used` 里有它的调用）不单独成行，也不打断分组。
 * 行：{ k: 'item', item } | { k: 'text' | 'thinking' | 'unknown', block } | { k: 'tool', block } | { k: 'group', cls, calls: [block] }
 */
export function chatRows(items, used = toolUseIds(items)) {
  const rows = [];
  let group = null;
  const flush = () => {
    if (!group) return;
    rows.push(group.calls.length > 1 ? group : { k: 'tool', key: group.calls[0].id, block: group.calls[0] });
    group = null;
  };
  for (const item of items) {
    if (item.k === 'tool_result' && used.has(item.tool_use_id)) continue;
    if (item.k !== 'assistant') {
      flush();
      rows.push({ k: 'item', key: item.key, item });
      continue;
    }
    (item.blocks ?? []).forEach((b, n) => {
      const key = b.id ?? `${item.key}:${n}`;
      if (b.t === 'tool_use') {
        const cls = toolClass(b.name);
        if (cls && group?.cls === cls) {
          group.calls.push(b);
          return;
        }
        flush();
        if (cls) group = { k: 'group', key: `g:${b.id}`, cls, calls: [b] };
        else rows.push({ k: 'tool', key, block: b });
        return;
      }
      // 空白的思考不打断分组
      if (b.t === 'thinking' && !String(b.text ?? '').trim()) return;
      flush();
      rows.push({ k: b.t === 'text' || b.t === 'thinking' ? b.t : 'unknown', key, block: b });
    });
  }
  flush();
  return rows;
}

/** 一组里每种工具各调用了几次：[[名字, 次数]]，按第一次出现的顺序 */
export function toolCounts(calls) {
  const m = new Map();
  for (const c of calls) m.set(c.name, (m.get(c.name) ?? 0) + 1);
  return [...m];
}

// ---------------- 模型、思考强度、权限模式 ----------------

const cap = (s) => s.charAt(0).toUpperCase() + s.slice(1).toLowerCase();

/** 模型的显示名：`claude-opus-5-5` → `Opus 5.5`，`sonnet[1m]` → `Sonnet 1M`；认不出的原样返回 */
export function modelLabel(model) {
  if (!model) return '';
  const s = String(model).replace(/^claude-/i, '');
  const full = s.match(/^(opus|sonnet|haiku|fable)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?(\[1m\])?$/i);
  if (full) return `${cap(full[1])} ${full[2]}${full[3] ? `.${full[3]}` : ''}${full[4] ? ' 1M' : ''}`;
  const alias = s.match(/^(opus|sonnet|haiku|fable)(\[1m\])?$/i);
  if (alias) return `${cap(alias[1])}${alias[2] ? ' 1M' : ''}`;
  return s;
}

/** 模型的系列（opus / sonnet / haiku / fable），判断快捷选项里哪个是当前的 */
export function modelFamily(model) {
  return String(model ?? '').match(/opus|sonnet|haiku|fable/i)?.[0].toLowerCase() ?? '';
}

/** 模型 ID 去掉日期和上下文后缀，用来比较：`claude-haiku-4-5-20251001[1m]` → `claude-haiku-4-5` */
export function baseModel(id) {
  const s = String(id ?? '').trim().toLowerCase().split('[')[0];
  return s.replace(/-\d{8}$/, '');
}

const ALIASES = ['opus', 'sonnet', 'haiku', 'fable'];

/** 选项是否就是这个模型（选项值、对应的模型 ID、或者别名对上家族） */
function optionIs(o, model) {
  const m = String(model ?? '').toLowerCase();
  if (!m) return false;
  if (o.value.toLowerCase() === m) return true;
  if (ALIASES.includes(m)) return o.value === m || (!o.model && modelFamily(o.value) === m);
  return Boolean(o.model) && baseModel(o.model) === baseModel(m) && o.value !== 'default';
}

/**
 * 模型按钮和菜单要显示的：
 * - checked：菜单里勾选哪一项的 value；
 * - label：按钮上的名字（最近一条回复实际用的模型；刚在界面上选了、还没有新回复时显示选的）；
 * - pending：选了还没生效；
 * - mismatch：在界面上选的模型和回复实际用的对不上（例如服务商把请求转给了别的模型），值是实际模型的名字。
 * status：对话状态（model、actual_model、model_pending、model_chosen）；options：/chat/models 的选项。
 */
export function modelView(status, options = []) {
  const sel = status?.model ?? null;
  const act = status?.actual_model ?? null;
  const pending = Boolean(status?.model_pending);
  const name = (m) => options.find((o) => o.value !== 'default' && optionIs(o, m))?.label ?? modelLabel(m);
  // 选的那项指向的就是回复用的模型（或者还没有回复）：勾选的那项；否则（在终端里切过）按回复用的模型找
  const selOpt = sel ? options.find((o) => o.value === sel) ?? options.find((o) => optionIs(o, sel)) : null;
  const selIsAct = selOpt && act && (selOpt.model ? baseModel(selOpt.model) === baseModel(act) : modelFamily(selOpt.value) === modelFamily(act));
  let checked = null;
  if (selOpt && (pending || !act || selIsAct)) checked = selOpt.value;
  else if (act) checked = options.find((o) => optionIs(o, act))?.value ?? null;
  const label = pending || !act ? (sel === 'default' ? options.find((o) => o.value === 'default')?.description || 'Default' : sel ? name(sel) : '') : name(act);
  let mismatch = null;
  if (status?.model_chosen && !pending && sel && act && sel !== 'default') {
    const o = options.find((x) => x.value === sel);
    const want = o?.model || sel;
    const same = ALIASES.includes(want.toLowerCase()) ? modelFamily(act) === want.toLowerCase() : baseModel(want) === baseModel(act);
    if (!same) mismatch = name(act);
  }
  return { checked, label, pending, mismatch };
}

/** token 数的简短写法：`950`、`12.3k`、`1.2M` */
export function tokens(n) {
  const v = Number(n) || 0;
  if (v < 1000) return String(v);
  if (v < 1e6) return `${(v / 1000).toFixed(v < 1e4 ? 1 : 0)}k`;
  return `${(v / 1e6).toFixed(1)}M`;
}

export const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];
/** Shift+Tab 能轮换到的权限模式（自动模式、全部权限要启动时开启才有） */
export const MODE_CHOICES = ['default', 'acceptEdits', 'plan', 'auto', 'bypassPermissions'];

/** 状态栏上的后台任务个数：{ shell: 1, subagent: 2 } */
export function backgroundCounts(status) {
  const out = {};
  for (const b of status?.background ?? []) out[b.kind] = (out[b.kind] ?? 0) + b.n;
  // 子代理：hooks 记的个数（前台的也算）和状态栏上的（只算后台的）取大的
  const agents = Math.max(out.subagent ?? 0, out.agent ?? 0, status?.agents ?? 0);
  delete out.agent;
  if (agents) out.subagent = agents;
  else delete out.subagent;
  return out;
}

/** 时长的简短写法：`45s`、`3m12s`、`1h05m` */
export function duration(ms) {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m${String(s % 60).padStart(2, '0')}s`;
  return `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}m`;
}
