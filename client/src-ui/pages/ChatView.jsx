// Claude Code 对话界面（docs/CLAUDE_UI.md §5）：同一个 tmux 会话里的交互式 Claude Code 的第二个视图，参照 Claude 桌面端。
// 内容来自 transcript，状态来自 hooks（服务端经 SSE 推过来）；输入、停止、审批都送回同一个进程。
// 对话页里不出现终端：模型、思考强度、权限模式在输入框上切换；Claude Code 自己的选择框读出来显示成弹窗；
// 子代理、后台命令、改动、用量在右侧面板里。只有用户自己切到“终端”标签才看到终端。
// 部分交互参考 CloudCLI UI (https://github.com/siteboon/claudecodeui)，AGPL-3.0-or-later：
// 命令卡片的标题用说明、命令放在下面一行、显示输出行数，参照它的 src/modules/chat/tools/BashCommandDisplay.tsx；
// 子代理步骤的时间线和“显示更早的步骤”参照 src/modules/chat/tools/SubagentTimeline.tsx（都是 7238368），
// 按本仓库的写法重写（2026-10-24）。
import {
  Activity,
  ArrowDown,
  ArrowRight,
  ArrowLeft,
  ArrowUp,
  Asterisk,
  Bot,
  Brain,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  CircleDashed,
  CircleStop,
  CornerDownRight,
  ExternalLink,
  FilePen,
  FileText,
  GitBranch,
  Globe,
  Hourglass,
  Image as ImageIcon,
  Layers,
  ListTodo,
  LoaderCircle,
  MessageCircleQuestion,
  PanelRight,
  Plug,
  Plus,
  Search,
  ShieldCheck,
  Sparkle,
  Square,
  SquareTerminal,
  TriangleAlert,
  WifiOff,
  Wrench,
  X,
  Zap,
} from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { api, fetchRaw, plainLinksWork } from '../shared/api.js';
import { spinnerText } from '../shared/spinner.js';
import {
  CODEX_COMMANDS,
  COMMANDS,
  EFFORTS,
  EMPTY,
  MODE_CHOICES,
  TERMINAL_ONLY,
  applyEvent,
  backgroundCounts,
  chatRows,
  conversationResources,
  contentText,
  diffLines,
  diffStat,
  duration,
  EDIT_TOOLS,
  fieldKind,
  KNOWN_DIALOGS,
  knownDialog,
  latestTodos,
  modelLabel,
  modelView,
  parseJson,
  questionAnswers,
  resultsById,
  slashAction,
  splitLinks,
  tokens,
  toolCounts,
  toolSummary,
  toolUseIds,
} from '../shared/chat.js';
import { errorText } from '../shared/i18n.js';
import Markdown from '../shared/Markdown.jsx';
import { useRouter } from '../shared/router.jsx';
import { formatTime, parseInstant, spanText } from '../shared/time.js';
import { runUpload } from '../shared/uploader.js';
import { Button, CopyButton, useConfirm, useToast } from '../shared/ui.jsx';

// ---------------- 数据 ----------------

/** 服务端状态里的 `agent` → 文案里的 {{agent}}；老版本服务端不带 agent，就是 Claude Code */
const AGENT_NAMES = { claude: 'Claude Code', codex: 'Codex' };
/** 这个对话的 AI 工具和它支持的操作（`status.caps`；老版本服务端不带 caps，就是全部可用）：界面只按它显示按钮 */
const AgentCtx = createContext({ id: 'claude', name: AGENT_NAMES.claude, caps: {} });
/** 这段对话产出的文件（Codex）：list 给资源栏，links（文字里的写法 → 地址）给回复里的路径变成链接 */
const ArtifactCtx = createContext({ list: [], links: null });

/** 产出的文件的地址：图片、PDF、音视频、文本直接在浏览器里看，其余下载 */
const artifactUrl = (id, path, download) => `/api/instances/${id}/chat/artifact?path=${encodeURIComponent(path)}${download ? '&download=1' : ''}`;

/** 和 useTranslation 一样，但文案里的 {{agent}} 换成这个对话的 AI 工具（`name` 给了就用它，否则看上层的 AgentCtx） */
function useT(name) {
  const { t, i18n } = useTranslation();
  const ctx = useContext(AgentCtx);
  const agent = name ?? ctx.name;
  return { t: useCallback((key, opts) => t(key, { agent, ...opts }), [t, agent]), i18n };
}

/** Codex（app-server）：`thinking` → 正在思考；其余是正在运行的命令，原样显示 */
function activityText(t, activity) {
  if (activity === 'compacting') return t('chat.compacting');
  return activity === 'thinking' ? t('chat.codex_thinking') : activity;
}

/** 订阅对话事件。连不上时 EventSource 自己重连；服务端拒绝（4xx）时它会放弃，这里隔几秒再试，并取一次快照拿到错误。 */
function useChat(id) {
  const [state, dispatch] = useReducer((s, a) => applyEvent(s, a.type, a.data), EMPTY);
  const [online, setOnline] = useState(false);
  const [error, setError] = useState(null);
  useEffect(() => {
    let es = null;
    let retry = null;
    let alive = true;
    const snapshot = () =>
      api.get(`/instances/${id}/chat`).then(
        (data) => {
          if (!alive) return;
          setError(null);
          dispatch({ type: 'snapshot', data });
        },
        (e) => alive && setError(e),
      );
    const connect = () => {
      es = new EventSource(`/api/instances/${encodeURIComponent(id)}/chat/events`, { withCredentials: true });
      const on = (type) => (e) => {
        try {
          dispatch({ type, data: JSON.parse(e.data) });
        } catch {
          /* 坏数据：等下一个快照 */
        }
      };
      es.addEventListener('snapshot', (e) => {
        setOnline(true);
        setError(null);
        on('snapshot')(e);
      });
      es.addEventListener('status', on('status'));
      es.addEventListener('items', on('items'));
      es.addEventListener('reset', () => snapshot());
      es.onerror = () => {
        setOnline(false);
        if (es.readyState === EventSource.CLOSED) {
          es.close();
          snapshot();
          retry = setTimeout(() => alive && connect(), 5000);
        }
      };
    };
    connect();
    return () => {
      alive = false;
      clearTimeout(retry);
      es?.close();
    };
  }, [id]);
  const loadOlder = useCallback(async () => {
    if (state.start <= 0) return;
    const data = await api.get(`/instances/${id}/chat/history?before=${state.start}&limit=200`);
    dispatch({ type: 'older', data });
  }, [id, state.start]);
  return { ...state, online, error, loadOlder };
}

// ---------------- 条目 ----------------

/** 工具的种类：卡片、时间线节点按种类上色 */
const TOOL_KINDS = { Bash: 'run', BashOutput: 'run', KillShell: 'run', Read: 'read', Edit: 'edit', MultiEdit: 'edit', Write: 'edit', NotebookEdit: 'edit', Grep: 'search', Glob: 'search', WebFetch: 'web', WebSearch: 'web', Task: 'agent', Agent: 'agent', TodoWrite: 'todo', ImageGen: 'image' };
const toolKind = (name) => TOOL_KINDS[name] ?? (name.startsWith('mcp__') ? 'mcp' : 'other');
const TOOL_ICONS = { ImageGen: ImageIcon, Bash: SquareTerminal, Read: FileText, Edit: FilePen, MultiEdit: FilePen, Write: FilePen, NotebookEdit: FilePen, Grep: Search, Glob: Search, WebFetch: Globe, WebSearch: Globe, TodoWrite: ListTodo, Task: Bot, Agent: Bot };
const DIFF_TOOLS = ['Edit', 'MultiEdit', 'Write'];
const DIFF_LIMIT = 400;

function Pre({ text, className = '' }) {
  if (!text) return null;
  return <pre className={`chat-pre ${className}`}>{text}</pre>;
}

/** 文字里的网址可以点（工具结果里常有：发布出来的页面、创建的文档……） */
function Linked({ text }) {
  return splitLinks(text).map((p, n) =>
    p.url ? (
      // eslint-disable-next-line react/no-array-index-key -- 片段没有别的标识
      <a key={n} href={p.url} target="_blank" rel="noopener noreferrer">
        {p.text}
      </a>
    ) : (
      p.text
    ),
  );
}

/** 格式化好的 JSON，右上角可以复制（复制的是完整的原文） */
function JsonBlock({ value, raw, className = '' }) {
  const text = JSON.stringify(value, null, 2);
  return (
    <div className="chat-json">
      <pre className={`chat-pre ${className}`}>
        <Linked text={text} />
      </pre>
      <span className="chat-json-copy">
        <CopyButton text={raw ?? text} label={false} variant="plain" />
      </span>
    </div>
  );
}

/** 工具结果、参数里的一段文字：整段是 JSON 时格式化显示，网址可以点 */
function RichText({ text, className = '' }) {
  if (!text) return null;
  const json = parseJson(text);
  if (json) return <JsonBlock value={json} raw={text} className={className} />;
  return (
    <pre className={`chat-pre ${className}`}>
      <Linked text={text} />
    </pre>
  );
}

/**
 * 认不出的工具（MCP、Artifact、文档、浏览器……）的参数：一项一行。短的值跟在名字后面，
 * 长文字、多行文字、JSON 单独成块（JSON 格式化），网址可以点。
 */
function FieldList({ input }) {
  const entries = Object.entries(input ?? {});
  if (!entries.length) return null;
  return (
    <dl className="chat-fields">
      {entries.map(([k, v]) => {
        const kind = fieldKind(v);
        return (
          <div key={k} className={`chat-field-row ${kind === 'json' || kind === 'block' ? 'wide' : ''}`}>
            <dt className="mono">{k}</dt>
            <dd>
              {kind === 'json' ? (
                <JsonBlock value={typeof v === 'string' ? parseJson(v) : v} raw={typeof v === 'string' ? v : undefined} />
              ) : kind === 'block' ? (
                <RichText text={v} />
              ) : kind === 'link' ? (
                <a href={v.trim()} target="_blank" rel="noopener noreferrer" className="mono">
                  {v.trim()}
                </a>
              ) : (
                <code>{typeof v === 'string' ? v : JSON.stringify(v)}</code>
              )}
            </dd>
          </div>
        );
      })}
    </dl>
  );
}

/** 参数被截断（太长）时：按所在的条目取完整的参数 */
function useFullInput(id, block) {
  const [full, setFull] = useState(null);
  const [busy, setBusy] = useState(false);
  const load = async () => {
    setBusy(true);
    try {
      const raw = await api.get(`/instances/${id}/chat/item?key=${encodeURIComponent(block.item)}`);
      const b = (raw?.message?.content ?? []).find?.((x) => x?.type === 'tool_use' && x.id === block.id);
      if (b?.input && typeof b.input === 'object') setFull(b.input);
    } finally {
      setBusy(false);
    }
  };
  return { input: full ?? block.input ?? {}, truncated: Boolean(block.truncated && !full), canLoad: Boolean(block.item), busy, load };
}

/** 增删行数，后面一条五格的比例条（像 GitHub 那样，一眼看出是加得多还是删得多） */
function DiffStat({ stat }) {
  const total = stat.add + stat.del;
  const green = total ? Math.round((stat.add / total) * 5) : 0;
  return (
    <span className="chat-stat">
      <i className="add">+{stat.add}</i> <i className="del">−{stat.del}</i>
      <span className="chat-stat-bar" aria-hidden="true">
        {[0, 1, 2, 3, 4].map((n) => (
          <b key={n} data-k={n < green ? 'add' : total ? 'del' : ''} />
        ))}
      </span>
    </span>
  );
}

function DiffView({ lines }) {
  const { t } = useT();
  const [all, setAll] = useState(false);
  const shown = all ? lines : lines.slice(0, DIFF_LIMIT);
  // 有行号时（Write、带 structuredPatch 的改动）左边加一栏行号
  const numbered = lines.some((l) => l.no != null);
  return (
    <div className="chat-diff">
      {shown.map((l, n) => (
        // eslint-disable-next-line react/no-array-index-key -- 行没有别的标识
        <div key={n} className={`chat-diff-line d${l.t === '+' ? 'add' : l.t === '-' ? 'del' : l.t === '@' ? 'hunk' : 'ctx'}`}>
          {numbered && <span className="chat-diff-no" aria-hidden="true">{l.no ?? ''}</span>}
          <span aria-hidden="true">{l.t === '@' ? '' : l.t}</span>
          {l.text || ' '}
        </div>
      ))}
      {!all && lines.length > DIFF_LIMIT && (
        <button type="button" className="chat-more" onClick={() => setAll(true)}>
          {t('chat.show_all_lines', { n: lines.length })}
        </button>
      )}
    </div>
  );
}

function TodoList({ todos }) {
  return (
    <ul className="chat-todos">
      {todos.map((x, n) => (
        // eslint-disable-next-line react/no-array-index-key -- 待办没有标识
        <li key={n} data-status={x.status}>
          <span className="chat-todo-box" aria-hidden="true">
            {x.status === 'completed' ? <Check size={12} /> : x.status === 'in_progress' ? <LoaderCircle size={12} /> : null}
          </span>
          <span>{x.status === 'in_progress' ? x.activeForm || x.content : x.content}</span>
        </li>
      ))}
    </ul>
  );
}

/** 工具结果的文字；被截断时可以取完整内容。 */
function ResultText({ id, result }) {
  const { t } = useT();
  const [full, setFull] = useState(null);
  const [busy, setBusy] = useState(false);
  if (!result) return null;
  const more = async () => {
    setBusy(true);
    try {
      const raw = await api.get(`/instances/${id}/chat/item?key=${encodeURIComponent(result.key)}`);
      const block = (raw?.message?.content ?? []).find?.((b) => b.tool_use_id === result.tool_use_id);
      setFull(contentText(block?.content) || result.content);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <RichText text={full ?? result.content} className={result.is_error ? 'err' : ''} />
      {result.images > 0 && <p className="chat-note">{t('chat.images', { n: result.images })}</p>}
      {result.truncated && full == null && (
        <button type="button" className="chat-more" onClick={more} disabled={busy}>
          {t('chat.load_full')}
        </button>
      )}
    </>
  );
}

/** 子代理的最后一条是纯文字回复：结束了（后台运行的子代理一开始就有结果“已在后台启动”，不能看结果） */
const agentDone = (items) => {
  const end = items?.[items.length - 1];
  return Boolean(end?.k === 'assistant' && end.blocks?.some((b) => b.t === 'text') && !end.blocks?.some((b) => b.t === 'tool_use'));
};

/**
 * 子代理自己的步骤（它的 transcript 单独存放）：`enabled` 时加载，没结束时每 2 秒刷新。
 * agent：agentId（工具结果或子代理列表里有）；tool：启动它的工具调用 id（还没有结果时按 prompt 找）。
 */
function useAgentSteps(id, { agent, tool, enabled }) {
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  useEffect(() => {
    if (!enabled) return undefined;
    let alive = true;
    let timer = null;
    const load = async () => {
      try {
        const d = await api.get(`/instances/${id}/chat/agent?tool=${encodeURIComponent(tool ?? '')}&agent=${encodeURIComponent(agent ?? '')}`);
        if (!alive) return;
        setData(d);
        setError(null);
        if (!agentDone(d.items)) timer = setTimeout(load, 2000);
      } catch (e) {
        if (!alive) return;
        setError(e);
        timer = setTimeout(load, 3000);
      }
    };
    load();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [enabled, agent, tool]);
  const items = data?.items ?? [];
  return { data, items, error, done: agentDone(items) };
}

/** 子代理的步骤一次先画这么多（每一步都可能展开成 diff），更早的点了再显示 */
const SUB_STEPS = 25;

/**
 * 工具卡片里子代理的步骤：卡片展开时就加载，左边一条竖线串起来；步骤多时只画最近的，更早的点了再显示。
 * 时间线和“再显示更多”参照 CloudCLI UI 的 SubagentTimeline.tsx。
 */
function SubAgent({ id, block, result }) {
  const { t } = useT();
  const [open, setOpen] = useState(true);
  const { data, items, error, done } = useAgentSteps(id, { agent: result?.extra?.agentId, tool: block.id, enabled: open });
  return (
    <div className="chat-sub">
      <button type="button" className="chat-more" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />} {t('chat.sub_steps')}
        {open && !done && <LoaderCircle size={13} className="spin" aria-hidden="true" />}
      </button>
      {open && (
        <div className="chat-sub-body">
          {error && !data && <p className="chat-note">{t('chat.sub_waiting')}</p>}
          <Rows id={id} items={items} live={!done} limit={SUB_STEPS} />
        </div>
      )}
      {/* 步骤里最后一段就是子代理的回复；步骤没展开、取不到时才单独显示结果 */}
      {result && !(open && data) && <Markdown text={result.content || ''} />}
    </div>
  );
}

function ToolBody({ id, block, result }) {
  const { t } = useT();
  const input = block.input ?? {};
  switch (block.name) {
    case 'Bash':
      return (
        <>
          <Pre text={`$ ${input.command ?? ''}`} className="cmd" />
          <ResultText id={id} result={result} />
        </>
      );
    case 'Read':
      // 文件名在标题上（完整路径在提示里），这里只放读到的内容
      return <ResultText id={id} result={result} />;
    case 'Edit':
    case 'MultiEdit':
    case 'Write':
      return (
        <>
          <DiffView lines={diffLines(block.name, input, result?.extra)} />
          {result?.is_error && <ResultText id={id} result={result} />}
        </>
      );
    case 'TodoWrite':
      return <TodoList todos={input.todos ?? []} />;
    case 'Task':
    case 'Agent':
      return (
        <>
          <Pre text={input.prompt} />
          <SubAgent id={id} block={block} result={result} />
        </>
      );
    case 'ExitPlanMode':
      return <Markdown text={input.plan ?? ''} />;
    case 'AskUserQuestion':
      return (
        <>
          {(input.questions ?? []).map((q) => (
            <p key={q.question} className="chat-note">
              <b>{q.question}</b> {(q.options ?? []).map((o) => o.label).join(' / ')}
            </p>
          ))}
          <ResultText id={id} result={result} />
        </>
      );
    default:
      return <OtherToolBody id={id} block={block} result={result} />;
  }
}

/** 认不出的工具：参数一项一行（JSON 格式化、网址可点），被截断的参数可以取完整的；结果同样认出 JSON 和网址 */
function OtherToolBody({ id, block, result }) {
  const { t } = useT();
  const full = useFullInput(id, block);
  return (
    <>
      <FieldList input={full.input} />
      {full.truncated && (
        <p className="chat-note">
          {t('chat.input_truncated')}{' '}
          {full.canLoad && (
            <button type="button" className="chat-more" onClick={full.load} disabled={full.busy}>
              {t('chat.load_full_input')}
            </button>
          )}
        </p>
      )}
      <ResultText id={id} result={result} />
    </>
  );
}

/** 对话生成的图片的地址（`chat/file`）：放进 <img>、查看原图、下载都用它 */
const genFileUrl = (id, name, download) => `/instances/${id}/chat/file?name=${encodeURIComponent(name)}${download ? '&download=1' : ''}`;

/** 一张生成的图片和它的操作（查看原图、下载） */
function GenFigure({ id, name, caption }) {
  const { t } = useT();
  const src = useGenSrc(genFileUrl(id, name));
  return (
    <figure className="chat-gen-fig">
      <GenImage src={src} name={name} />
      <figcaption className="chat-gen-acts">
        <span className="chat-note ellipsis" title={caption || name}>
          {caption || name}
        </span>
        {src && (
          <a className="btn btn-plain btn-sm" href={src} target="_blank" rel="noopener noreferrer">
            {t('chat.res_open')}
          </a>
        )}
        {plainLinksWork() && (
          <a className="btn btn-plain btn-sm" href={`/api${genFileUrl(id, name, true)}`} download={name}>
            {t('chat.res_download')}
          </a>
        )}
      </figcaption>
    </figure>
  );
}

/** Codex 生成的图片：直接显示在对话里（手机上也不用去侧栏找） */
function ImageGenCard({ id, block, result, live }) {
  const { t } = useT();
  return (
    <div className="chat-tool chat-imagegen" data-state={toolState(block, result, live)}>
      <div className="chat-tool-head">
        <ImageIcon size={15} aria-hidden="true" />
        <b>{t('chat.imagegen')}</b>
      </div>
      <GenFigure id={id} name={block.input.file} caption={block.input.prompt} />
    </div>
  );
}

function ToolCard(props) {
  return props.block.name === 'ImageGen' && props.block.input?.file ? <ImageGenCard {...props} /> : <ToolCallCard {...props} />;
}

function ToolCallCard({ id, block, result, live }) {
  const { t } = useT();
  const diff = DIFF_TOOLS.includes(block.name) ? diffLines(block.name, block.input, result?.extra) : null;
  const stat = diff ? diffStat(diff) : null;
  // 改动默认展开（看改了什么是最常见的需要），太长的不展开
  const [open, setOpen] = useState(() => Boolean(diff && diff.length <= 40) || block.name === 'TodoWrite');
  const Icon = TOOL_ICONS[block.name] ?? Wrench;
  const state = toolState(block, result, live);
  const name = block.name.startsWith('mcp__') ? block.name.split('__').slice(1).join(' · ') : block.name;
  // 命令有说明时：标题是说明，命令收起时在下面一行（展开后在正文里）
  const cmd = block.name === 'Bash' && block.input?.description ? block.input.command : null;
  const out = block.name === 'Bash' && result?.content ? result.content.replace(/\s+$/, '') : '';
  const outLines = out ? out.split('\n').length : 0;
  return (
    <div className="chat-tool" data-state={state} data-kind={toolKind(block.name)}>
      <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
        <Icon size={15} className="chat-tool-icon" aria-hidden="true" />
        <b>{name}</b>
        <span className={`chat-tool-sum ellipsis ${cmd || block.name === 'Task' || block.name === 'Agent' ? 'plain' : ''}`} title={block.input?.file_path ?? undefined}>
          {toolSummary(block.name, block.input)}
        </span>
        {!open && outLines > 0 && <span className="chat-tool-lines">{t('chat.output_lines', { count: outLines })}</span>}
        {(block.input?.run_in_background === true || Boolean(result?.extra?.backgroundTaskId)) && <span className="badge">{t('chat.agent_background')}</span>}
        {stat && (stat.add > 0 || stat.del > 0) && (
          <DiffStat stat={stat} />
        )}
        <span className="chat-tool-state" title={t(`chat.tool_${state}`)}>
          {state === 'run' ? <LoaderCircle size={14} className="spin" /> : state === 'ok' ? <Check size={14} /> : state === 'err' ? <X size={14} /> : null}
        </span>
      </button>
      {cmd && !open && <code className="chat-tool-cmd ellipsis">$ {cmd.replace(/\s+/g, ' ')}</code>}
      {open && (
        <div className="chat-tool-body">
          <ToolBody id={id} block={block} result={result} />
        </div>
      )}
    </div>
  );
}

const toolState = (block, result, live) => (result ? (result.is_error ? 'err' : 'ok') : live ? 'run' : 'idle');

/**
 * 两段文字之间连续的工具调用合成一行（“运行 2 条命令 · 读取 3 个文件 · 修改 1 次”），默认收起，点开再看每一次。
 * 标题：各工具的次数、改动的增删行数；只有一种工具时跟着涉及的文件或命令；正在运行时显示正在做的那一步。
 */
function ToolGroup({ id, row, results, live }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const calls = row.calls;
  const states = calls.map((b) => toolState(b, results.get(b.id), live));
  const state = states.includes('run') ? 'run' : states.includes('err') ? 'err' : states.every((x) => x === 'ok') ? 'ok' : 'idle';
  const kinds = toolCounts(calls);
  const Icon = kinds.length === 1 ? TOOL_ICONS[calls[0].name] ?? Wrench : Layers;
  // 每种工具一段，按种类上色
  const label = kinds.map(([name, n], i) => (
    <span key={name} data-kind={toolKind(name)}>
      {i > 0 && ' · '}
      {t(`chat.count.${name}`, { count: n, name: name.startsWith('mcp__') ? name.split('__').slice(1).join(' · ') : name, defaultValue: '{{name}} ×{{count}}' })}
    </span>
  ));
  const groupKind = new Set(calls.map((b) => toolKind(b.name))).size === 1 ? toolKind(calls[0].name) : 'mix';
  // 正在运行时显示正在做的那一步；只有一种工具时列出涉及的文件、命令（几种混在一起时列出来太乱）
  const running = calls.find((b, n) => states[n] === 'run');
  const sums = running ? [toolSummary(running.name, running.input)] : kinds.length === 1 ? [...new Set(calls.map((b) => toolSummary(b.name, b.input)).filter(Boolean))] : [];
  let stat = null;
  for (const b of calls) {
    if (!EDIT_TOOLS.has(b.name)) continue;
    const d = diffStat(diffLines(b.name, b.input, results.get(b.id)?.extra));
    stat = { add: (stat?.add ?? 0) + d.add, del: (stat?.del ?? 0) + d.del };
  }
  return (
    <div className="chat-tool chat-group" data-state={state} data-kind={groupKind}>
      <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
        <Icon size={15} className="chat-tool-icon" aria-hidden="true" />
        <b>{label}</b>
        <span className="chat-tool-sum ellipsis">{sums.join(', ')}</span>
        {stat && (stat.add > 0 || stat.del > 0) && (
          <DiffStat stat={stat} />
        )}
        <span className="chat-tool-state" title={t(`chat.tool_${state}`)}>
          {state === 'run' ? <LoaderCircle size={14} className="spin" /> : state === 'ok' ? <Check size={14} /> : state === 'err' ? <X size={14} /> : null}
        </span>
      </button>
      {open && (
        <div className="chat-group-body">
          {calls.map((b) => (
            <ToolCard key={b.id} id={id} block={b} result={results.get(b.id)} live={live} />
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * 一串条目：assistant 回复拆成文字、思考、工具调用，连续的同类工具调用合成一组（主对话和子代理共用）。
 * limit：只画最近的这么多行，前面放“显示更早的 N 步”（每点一次多画一些）。
 */
function Rows({ id, items, live, limit = 0 }) {
  const { t } = useT();
  const { links } = useContext(ArtifactCtx);
  const results = useMemo(() => resultsById(items), [items]);
  const all = useMemo(() => chatRows(items, toolUseIds(items)), [items]);
  const [max, setMax] = useState(limit);
  const hidden = max ? Math.max(0, all.length - max) : 0;
  const rows = hidden ? all.slice(hidden) : all;
  const more = hidden > 0 && (
    <button key="more" type="button" className="chat-more" onClick={() => setMax((x) => x + limit * 4)}>
      {t('chat.sub_earlier', { count: hidden })}
    </button>
  );
  return [more, ...rows.map((r, n) => {
    switch (r.k) {
      case 'text': {
        // 一段回复的最后一段文字（后面是用户消息、提示或者没有了）：下面放复制按钮，复制这段回复的全部文字
        const next = rows[n + 1];
        if (live && !next) return <Markdown key={r.key} text={r.block.text} links={links} />;
        if (next && next.k !== 'item') return <Markdown key={r.key} text={r.block.text} links={links} />;
        let from = n;
        while (from > 0 && rows[from - 1].k !== 'item') from--;
        const all = rows.slice(from, n + 1).filter((x) => x.k === 'text').map((x) => x.block.text).join('\n\n');
        // 这一轮用了多久跟在复制按钮后面（不再单独占一行）
        const turn = next?.item?.k === 'turn' ? next.item : null;
        return (
          <div key={r.key} className="chat-reply-end">
            <Markdown text={r.block.text} links={links} />
            <div className="chat-reply-acts">
              <CopyButton text={all} label={false} variant="plain" />
              {turn && <span className="chat-turn">{t('chat.turn', { s: Math.max(1, Math.round(turn.ms / 1000)) })}</span>}
            </div>
          </div>
        );
      }
      case 'thinking':
        return <Thinking key={r.key} text={r.block.text} />;
      case 'tool':
        return <ToolCard key={r.key} id={id} block={r.block} result={results.get(r.block.id)} live={live} />;
      case 'group':
        return <ToolGroup key={r.key} id={id} row={r} results={results} live={live} />;
      case 'unknown':
        return (
          <p key={r.key} className="chat-note">
            {t('chat.unknown_block', { type: r.block.raw_type })}
          </p>
        );
      default:
        // 跟在回复后面的用时已经画在回复的结尾
        if (r.item.k === 'turn' && rows[n - 1]?.k === 'text') return null;
        return <Item key={r.key} id={id} item={r.item} />;
    }
  })];
}

function Thinking({ text }) {
  const { t } = useT();
  // 收起时标题后面跟着第一句，不用点开也知道在想什么
  const first = String(text ?? '').trim().split('\n')[0];
  return (
    <details className="chat-thinking">
      <summary>
        <Brain size={14} aria-hidden="true" /> {t('chat.thinking')}
        <span className="chat-thinking-peek ellipsis">{first}</span>
      </summary>
      <Pre text={text} />
    </details>
  );
}

/** 用户消息里第 i 张图片的地址（`chat/image`：从记录里取出来） */
const userImageUrl = (id, key, i) => `/instances/${id}/chat/image?key=${encodeURIComponent(key)}&n=${i}`;

/** 用户发的图片：气泡上方一排小图，点开看大图 */
function UserImages({ id, itemKey, n }) {
  const { t } = useT();
  const [open, setOpen] = useState(null);
  const urls = useMemo(() => Array.from({ length: n }, (_, i) => userImageUrl(id, itemKey, i)), [id, itemKey, n]);
  return (
    <div className="chat-user-imgs">
      {urls.map((url, i) => (
        <UserThumb key={url} url={url} label={t('chat.image_n', { n: i + 1, total: n })} onOpen={() => setOpen(i)} />
      ))}
      {open != null && <Lightbox urls={urls} index={open} onIndex={setOpen} onClose={() => setOpen(null)} />}
    </div>
  );
}

function UserThumb({ url, label, onOpen }) {
  const src = useGenSrc(url);
  const [broken, setBroken] = useState(false);
  return (
    <button type="button" className="chat-user-img" onClick={onOpen} aria-label={label} title={label} disabled={broken}>
      {broken ? <ImageIcon size={18} aria-hidden="true" /> : src ? <img src={src} alt="" loading="lazy" decoding="async" onError={() => setBroken(true)} /> : <LoaderCircle size={16} className="spin" aria-hidden="true" />}
    </button>
  );
}

/** 全屏看大图：点空白处、Esc 关闭；多张时左右切换（按钮、方向键、手机上左右滑） */
function Lightbox({ urls, index, onIndex, onClose }) {
  const { t } = useT();
  const src = useGenSrc(urls[index]);
  const many = urls.length > 1;
  const go = (d) => onIndex((index + d + urls.length) % urls.length);
  const touch = useRef(null);
  useEffect(() => {
    const key = (e) => {
      if (e.key === 'Escape') onClose();
      else if (many && e.key === 'ArrowLeft') go(-1);
      else if (many && e.key === 'ArrowRight') go(1);
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  });
  return createPortal(
    <div
      className="chat-lightbox"
      role="dialog"
      aria-modal="true"
      aria-label={t('chat.image_n', { n: index + 1, total: urls.length })}
      onClick={(e) => e.target === e.currentTarget && onClose()}
      onTouchStart={(e) => (touch.current = e.touches[0].clientX)}
      onTouchEnd={(e) => {
        const dx = e.changedTouches[0].clientX - (touch.current ?? 0);
        if (many && Math.abs(dx) > 50) go(dx < 0 ? 1 : -1);
      }}
    >
      <div className="chat-lightbox-bar">
        {many && <span className="chat-lightbox-count">{index + 1} / {urls.length}</span>}
        <span className="grow" />
        {src && (
          <a className="chat-lightbox-btn" href={src} target="_blank" rel="noopener noreferrer" aria-label={t('chat.res_open')} title={t('chat.res_open')}>
            <ExternalLink size={18} />
          </a>
        )}
        <button type="button" className="chat-lightbox-btn" onClick={onClose} aria-label={t('action.close')} title={t('action.close')} autoFocus>
          <X size={20} />
        </button>
      </div>
      {src ? <img key={src} className="chat-lightbox-img" src={src} alt="" onClick={onClose} /> : <LoaderCircle size={28} className="spin chat-lightbox-wait" aria-hidden="true" />}
      {many && (
        <>
          <button type="button" className="chat-lightbox-btn chat-lightbox-nav prev" onClick={() => go(-1)} aria-label={t('chat.image_prev')}>
            <ChevronLeft size={22} />
          </button>
          <button type="button" className="chat-lightbox-btn chat-lightbox-nav next" onClick={() => go(1)} aria-label={t('chat.image_next')}>
            <ChevronRight size={22} />
          </button>
        </>
      )}
    </div>,
    document.body,
  );
}

/** 不是 assistant 回复的条目（用户消息、单独的工具结果、分隔线、提示……） */
function Item({ id, item }) {
  const { t } = useT();
  const agent = useContext(AgentCtx).id;
  const thumbs = item.images > 0 && agent === 'claude';
  switch (item.k) {
    case 'user':
      return (
        <div className="chat-user" data-mid={item.mid ? '' : undefined}>
          {/* Claude 在忙时发的：这一步做完就并进了这一轮 */}
          {item.mid && (
            <span className="chat-mid-tag">
              <CornerDownRight size={12} aria-hidden="true" />
              {t('chat.mid_tag')}
            </span>
          )}
          {/* 图片小图只有 Claude Code 有（图片存在 transcript 里）；Codex 照旧只写几张 */}
          {thumbs && <UserImages id={id} itemKey={item.key} n={item.images} />}
          {(item.text.trim() || (item.images > 0 && !thumbs)) && (
            <div className="chat-bubble">
              {item.text}
              {item.images > 0 && !thumbs && <span className="chat-note">{t('chat.images', { n: item.images })}</span>}
            </div>
          )}
        </div>
      );
    case 'task':
      return (
        <p className="chat-event" title={item.summary}>
          <Layers size={13} aria-hidden="true" />
          <span className="ellipsis">{t(`chat.task_${['completed', 'failed', 'killed'].includes(item.status) ? item.status : 'other'}`, { status: item.status })} · {item.summary}</span>
        </p>
      );
    case 'tool_result':
      // 对应的工具调用不在已加载的范围里：单独显示
      return (
        <div className="chat-tool" data-state={item.is_error ? 'err' : 'ok'}>
          <div className="chat-tool-body">
            <ResultText id={id} result={item} />
          </div>
        </div>
      );
    case 'interrupt':
      return <p className="chat-event">{t('chat.interrupted')}</p>;
    case 'command':
      return <Pre text={item.text} className="chat-command" />;
    case 'divider':
      return <p className="chat-divider">{t('chat.compacted')}</p>;
    case 'summary':
      return (
        <details className="chat-thinking">
          <summary>{t('chat.summary')}</summary>
          <Markdown text={item.text} />
        </details>
      );
    case 'info':
      return <p className="chat-event">{item.text}</p>;
    case 'turn':
      return <p className="chat-turn">{t('chat.turn', { s: Math.max(1, Math.round(item.ms / 1000)) })}</p>;
    default:
      return null;
  }
}

// ---------------- 等你处理的卡片（在对话流的最后） ----------------

/** 输入框、按钮以外的地方按回车：等于点了卡片上的主按钮 */
function useEnterKey(active, onEnter) {
  useEffect(() => {
    if (!active) return undefined;
    const on = (e) => {
      if (e.key !== 'Enter' || e.shiftKey || e.isComposing) return;
      const el = document.activeElement;
      if (el && (el.closest('input, textarea, select, button, [contenteditable="true"]') || el.isContentEditable)) return;
      e.preventDefault();
      onEnter();
    };
    document.addEventListener('keydown', on);
    return () => document.removeEventListener('keydown', on);
  }, [active, onEnter]);
}

function QuestionForm({ pending, onAnswer, busy }) {
  const { t } = useT();
  const questions = pending.input?.questions ?? [];
  const [picked, setPicked] = useState({});
  const toggle = (n, label, multi) =>
    setPicked((p) => {
      const cur = p[n]?.labels ?? [];
      const labels = multi ? (cur.includes(label) ? cur.filter((x) => x !== label) : [...cur, label]) : [label];
      return { ...p, [n]: { ...p[n], labels } };
    });
  const answers = questionAnswers(questions, picked);
  const ready = questions.every((q) => answers[q.question]);
  return (
    <>
      {questions.map((q, n) => (
        <fieldset key={q.question} className="chat-q">
          <legend>{q.question}</legend>
          {(q.options ?? []).map((o) => {
            const on = (picked[n]?.labels ?? []).includes(o.label);
            return (
              <button key={o.label} type="button" className="chat-opt" role={q.multiSelect ? 'checkbox' : 'radio'} aria-checked={on} data-multi={q.multiSelect ? '' : undefined} onClick={() => toggle(n, o.label, q.multiSelect)}>
                <span className="chat-opt-dot" aria-hidden="true">{on && q.multiSelect && <Check size={12} />}</span>
                <span className="chat-opt-text">
                  <b>{o.label}</b>
                  {o.description && o.description !== o.label && <span className="chat-note">{o.description}</span>}
                </span>
              </button>
            );
          })}
          <input className="chat-field" placeholder={t('chat.q_other')} value={picked[n]?.other ?? ''} onChange={(e) => setPicked((p) => ({ ...p, [n]: { ...p[n], other: e.target.value } }))} />
        </fieldset>
      ))}
      <div className="chat-acts">
        <Button busy={busy} disabled={!ready} onClick={() => onAnswer('answer', { answers })}>
          {t('chat.q_submit')}
        </Button>
      </div>
    </>
  );
}

/** 权限审批、Claude 的提问、计划审批 */
function PendingCard({ id, pending, active }) {
  const { t } = useT();
  const { caps } = useContext(AgentCtx);
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [reason, setReason] = useState('');
  const [change, setChange] = useState('');
  const [open, setOpen] = useState(false);
  useEffect(() => {
    setReason('');
    setChange('');
    setOpen(false);
  }, [pending.id]);
  const answer = async (decision, extra = {}) => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/permission`, { id: pending.id, decision, ...extra });
    } catch (e) {
      // 已经在别处答过了：卡片会随状态更新消失
      if (e.code !== 'CHAT_STALE') toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const plan = async (choice, feedback) => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/plan`, { choice, feedback });
      setChange('');
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const kind = pending.kind;
  const enter = useCallback(() => {
    if (busy) return;
    if (kind === 'permission') answer('allow');
    else if (kind === 'plan') plan(1);
  }, [busy, kind, pending.id]);
  useEnterKey(active && kind !== 'question', enter);
  const input = pending.input ?? {};
  const dirs = (pending.suggestions ?? []).filter?.((s) => s.type === 'addDirectories') ?? [];
  let body;
  if (kind === 'question') {
    body = <QuestionForm pending={pending} onAnswer={answer} busy={busy} />;
  } else if (kind === 'plan') {
    body = (
      <>
        <div className="chat-plan">
          <Markdown text={input.plan ?? ''} />
        </div>
        <div className="chat-acts">
          <Button busy={busy} onClick={() => plan(1)}>
            {t('chat.plan_auto')} <kbd className="chat-kbd">↵</kbd>
          </Button>
          <Button variant="secondary" busy={busy} onClick={() => plan(2)}>
            {t('chat.plan_manual')}
          </Button>
        </div>
        <form
          className="chat-acts"
          onSubmit={(e) => {
            e.preventDefault();
            if (change.trim()) plan(3, change.trim());
          }}
        >
          <input className="chat-field grow" placeholder={t('chat.plan_change')} value={change} onChange={(e) => setChange(e.target.value)} />
          <Button type="submit" variant="ghost" busy={busy} disabled={!change.trim()}>
            {t('chat.plan_send_change')}
          </Button>
        </form>
      </>
    );
  } else {
    const diff = DIFF_TOOLS.includes(pending.tool) ? diffLines(pending.tool, input) : null;
    body = (
      <>
        {pending.tool === 'Bash' ? (
          <Pre text={`$ ${input.command ?? ''}`} className="cmd" />
        ) : pending.tool === 'apply_patch' && Array.isArray(input.changes) ? (
          // Codex 要改文件：列出文件，展开看改动
          <>
            <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
              {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
              <span className="chat-tool-sum ellipsis">{input.changes.length === 1 ? input.changes[0].path : t('chat.patch_files', { n: input.changes.length })}</span>
            </button>
            {open && <Pre text={input.changes.map((c) => `${c.path}\n${c.diff ?? ''}`).join('\n')} />}
          </>
        ) : (
          <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
            {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
            <span className="chat-tool-sum ellipsis">{toolSummary(pending.tool, input) || t('chat.details')}</span>
          </button>
        )}
        {pending.tool !== 'apply_patch' && (open || (diff && diff.length <= 40)) && (diff ? <DiffView lines={diff} /> : <Pre text={JSON.stringify(input, null, 2)} />)}
        {input.description && <p className="chat-note">{input.description}</p>}
        <div className="chat-acts">
          <Button busy={busy} onClick={() => answer('allow')}>
            {t('chat.allow')} <kbd className="chat-kbd">↵</kbd>
          </Button>
          {caps.allow_session !== false && (
            <Button variant="secondary" busy={busy} onClick={() => answer('allow_session')} title={t('chat.allow_session_hint')}>
              {t('chat.allow_session')}
            </Button>
          )}
          {caps.allow_dirs !== false && dirs.length > 0 && (
            <Button variant="secondary" busy={busy} onClick={() => answer('allow_dirs')} title={dirs.flatMap((d) => d.directories ?? []).join('\n')}>
              {t('chat.allow_dirs')}
            </Button>
          )}
        </div>
        <form
          className="chat-acts"
          onSubmit={(e) => {
            e.preventDefault();
            answer('deny', { message: reason });
          }}
        >
          <input className="chat-field grow" placeholder={t('chat.deny_reason')} value={reason} onChange={(e) => setReason(e.target.value)} />
          <Button type="submit" variant="ghost" busy={busy}>
            {t('chat.deny')}
          </Button>
        </form>
      </>
    );
  }
  const title = kind === 'question' ? t('chat.q_title') : kind === 'plan' ? t('chat.plan_title') : t('chat.perm_title', { tool: pending.tool });
  const Icon = kind === 'permission' ? ShieldCheck : kind === 'plan' ? ListTodo : MessageCircleQuestion;
  return (
    <section className="chat-ask" aria-live="polite">
      <header>
        <Icon size={16} aria-hidden="true" />
        <b>{title}</b>
      </header>
      <div className="chat-ask-body">{body}</div>
    </section>
  );
}

// ---------------- Claude Code 的选择框（原生弹窗） ----------------

/**
 * Claude Code 显示了选择框（信任文件夹、切换模型的确认、/rewind……）：读出来的选项显示成弹窗，选中的那项按方向键移过去再回车。
 * 读不出选项时只能取消（Esc）。
 */
// ---------------- 额度用完 ----------------

/**
 * Claude Code 额度用完时的菜单（`/rate-limit-options`，2.1.310）：标题、选项是英文，这里换成中文。
 * “等重置后自动继续”只在关掉了 autoContinueAtUsageLimit 时出现（开着时不弹菜单，直接等）。
 */
const LIMIT_AUTO = 'Wait here, then continue automatically';
const LIMIT_OPTIONS = {
  'Stop and wait for limit to reset': 'stop',
  Stop: 'stop',
  'Switch to usage credits': 'credits',
  'Upgrade your plan': 'upgrade',
  'Ask your admin for more usage': 'admin',
  'Don\u2019t continue automatically': 'no_auto',
  "Don't continue automatically": 'no_auto',
};
const isLimitDialog = (d) => Boolean(d?.options?.some((o) => o.label.startsWith(LIMIT_AUTO) || o.label === 'Stop and wait for limit to reset'));
function limitOption(t, label) {
  if (label.startsWith(LIMIT_AUTO)) return t('chat.limit.opt_auto');
  const k = LIMIT_OPTIONS[label];
  return k ? t(`chat.limit.opt_${k}`) : label;
}

/** 额度什么时候重置：用量接口给的准确时间（用完的那个窗口；都没满时取最近的），按浏览器时区显示 */
function LimitReset({ id }) {
  const { data } = usePoll(`/instances/${id}/chat/usage`);
  const wins = [data?.limits?.five_hour, data?.limits?.seven_day].filter((w) => w?.resets_at);
  const full = wins.find((w) => w.used >= 100);
  const at = (full ?? wins.sort((a, b) => parseInstant(a.resets_at) - parseInstant(b.resets_at))[0])?.resets_at;
  return at ? <ResetAt at={at} /> : null;
}

/** 看看替用户选“自动继续”的倒计时 */
function AutoIn({ at }) {
  const { t } = useT();
  const [, tick] = useReducer((n) => n + 1, 0);
  useEffect(() => {
    const iv = setInterval(tick, 1000);
    return () => clearInterval(iv);
  }, []);
  const s = Math.max(0, Math.ceil((at - Date.now()) / 1000));
  return <p className="chat-limit-auto">{t('chat.limit.auto_in', { s })}</p>;
}

/** 额度用完、正在等重置后自动继续：输入框上面一张卡片，可以取消 */
function LimitWait({ id, waiting }) {
  const { t } = useT();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const cancel = async () => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/interrupt`);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="chat-limit" role="status">
      <span className="chat-limit-head">
        <Hourglass size={15} aria-hidden="true" />
        {t('chat.limit.waiting')}
      </span>
      <div className="chat-limit-when">
        <LimitReset id={id} />
        <span className="chat-note">{t('chat.limit.terminal_says', { when: waiting })}</span>
      </div>
      <p className="chat-note">{t('chat.limit.keep_open')}</p>
      <div className="chat-acts">
        <Button variant="ghost" onClick={cancel} busy={busy}>
          {t('chat.limit.cancel_wait')}
        </Button>
      </div>
    </div>
  );
}

/** 认出来的确认框换成当前语言：回给后端的还是原文 */
function knownText(t, { kind, path }) {
  return {
    title: t(`chat.known.${kind}_title`),
    body: t(`chat.known.${kind}_body`, { path }),
    option: (label) => (label === KNOWN_DIALOGS[kind] ? t(`chat.known.${kind}_yes`) : label === 'No, exit' ? t(`chat.known.${kind}_no`) : label),
  };
}

function TuiDialog({ id, dialog, limit }) {
  const { t } = useT();
  const toast = useToast();
  const [pick, setPick] = useState(dialog?.selected ?? 0);
  const [busy, setBusy] = useState(false);
  const box = useRef(null);
  const sig = dialog ? `${dialog.title}|${dialog.options.map((o) => o.label).join('|')}` : '';
  const isLimit = isLimitDialog(dialog);
  const kd = knownDialog(dialog);
  const known = kd ? knownText(t, kd) : null;
  useEffect(() => {
    setPick(dialog?.selected ?? 0);
  }, [sig]);
  useEffect(() => {
    box.current?.focus();
  }, [sig]);
  const send = async (body) => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/dialog`, body);
    } catch (e) {
      if (e.code !== 'CHAT_STALE') toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const choose = () => send({ index: pick, title: dialog.title, label: dialog.options[pick]?.label ?? '' });
  const cancel = () => send({});
  const onKey = (e) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      cancel();
    } else if (dialog && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      setPick((p) => Math.max(0, Math.min(dialog.options.length - 1, p + (e.key === 'ArrowDown' ? 1 : -1))));
    } else if (dialog && e.key === 'Enter' && !e.target.closest('button')) {
      e.preventDefault();
      choose();
    }
  };
  return (
    <div className="chat-scrim">
      <div className="chat-dialog" role="dialog" aria-modal="true" aria-label={known?.title || dialog?.title || t('chat.dialog_title')} tabIndex={-1} ref={box} onKeyDown={onKey}>
        {dialog ? (
          <>
            <h3>{isLimit ? t('chat.limit.title') : (known?.title ?? dialog.title)}</h3>
            {known ? <p className="chat-dialog-body">{known.body}</p> : dialog.body.length > 0 && <p className="chat-dialog-body">{dialog.body.join('\n').trim()}</p>}
            {isLimit && (
              <div className="chat-limit-when">
                <LimitReset id={id} />
              </div>
            )}
            {isLimit && limit?.auto_at > 0 && <AutoIn at={limit.auto_at} />}
            <div className="chat-dialog-opts" role="radiogroup">
              {dialog.options.map((o, n) => (
                // eslint-disable-next-line react/no-array-index-key -- 选项按位置对应 TUI 里的顺序
                <button key={n} type="button" className="chat-opt" role="radio" aria-checked={pick === n} onClick={() => setPick(n)} onDoubleClick={() => send({ index: n, title: dialog.title, label: o.label })}>
                  <span className="chat-opt-dot" aria-hidden="true" />
                  <span className="chat-opt-text">
                    <b>
                      {isLimit ? limitOption(t, o.label) : known ? known.option(o.label) : o.label}
                      {o.current && <Check size={13} className="chat-opt-cur" aria-label={t('chat.dialog_current')} />}
                    </b>
                    {isLimit && o.label !== limitOption(t, o.label) && <span className="chat-note">{o.label}</span>}
                    {o.hint && <span className="chat-note">{o.hint}</span>}
                  </span>
                </button>
              ))}
            </div>
            <div className="chat-acts chat-dialog-acts">
              <Button variant="ghost" onClick={cancel} disabled={busy}>
                {t('action.cancel')}
              </Button>
              <Button onClick={choose} busy={busy}>
                {t('action.confirm')}
              </Button>
            </div>
          </>
        ) : (
          <>
            <h3>{t('chat.dialog_title')}</h3>
            <p className="chat-dialog-body">{t('chat.dialog_unknown')}</p>
            <div className="chat-acts chat-dialog-acts">
              <Button variant="secondary" onClick={cancel} busy={busy}>
                {t('chat.dialog_close')}
              </Button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

// ---------------- 输入框 ----------------

let attachSeq = 0;

/** 弹出层：贴着输入框上沿往上展开，点外面或按 Esc 关掉 */
function Popover({ onClose, className = '', children, label }) {
  const ref = useRef(null);
  useEffect(() => {
    const away = (e) => !ref.current?.contains(e.target) && !e.target.closest?.('[data-pop-anchor]') && onClose();
    const esc = (e) => e.key === 'Escape' && onClose();
    document.addEventListener('pointerdown', away);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('pointerdown', away);
      document.removeEventListener('keydown', esc);
    };
  }, [onClose]);
  return (
    <div ref={ref} className={`chat-pop ${className}`} role="menu" aria-label={label}>
      {children}
    </div>
  );
}

const MODEL_RE = /^[A-Za-z0-9._\-[\]]{1,64}$/;

/** 模型菜单：Claude Code 的选项（和 /model 一样）、服务商的其他模型、自定义模型 ID，下面是这个模型的思考强度。 */
function ModelMenu({ id, status, models, view, onClose }) {
  const { t } = useT();
  const toast = useToast();
  const [busy, setBusy] = useState(null);
  const [custom, setCustom] = useState('');
  const [more, setMore] = useState(false);
  const idle = status?.phase === 'idle';
  const options = models?.options ?? [];
  const set = async (kind, value) => {
    setBusy(`${kind}:${value}`);
    try {
      await api.post(`/instances/${id}/chat/setting`, { kind, value });
      if (kind === 'model') onClose();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(null);
    }
  };
  const cur = options.find((o) => o.value === view.checked);
  const efforts = cur?.efforts?.length ? cur.efforts : EFFORTS;
  const row = (o) => (
    <button key={o.value} type="button" className="chat-mi" role="menuitemradio" aria-checked={view.checked === o.value} disabled={!idle || busy != null} onClick={() => set('model', o.value)}>
      <span className="chat-mi-text">
        <span className={o.section === 'provider' ? 'mono' : ''}>{o.value === 'default' ? t('chat.model_default') : o.label}</span>
        {(o.value === 'default' ? o.description && t('chat.model_default_is', { name: o.description }) : o.description) && <small>{o.value === 'default' ? t('chat.model_default_is', { name: o.description }) : o.description}</small>}
      </span>
      {busy === `model:${o.value}` ? <LoaderCircle size={15} className="spin" /> : <Check size={15} className="chat-mi-check" aria-hidden="true" />}
    </button>
  );
  const main = options.filter((o) => o.section === 'main');
  const older = options.filter((o) => o.section === 'more');
  const provider = options.filter((o) => o.section === 'provider');
  const olderOpen = more || older.some((o) => o.value === view.checked);
  return (
    <Popover onClose={onClose} className="chat-pop-right chat-pop-model" label={t('chat.model_pick')}>
      <div className="chat-pop-h">
        <span>{t('chat.model')}</span>
        {models?.provider?.name && <span>{models.provider.name}</span>}
      </div>
      {!models && <LoaderCircle size={16} className="spin chat-pop-wait" />}
      {main.map(row)}
      {older.length > 0 &&
        (olderOpen ? (
          older.map(row)
        ) : (
          <button type="button" className="chat-mi chat-mi-more" onClick={() => setMore(true)}>
            <span className="chat-mi-text">{t('chat.model_older', { n: older.length })}</span>
            <ChevronDown size={14} aria-hidden="true" />
          </button>
        ))}
      {provider.length > 0 && (
        <>
          <div className="chat-pop-sep" />
          <div className="chat-pop-h">
            <span>{t('chat.model_provider')}</span>
          </div>
          <div className="chat-pop-scroll">{provider.map(row)}</div>
        </>
      )}
      <form
        className="chat-pop-custom"
        onSubmit={(e) => {
          e.preventDefault();
          const v = custom.trim();
          if (MODEL_RE.test(v)) set('model', v);
        }}
      >
        <input className="chat-field mono" placeholder={t('chat.model_custom')} value={custom} onChange={(e) => setCustom(e.target.value)} disabled={!idle} spellCheck={false} />
        <Button type="submit" variant="secondary" disabled={!idle || !MODEL_RE.test(custom.trim())}>
          {t('chat.model_use')}
        </Button>
      </form>
      <div className="chat-pop-sep" />
      <div className="chat-pop-h">
        <span>{t('chat.effort_pick')}</span>
        <span>{status?.effort ? t(`chat.effort_${status.effort}`, { defaultValue: status.effort }) : ''}</span>
      </div>
      <div className="chat-seg" role="radiogroup" aria-label={t('chat.effort_pick')}>
        {efforts.map((x) => (
          <button key={x} type="button" role="radio" aria-checked={status?.effort === x} disabled={!idle || busy != null} onClick={() => set('effort', x)}>
            {busy === `effort:${x}` ? <LoaderCircle size={13} className="spin" /> : t(`chat.effort_${x}`, { defaultValue: x })}
          </button>
        ))}
      </div>
      {!idle && <p className="chat-pop-note">{t('chat.model_wait')}</p>}
      {status?.actual_model && <p className="chat-pop-note">{t('chat.model_actual', { model: status.actual_model })}</p>}
    </Popover>
  );
}

function ModeMenu({ id, status, onClose }) {
  const { t } = useT();
  const toast = useToast();
  const [busy, setBusy] = useState(null);
  const ready = status?.phase === 'idle' || status?.phase === 'running';
  const set = async (m) => {
    setBusy(m);
    try {
      await api.post(`/instances/${id}/chat/setting`, { kind: 'mode', value: m });
      onClose();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(null);
    }
  };
  return (
    <Popover onClose={onClose} className="chat-pop-left" label={t('chat.mode_pick')}>
      <div className="chat-pop-h">
        <span>{t('chat.mode_pick')}</span>
        <span>Shift+Tab</span>
      </div>
      {MODE_CHOICES.map((m) => (
        <button key={m} type="button" className="chat-mi" role="menuitemradio" aria-checked={status?.mode === m} disabled={!ready || busy != null} onClick={() => set(m)}>
          <span className="chat-mi-text">
            <span>{t(`chat.mode_${m}`)}</span>
            <small>{t(`chat.mode_hint_${m}`)}</small>
          </span>
          {busy === m ? <LoaderCircle size={15} className="spin" /> : <Check size={15} className="chat-mi-check" aria-hidden="true" />}
        </button>
      ))}
    </Popover>
  );
}

/** `2.1.310` 这样的版本号不低于 min（不知道版本时当作够新，服务端还会再查） */
function versionAtLeast(v, min) {
  if (!v) return true;
  const p = String(v).split('.').map((x) => parseInt(x, 10));
  if (p.length < 3 || p.some(Number.isNaN)) return true;
  for (let i = 0; i < 3; i++) if (p[i] !== min[i]) return p[i] > min[i];
  return true;
}

/** 上下文用量的小圆环（点开右侧的“用量”） */
function ContextRing({ used, window: win, onClick }) {
  const { t } = useT();
  if (!used) return null;
  const total = win || 200000;
  const p = Math.min(1, used / total);
  const label = t('chat.context_ring', { pct: Math.round(p * 100), used: tokens(used), total: tokens(total) });
  return (
    <button type="button" className="btn btn-plain chat-ring" data-level={p > 0.85 ? 'high' : p > 0.6 ? 'mid' : 'low'} onClick={onClick} aria-label={label} title={label}>
      <svg viewBox="0 0 36 36" width="16" height="16" aria-hidden="true">
        <circle cx="18" cy="18" r="14" fill="none" stroke="var(--line)" strokeWidth="5" />
        <circle cx="18" cy="18" r="14" fill="none" stroke="currentColor" strokeWidth="5" strokeDasharray={`${(88 * p).toFixed(1)} 88`} transform="rotate(-90 18 18)" />
      </svg>
      {/* 只有圆环时像在转圈加载，写上百分比 */}
      <span>{Math.round(p * 100)}%</span>
    </button>
  );
}

/**
 * 输入框里还没发出去的字，在这个浏览器里按会话各存一份：手机把后台的页面回收了、登录过期自动刷新、
 * 下拉刷新之后，重新打开还在（只在本机，不同步到别的设备）。发出去后清掉；7 天没动的不要了。
 */
const DRAFT_KEY = (id) => `looklook.chat-draft.${id}`;
const DRAFT_MAX_AGE = 7 * 86_400_000;
function readDraft(id) {
  try {
    const d = JSON.parse(localStorage.getItem(DRAFT_KEY(id)) || 'null');
    return d && typeof d.text === 'string' && Date.now() - d.at < DRAFT_MAX_AGE ? d.text : '';
  } catch {
    return '';
  }
}
function writeDraft(id, text) {
  try {
    if (text.trim()) localStorage.setItem(DRAFT_KEY(id), JSON.stringify({ text, at: Date.now() }));
    else localStorage.removeItem(DRAFT_KEY(id));
  } catch {
    /* 隐私模式等存不了：不影响输入 */
  }
}

function Composer({ id, status, phase, touch, offline, models, usage, onAction, onSide }) {
  const { t } = useT();
  // 权限模式只有 Claude Code 有；模型、思考强度、上下文用量按工具的能力（caps）显示
  const { id: agentId, caps } = useContext(AgentCtx);
  const toast = useToast();
  const [text, setText] = useState(() => readDraft(id));
  const [files, setFiles] = useState([]);
  const [sending, setSending] = useState(false);
  // 停手 0.3 秒存一次；页面要被收起、关掉时马上存
  // 正在发送的那段：输入框已经清空，但确认送到之前存着的仍是它（发送途中页面被回收也不丢）
  const inflight = useRef('');
  const latest = useRef(text);
  latest.current = text;
  useEffect(() => {
    const timer = setTimeout(() => writeDraft(id, text || inflight.current), 300);
    return () => clearTimeout(timer);
  }, [id, text]);
  useEffect(() => {
    const flush = () => writeDraft(id, latest.current || inflight.current);
    const onHide = () => document.visibilityState === 'hidden' && flush();
    window.addEventListener('pagehide', flush);
    document.addEventListener('visibilitychange', onHide);
    return () => {
      window.removeEventListener('pagehide', flush);
      document.removeEventListener('visibilitychange', onHide);
    };
  }, [id]);
  const [menu, setMenu] = useState(null);
  const area = useRef(null);
  const picker = useRef(null);
  const disabled = offline || phase === 'tui' || phase === 'approval' || phase === 'detached';
  const running = phase === 'running';
  const view = modelView(status, models?.options ?? []);
  const bg = backgroundCounts(status);
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.35))}px`;
  }, [text]);
  // 外面（斜杠命令）要打开模型菜单
  useEffect(() => {
    const open = () => setMenu('model');
    window.addEventListener(`ll-chat-model:${id}`, open);
    return () => window.removeEventListener(`ll-chat-model:${id}`, open);
  }, [id]);
  // 外面（空白对话里的常用开头）要把一句话填进输入框
  useEffect(() => {
    const fill = (e) => {
      setText(e.detail ?? '');
      requestAnimationFrame(() => area.current?.focus());
    };
    window.addEventListener(`ll-chat-fill:${id}`, fill);
    return () => window.removeEventListener(`ll-chat-fill:${id}`, fill);
  }, [id]);
  // 图片先传到运行终端的那台电脑的附件目录，发送时把路径交给 Claude Code
  const addFiles = (list) => {
    for (const file of list) {
      if (!file.type.startsWith('image/')) continue;
      const item = { key: ++attachSeq, name: file.name || 'image.png', url: URL.createObjectURL(file), path: null, failed: false };
      setFiles((f) => [...f, item]);
      runUpload({ file, dest: 'attach' }, { instanceId: id })
        .then((path) => setFiles((f) => f.map((x) => (x.key === item.key ? { ...x, path } : x))))
        .catch((e) => {
          toast(errorText(t, e), 'error');
          setFiles((f) => f.map((x) => (x.key === item.key ? { ...x, failed: true } : x)));
        });
    }
  };
  const removeFile = (key) =>
    setFiles((f) => {
      const x = f.find((y) => y.key === key);
      if (x) URL.revokeObjectURL(x.url);
      return f.filter((y) => y.key !== key);
    });
  const uploading = files.some((f) => !f.path && !f.failed);
  // now：Claude Code 的“立即发送”（Ctrl+Enter）。不带 now 时运行中发的是排队
  const send = async (now = false) => {
    const body = text.trim();
    const images = files.filter((f) => f.path).map((f) => f.path);
    if ((!body && !images.length && !now) || sending || uploading || disabled) return;
    // 界面自己能做的命令（/model、/resume、/context……）不送进去
    const action = !images.length && slashAction(body);
    if (action) {
      setText('');
      if (action === 'model') setMenu('model');
      else onAction(action);
      return;
    }
    // 第一次发消息时（有用户操作）申请通知权限：需要确认、做完了在后台也能知道
    if (typeof Notification !== 'undefined' && Notification.permission === 'default') Notification.requestPermission().catch?.(() => {});
    setSending(true);
    // 先清空输入框（斜杠命令也一样）；没送出去时再放回来，不丢用户打的字
    inflight.current = body;
    setText('');
    try {
      const r = await api.post(`/instances/${id}/chat/input`, now && canNow ? { text: body, images, now: true } : { text: body, images });
      inflight.current = '';
      writeDraft(id, latest.current);
      files.forEach((f) => URL.revokeObjectURL(f.url));
      setFiles([]);
      if (r?.delivered === false) toast(t('chat.not_delivered'), 'error');
    } catch (e) {
      inflight.current = '';
      setText((cur) => cur || body);
      toast(errorText(t, e), 'error');
    } finally {
      setSending(false);
    }
  };
  const stop = async () => {
    try {
      await api.post(`/instances/${id}/chat/interrupt`);
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  const onKey = (e) => {
    if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !e.nativeEvent.isComposing && canNow) {
      // 和 Claude Code 一样：Ctrl+Enter 立即发送
      e.preventDefault();
      send(true);
    } else if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing && !touch) {
      e.preventDefault();
      if (slash.length && slashPick != null) {
        pickSlash(slash[slashPick]);
        return;
      }
      send();
    } else if (e.key === 'Escape' && running && !text) {
      e.preventDefault();
      stop();
    } else if (slash.length && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
      e.preventDefault();
      setSlashPick((p) => ((p ?? -1) + (e.key === 'ArrowDown' ? 1 : -1) + slash.length) % slash.length);
    } else if (e.key === 'Tab' && !e.shiftKey && !text && suggestion) {
      // 和 Claude Code 一样：Tab 用上推荐回复（填进输入框，可以改了再发）
      e.preventDefault();
      applySuggestion();
    } else if (e.key === 'Tab' && e.shiftKey && !text && caps.mode !== false) {
      // 和 Claude Code 一样：Shift+Tab 轮换权限模式
      e.preventDefault();
      const i = MODE_CHOICES.indexOf(status?.mode);
      api.post(`/instances/${id}/chat/setting`, { kind: 'mode', value: MODE_CHOICES[(i + 1) % 3] }).catch((err) => toast(errorText(t, err), 'error'));
    }
  };
  // 输入 / 时：内置的常用命令 + 这个项目和用户自己的命令、技能（第一次用到时取）
  const [custom, setCustom] = useState(null);
  const [slashPick, setSlashPick] = useState(null);
  const wantSlash = /^\/\S*$/.test(text);
  useEffect(() => {
    if (wantSlash && custom === null) {
      setCustom([]);
      api.get(`/instances/${id}/chat/commands`).then((r) => setCustom(r?.items ?? []), () => {});
    }
    setSlashPick(null);
  }, [wantSlash, text]);
  const slash = useMemo(() => {
    if (!wantSlash) return [];
    const q = text.toLowerCase();
    const builtin = COMMANDS.filter(([name]) => agentId !== 'codex' || CODEX_COMMANDS.includes(name)).map(([name, action]) => ({ name, action, description: t(`chat.cmd.${name.slice(1)}`) }));
    const names = new Set([...builtin.map((c) => c.name), ...TERMINAL_ONLY]);
    const mine = (custom ?? []).filter((c) => !names.has(c.name));
    const all = [...builtin, ...mine];
    const hit = all.filter((c) => c.name.toLowerCase().startsWith(q));
    return (hit.length ? hit : all.filter((c) => c.name.toLowerCase().includes(q.slice(1)))).slice(0, 40);
  }, [wantSlash, text, custom]);
  const pickSlash = (c) => {
    if (c.action) {
      setText('');
      if (c.action === 'model') setMenu('model');
      else onAction(c.action);
      return;
    }
    setText(`${c.name} `);
    area.current?.focus();
  };
  // Claude Code 跑完一轮后推荐的下一句（终端里灰着显示在输入框里）：空闲、输入框空着时显示，点一下或按 Tab 填进来
  const suggestion = phase === 'idle' && !offline && !text && !files.length ? status?.suggestion || null : null;
  const applySuggestion = () => {
    setText(suggestion);
    requestAnimationFrame(() => {
      const el = area.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    });
  };
  const modeText = status?.mode ? t(`chat.mode_${MODE_CHOICES.includes(status.mode) || status.mode === 'dontAsk' ? status.mode : 'other'}`, { mode: status.mode }) : t('chat.mode');
  const ready = phase === 'idle' || phase === 'running';
  const showStop = running && !text.trim() && !files.length;
  const canSend = !!text.trim() || files.some((f) => f.path);
  // 运行中能不能“立即发送”：Claude Code 2.1.275 起才有
  const canNow = running && agentId === 'claude' && versionAtLeast(status?.version, [2, 1, 275]);
  // 运行中打了字：讲清楚“排队”和“立即发送”
  const twoWays = running && canSend && !disabled;
  return (
    <div className="chat-composer" data-running={running ? '' : undefined} data-ways={twoWays && canNow ? '' : undefined}>
      {slash.length > 0 && (
        <ul className="chat-slash" role="listbox">
          {slash.map((c, n) => (
            <li key={c.name}>
              <button type="button" aria-selected={slashPick === n} onClick={() => pickSlash(c)}>
                <b>{c.name}</b>
                <span className="chat-note ellipsis">{c.description}</span>
                {c.action && <span className="badge">{t(`chat.cmd_action_${c.action}`)}</span>}
                {c.source && <span className="badge">{t(`chat.cmd_source_${c.source}`)}</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
      {menu === 'model' && <ModelMenu id={id} status={status} models={models} view={view} onClose={() => setMenu(null)} />}
      {menu === 'mode' && <ModeMenu id={id} status={status} onClose={() => setMenu(null)} />}
      {/* 运行中发的、还在 Claude Code 队列里的消息：这一步做完会并进对话 */}
      {status?.queued?.length > 0 && (
        <div className="chat-queued" role="status">
          <span className="chat-queued-head">
            <i aria-hidden="true" />
            <b>{t('chat.queued_title')}</b>
            <span className="chat-note">{t('chat.queued_hint')}</span>
            {canNow && !canSend && (
              <button type="button" className="btn btn-plain btn-sm chat-now-btn" onClick={() => send(true)} disabled={sending} title={t('chat.way_now_hint')}>
                <Zap size={13} aria-hidden="true" />
                {t('chat.way_now')}
              </button>
            )}
          </span>
          {status.queued.map((q, n) => (
            <p key={n} className="chat-queued-text">
              {q}
            </p>
          ))}
        </div>
      )}
      {twoWays && (
        <div className="chat-ways" role="note">
          <span>
            {touch ? <ArrowUp size={13} aria-hidden="true" /> : <kbd>Enter</kbd>}
            <b>{t('chat.way_queue')}</b>
            {t('chat.way_queue_hint')}
          </span>
          {canNow && (
            <span>
              {touch ? <Zap size={13} aria-hidden="true" /> : <kbd>Ctrl+Enter</kbd>}
              <b>{t('chat.way_now')}</b>
              {t('chat.way_now_hint')}
            </span>
          )}
        </div>
      )}
      {files.length > 0 && (
        <div className="chat-files">
          {files.map((f) => (
            <span key={f.key} className="chat-file" data-state={f.failed ? 'err' : f.path ? 'ok' : 'up'}>
              <img src={f.url} alt={f.name} />
              {!f.path && !f.failed && <LoaderCircle size={14} className="spin" />}
              <button type="button" onClick={() => removeFile(f.key)} aria-label={t('chat.remove_image')}>
                <X size={12} />
              </button>
            </span>
          ))}
        </div>
      )}
      <div className="chat-input-box">
        <input
          ref={picker}
          type="file"
          accept="image/*"
          multiple
          hidden
          onChange={(e) => {
            addFiles([...e.target.files]);
            e.target.value = '';
          }}
        />
        <textarea
          ref={area}
          rows={1}
          value={text}
          disabled={disabled}
          placeholder={suggestion ?? t(offline ? 'chat.placeholder_offline' : disabled ? `chat.placeholder_${phase}` : running ? 'chat.placeholder_running' : touch ? 'chat.placeholder_touch' : 'chat.placeholder')}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={onKey}
          onPaste={(e) => {
            const imgs = [...(e.clipboardData?.files ?? [])].filter((f) => f.type.startsWith('image/'));
            if (imgs.length) {
              e.preventDefault();
              addFiles(imgs);
            }
          }}
        />
        {suggestion && !touch && (
          <button type="button" className="chat-suggest-key" onClick={applySuggestion} title={t('chat.suggest_hint')}>
            <kbd>Tab</kbd>
          </button>
        )}
        <div className="chat-input-tools">
          <button type="button" className="btn btn-plain btn-icon" onClick={() => picker.current?.click()} disabled={disabled || caps.images === false} aria-label={t('chat.attach')} title={caps.images === false ? t('chat.no_images') : t('chat.attach')}>
            <Plus size={18} />
          </button>
          {caps.mode !== false && (
          <button type="button" className={`btn btn-plain chat-chip chat-chip-mode`} data-mode={status?.mode ?? ''} data-pop-anchor="" aria-expanded={menu === 'mode'} disabled={!ready} onClick={() => setMenu(menu === 'mode' ? null : 'mode')} title={t('chat.mode_pick')}>
            <ShieldCheck size={14} aria-hidden="true" />
            <span className="ellipsis">{modeText}</span>
            <ChevronDown size={13} aria-hidden="true" />
          </button>
          )}
          <span className="grow" />
          {/* 还在跑的后台子代理、命令：点了打开右侧的“任务” */}
          {bg.subagent > 0 && (
            <button type="button" className="btn btn-plain chat-chip chat-bg-on" onClick={() => onSide('tasks')} title={t('chat.bg_agents', { count: bg.subagent })}>
              <Bot size={14} aria-hidden="true" />
              {bg.subagent}
            </button>
          )}
          {bg.shell > 0 && (
            <button type="button" className="btn btn-plain chat-chip chat-bg-on" onClick={() => onSide('tasks')} title={t('chat.bg_shells', { count: bg.shell })}>
              <Activity size={14} aria-hidden="true" />
              {bg.shell}
            </button>
          )}
          {caps.models !== false && (
          <button
            type="button"
            className="btn btn-plain chat-chip"
            data-pop-anchor=""
            aria-expanded={menu === 'model'}
            disabled={phase === 'detached'}
            onClick={() => setMenu(menu === 'model' ? null : 'model')}
            title={view.mismatch ? t('chat.model_mismatch', { model: view.mismatch }) : view.pending ? t('chat.model_pending') : status?.actual_model ? t('chat.model_actual', { model: status.actual_model }) : t('chat.model_pick')}
          >
            {view.mismatch && <TriangleAlert size={13} className="chat-warn" aria-label={t('chat.model_mismatch', { model: view.mismatch })} />}
            <span className="ellipsis">{view.mismatch || view.label || t('chat.model')}</span>
            {status?.effort && <span className="chat-chip-sub">{t(`chat.effort_${status.effort}`, { defaultValue: status.effort })}</span>}
            <ChevronDown size={13} aria-hidden="true" />
          </button>
          )}
          {caps.usage !== false && <ContextRing used={status?.context} window={usage?.window} onClick={() => onAction('usage')} />}
          {showStop ? (
            <button type="button" className="btn btn-icon chat-send chat-stop" onClick={stop} aria-label={t('chat.stop')} title={t('chat.stop_hint')}>
              <Square size={14} fill="currentColor" />
            </button>
          ) : (
            <>
              {twoWays && canNow && (
                <button type="button" className="btn btn-plain chat-chip chat-now-btn" onClick={() => send(true)} disabled={sending || uploading} title={t('chat.way_now_title')}>
                  <Zap size={14} aria-hidden="true" />
                  {t('chat.way_now')}
                </button>
              )}
              <button type="button" className="btn btn-primary btn-icon chat-send" onClick={() => send()} disabled={disabled || sending || uploading || !canSend} aria-label={running ? t('chat.way_queue') : t('chat.send')} title={running ? t('chat.send_queue') : t('chat.send')}>
                {sending ? <LoaderCircle size={18} className="spin" /> : <ArrowUp size={18} />}
              </button>
            </>
          )}
        </div>
      </div>
    </div>
  );
}

// ---------------- 状态 ----------------

/**
 * 输入框上面的状态提示：空闲、运行中不显示（运行中对话流的最后有“正在…”），
 * 只有要注意的状态（等你确认、在等选择、重连中、出错）才出现。
 * 电脑离线也在这里说（对话标签不叠浮层，免得挡住消息）。
 */
function StatusNotice({ status, online, offline }) {
  const { t } = useT();
  if (!status) return null;
  const { phase, tool, error, reason } = status;
  if (!online) {
    return (
      <div className="chat-status chat-status-off" role="status">
        {offline ? <WifiOff size={14} aria-hidden="true" /> : <span className="chat-status-dot" aria-hidden="true" />}
        <span className="ellipsis">{t(offline ? 'chat.offline_status' : 'chat.reconnecting')}</span>
        <span className="chat-status-dots" aria-hidden="true">
          <i />
          <i />
          <i />
        </span>
      </div>
    );
  }
  if (!error && (phase === 'idle' || phase === 'running' || phase === 'detached')) return null;
  const label = phase === 'detached' && reason === 'not_attached' ? 'chat.phase_not_attached' : `chat.phase_${phase}`;
  return (
    <div className="chat-status" data-phase={phase}>
      <span className="chat-status-dot" aria-hidden="true" />
      <span className="grow ellipsis">
        {phase === 'running' && tool ? t('chat.running_tool', { tool }) : t(label)}
        {error && <span className="chat-status-err"> · {error}</span>}
      </span>
    </div>
  );
}

/** 对话区右上角：右侧面板的开关（浮在对话上面，不占一行） */
function SideToggle({ side, onSide }) {
  const { t } = useT();
  // Codex 的侧栏有“资源”（生成的图片、产出的文件），按钮的说明里写上
  const label = useContext(AgentCtx).id === 'codex' ? t('chat.side_toggle_codex') : t('chat.side_toggle');
  return (
    <button type="button" className="btn btn-plain btn-icon chat-side-toggle" onClick={() => onSide(side ? null : 'changes')} aria-pressed={Boolean(side)} aria-label={label} title={label}>
      <PanelRight size={17} />
    </button>
  );
}

/** Claude Code 没在运行或没接上对话界面时的说明 */
function Detached({ id, status }) {
  const { t } = useT();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const reason = status?.reason ?? 'starting';
  const run = async (path) => {
    setBusy(true);
    try {
      await api.post(path);
      setConfirming(false);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const relaunch = () => run(`/instances/${id}/chat/relaunch`);
  // 以前的会话（升级前开的、在终端里自己开的 Claude Code / Codex）：后台等它这一轮做完自动转到对话模式、
  // 接着同一段对话，也可以现在就转
  const adoptable = reason === 'not_attached';
  const key = adoptable ? 'adopt_wait' : reason;
  const adoptNow = () => run(`/instances/${id}/chat/adopt`);
  return (
    <div className="chat-detached">
      {(reason === 'starting' || reason === 'adopting' || adoptable) && <LoaderCircle size={18} className="spin" aria-hidden="true" />}
      <b>{t(`chat.detached_${key}_title`, { defaultValue: t('chat.detached_starting_title') })}</b>
      <p className="muted">{t(`chat.detached_${key}_body`, { defaultValue: '' })}</p>
      {adoptable && (
        <div className="chat-acts">
          {confirming ? (
            <>
              <p className="chat-note" style={{ flexBasis: '100%', margin: 0 }}>
                {t('chat.adopt_now_confirm')}
              </p>
              <Button busy={busy} onClick={adoptNow}>
                {t('chat.adopt_now')}
              </Button>
              <Button variant="ghost" onClick={() => setConfirming(false)}>
                {t('action.cancel')}
              </Button>
            </>
          ) : (
            <Button variant="ghost" onClick={() => setConfirming(true)}>
              {t('chat.adopt_now')}
            </Button>
          )}
        </div>
      )}
      {(reason === 'not_running' || reason === 'ended' || reason === 'app_server') && (
        <div className="chat-acts">
          <Button busy={busy} onClick={relaunch}>
            {t(reason === 'app_server' ? 'chat.relaunch_app_server' : status?.session_id ? 'chat.relaunch_resume' : 'chat.relaunch')}
          </Button>
        </div>
      )}
    </div>
  );
}

const STATE_ICONS = { running: LoaderCircle, done: Check, completed: Check, failed: X, killed: CircleStop, stopped: CircleStop, ended: CircleDashed };

function TaskState({ state }) {
  const { t } = useT();
  const Icon = STATE_ICONS[state] ?? CircleDashed;
  return (
    <span className="chat-task-state" data-state={state} title={t(`chat.state_${state}`, { defaultValue: state })}>
      <Icon size={14} className={state === 'running' ? 'spin' : ''} aria-hidden="true" />
    </span>
  );
}

/** 后台命令：命令、状态、输出（展开后运行中每 2 秒刷新）、停止 */
function ShellTask({ id, shell }) {
  const { t } = useT();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [out, setOut] = useState(null);
  const confirm = useConfirm();
  const [stopping, setStopping] = useState(false);
  const running = shell.state === 'running';
  useEffect(() => {
    if (!open) return undefined;
    let alive = true;
    let timer = null;
    const load = async () => {
      try {
        const d = await api.get(`/instances/${id}/chat/task/output?tool=${encodeURIComponent(shell.tool_use_id)}`);
        if (alive) setOut(d);
      } catch {
        if (alive) setOut({ text: '', missing: true });
      }
      if (alive && running) timer = setTimeout(load, 2000);
    };
    load();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [open, running]);
  // 停止会结束这条命令（构建、服务、测试……），先确认
  const stop = async () => {
    const ok = await confirm({ title: t('chat.task_stop_title'), body: shell.command, confirm: t('chat.task_stop_confirm'), danger: true });
    if (!ok) return;
    setStopping(true);
    try {
      await api.post(`/instances/${id}/chat/task/stop`, { tool: shell.tool_use_id });
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setStopping(false);
    }
  };
  const started = shell.started ? Date.parse(shell.started) : null;
  return (
    <li className="chat-task" data-state={shell.state}>
      <div className="chat-task-head">
        <TaskState state={shell.state} />
        <button type="button" className="chat-task-main" onClick={() => setOpen(!open)} aria-expanded={open}>
          <code className="ellipsis">{shell.command}</code>
          <span className="chat-note ellipsis">
            {[shell.description, t(`chat.state_${shell.state}`, { defaultValue: shell.state }), started && running ? duration(Date.now() - started) : null].filter(Boolean).join(' · ')}
          </span>
        </button>
        {running && (
          <Button variant="danger" className="chat-task-stop" busy={stopping} onClick={stop}>
            {t('chat.task_stop')}
          </Button>
        )}
      </div>
      {open && (
        <div className="chat-task-body">
          {out == null ? <LoaderCircle size={14} className="spin" /> : out.missing ? <p className="chat-note">{t('chat.task_no_output')}</p> : <Pre text={out.text || t('chat.task_empty_output')} />}
          {out?.truncated && <p className="chat-note">{t('chat.task_output_tail')}</p>}
          {shell.summary && <p className="chat-note">{shell.summary}</p>}
        </div>
      )}
    </li>
  );
}

/** 一个子代理的步骤（“任务”面板里点开） */
function AgentDetail({ id, agent, onBack }) {
  const { t } = useT();
  const { data, items, error, done } = useAgentSteps(id, { agent: agent.id, tool: agent.tool_use_id, enabled: true });
  const live = agent.state === 'running' && !done;
  const body = useRef(null);
  const steps = useRef(null);
  // 跟着最新的步骤走（停在底部时）
  useFollow(body, steps, [items]);
  return (
    <div className="chat-side-detail">
      <header className="chat-side-head">
        <button type="button" className="btn btn-plain btn-icon" onClick={onBack} aria-label={t('chat.back')} title={t('chat.back')}>
          <ArrowLeft size={18} />
        </button>
        <TaskState state={live ? 'running' : agent.state} />
        <b className="grow ellipsis">{agent.description || agent.type}</b>
      </header>
      <div className="chat-side-body" ref={body}>
        <p className="chat-note">{[agent.type, agent.model && modelLabel(agent.model), agent.background ? t('chat.agent_background') : null, agent.worktree ? t('chat.agent_worktree') : null].filter(Boolean).join(' · ')}</p>
        {error && !data && <p className="chat-note">{t('chat.sub_waiting')}</p>}
        <div className="chat-items" ref={steps}>
          <Rows id={id} items={items} live={live} />
        </div>
      </div>
    </div>
  );
}

// ---------------- 右侧面板：改动、任务、用量、设置 ----------------

/** 打开着、需要时每隔一会儿重新取一次 */
function usePoll(url, { enabled = true, every = 0, deps = [] } = {}) {
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  useEffect(() => {
    if (!enabled) return undefined;
    let alive = true;
    let timer = null;
    let misses = 0;
    const load = async () => {
      try {
        const d = await api.get(url);
        if (!alive) return;
        setData(d);
        setError(null);
        misses = 0;
      } catch (e) {
        if (!alive) return;
        setError(e);
        // 连不上（客户端在重启、更新、网络断了）：过一会儿自己再取，不用等用户重新打开
        if (e.code === 'CLIENT_UNREACHABLE' || e.code === 'NETWORK') {
          misses += 1;
          timer = setTimeout(load, Math.min(2000 * misses, 15000));
          return;
        }
      }
      if (alive && every) timer = setTimeout(load, document.hidden ? every * 3 : every);
    };
    load();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [url, enabled, ...deps]);
  return { data, error };
}

function ChangesTab({ id, items, onTab }) {
  const { t } = useT();
  // 对话有新内容时重新取（改动只会在工具调用之后出现）
  const n = items.length;
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const timer = setTimeout(() => setTick((x) => x + 1), 800);
    return () => clearTimeout(timer);
  }, [n]);
  const { data, error } = usePoll(`/instances/${id}/chat/changes`, { deps: [tick] });
  const files = data?.files ?? [];
  const sum = files.reduce((a, f) => ({ add: a.add + f.add, del: a.del + f.del }), { add: 0, del: 0 });
  return (
    <>
      {!data && !error && <LoaderCircle size={16} className="spin" />}
      {error && !data && <p className="chat-note">{errorText(t, error)}</p>}
      {data && files.length === 0 && <p className="chat-note">{t('chat.changes_empty')}</p>}
      {files.length > 0 && (
        <>
          <p className="chat-side-sum">
            {t('chat.changes_sum', { count: files.length })}
            <DiffStat stat={sum} />
          </p>
          <ul className="chat-files-list">
            {files.map((f) => (
              <li key={f.path} title={f.path}>
                <FilePen size={14} aria-hidden="true" />
                <span className="ellipsis mono">{f.path.split(/[\\/]/).slice(-2).join('/')}</span>
                <DiffStat stat={f} />
              </li>
            ))}
          </ul>
          <Button variant="secondary" icon={GitBranch} onClick={() => onTab('git')}>
            {t('chat.changes_git')}
          </Button>
        </>
      )}
    </>
  );
}

/** 子代理和后台命令。打开时每 2 秒刷新；点子代理看它的每一步，后台命令可以看输出、停止。 */
function TasksTab({ id }) {
  const { t } = useT();
  const [agentId, setAgentId] = useState(null);
  const [, tick] = useReducer((n) => n + 1, 0);
  const { data, error } = usePoll(`/instances/${id}/chat/tasks`, { every: 2000 });
  useEffect(() => {
    const iv = setInterval(tick, 1000);
    return () => clearInterval(iv);
  }, []);
  const agents = data?.agents ?? [];
  const shells = [...(data?.shells ?? [])].reverse();
  const agent = agents.find((a) => a.id === agentId);
  if (agent) return <AgentDetail id={id} agent={agent} onBack={() => setAgentId(null)} />;
  const runningAgents = agents.filter((a) => a.state === 'running').length;
  const runningShells = shells.filter((s) => s.state === 'running').length + (data?.unlisted_shells ?? 0);
  return (
    <>
      {!data && !error && <LoaderCircle size={16} className="spin" />}
      {error && !data && <p className="chat-note">{errorText(t, error)}</p>}
      {data && (
        <>
          <h4 className="chat-side-title">
            <Bot size={15} aria-hidden="true" /> {t('chat.agents_title')}
            <span className="chat-note">{runningAgents ? t('chat.n_running', { count: runningAgents }) : agents.length || ''}</span>
          </h4>
          {agents.length === 0 ? (
            <p className="chat-note">{t('chat.agents_empty')}</p>
          ) : (
            <ul className="chat-tasks">
              {agents.map((a) => {
                const last = a.last_tool ? `${a.last_tool.name} ${toolSummary(a.last_tool.name, a.last_tool.input)}` : a.last_text;
                const started = a.started ? Date.parse(a.started) : null;
                const end = a.state === 'running' ? Date.now() : a.updated;
                return (
                  <li key={a.id} className="chat-task" data-state={a.state}>
                    <button type="button" className="chat-task-head chat-task-main" onClick={() => setAgentId(a.id)}>
                      <TaskState state={a.state} />
                      <span className="chat-task-text">
                        <b className="ellipsis">{a.description || a.type}</b>
                        <span className="chat-note ellipsis">
                          {[a.type, a.model && modelLabel(a.model), t('chat.agent_steps', { count: a.steps }), started && end ? duration(end - started) : null, a.background ? t('chat.agent_background') : null].filter(Boolean).join(' · ')}
                        </span>
                        {last && (
                          <span className="chat-note ellipsis mono">
                            {a.state === 'running' ? '▸ ' : ''}
                            {last}
                          </span>
                        )}
                      </span>
                      <ChevronRight size={15} aria-hidden="true" />
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          <h4 className="chat-side-title">
            <Activity size={15} aria-hidden="true" /> {t('chat.shells_title')}
            <span className="chat-note">{runningShells ? t('chat.n_running', { count: runningShells }) : shells.length || ''}</span>
          </h4>
          {/* 状态栏上说在跑、对话记录里找不到的（子代理启动的、/clear 之前启动的） */}
          {data?.unlisted_shells > 0 && <p className="chat-note chat-unlisted">{t('chat.shells_unlisted', { count: data.unlisted_shells })}</p>}
          {shells.length === 0 ? (
            !data?.unlisted_shells && <p className="chat-note">{t('chat.shells_empty')}</p>
          ) : (
            <ul className="chat-tasks">
              {shells.map((s) => (
                <ShellTask key={s.tool_use_id} id={id} shell={s} />
              ))}
            </ul>
          )}
        </>
      )}
    </>
  );
}

function Meter({ label, value, sub }) {
  const p = Math.max(0, Math.min(100, value));
  return (
    <div className="chat-meter" data-level={p > 85 ? 'high' : p > 60 ? 'mid' : 'low'}>
      <div className="chat-meter-head">
        <span>{label}</span>
        <b>{Math.round(p)}%</b>
      </div>
      <div className="chat-meter-bar">
        <i style={{ width: `${p}%` }} />
      </div>
      {sub && <span className="chat-note">{sub}</span>}
    </div>
  );
}

/** 重置时间：具体时间（浏览器时区，带偏移）和还剩多久，每半分钟更新 */
function ResetAt({ at }) {
  const { t } = useT();
  const [, tick] = useReducer((n) => n + 1, 0);
  useEffect(() => {
    const iv = setInterval(tick, 30_000);
    return () => clearInterval(iv);
  }, []);
  // 给的是 17:59:59.94 这种，按整分钟显示（18:00）
  const when = Math.round(parseInstant(at) / 60_000) * 60_000;
  const ms = when - Date.now();
  if (!Number.isFinite(ms) || ms <= 0) return null;
  return (
    <span className="chat-reset" title={new Date(when).toString()}>
      <span>{t('chat.usage_reset_at', { at: formatTime(when) })}</span>
      <b>{t('chat.usage_left', { in: spanText(t, ms) })}</b>
    </span>
  );
}
const resetText = (_t, iso) => (iso ? <ResetAt at={iso} /> : '');

function UsageTab({ id, status, onCompact }) {
  const { t } = useT();
  const { name: agentName } = useContext(AgentCtx);
  const { data, error } = usePoll(`/instances/${id}/chat/usage`, { deps: [status?.context, status?.actual_model] });
  if (!data) return error ? <p className="chat-note">{errorText(t, error)}</p> : <LoaderCircle size={16} className="spin" />;
  const used = data.context ?? 0;
  const win = data.window || 200000;
  const lim = data.limits;
  return (
    <>
      <Meter label={t('chat.usage_context')} value={(used / win) * 100} sub={t('chat.usage_context_sub', { used: tokens(used), total: tokens(win) })} />
      <dl className="chat-kv">
        <dt>{t('chat.usage_model')}</dt>
        <dd className="mono">{data.model ?? '—'}</dd>
        <dt>{t('chat.usage_in')}</dt>
        <dd>{tokens(data.totals?.input)}</dd>
        <dt>{t('chat.usage_cache')}</dt>
        <dd>{tokens(data.totals?.cache_read)}</dd>
        <dt>{t('chat.usage_out')}</dt>
        <dd>{tokens(data.totals?.output)}</dd>
        {data.version && (
          <>
            <dt>{agentName}</dt>
            <dd>{data.version}</dd>
          </>
        )}
      </dl>
      {lim?.account && (
        <dl className="chat-kv">
          <dt>{t('chat.usage_account')}</dt>
          <dd className="ellipsis" title={lim.account.email ?? ''}>
            {lim.account.email ?? lim.account.type ?? '—'}
          </dd>
          {(lim.account.plan || lim.plan) && (
            <>
              <dt>{t('chat.usage_plan')}</dt>
              <dd>{lim.account.plan || lim.plan}</dd>
            </>
          )}
        </dl>
      )}
      {lim?.five_hour && <Meter label={t('chat.usage_5h')} value={lim.five_hour.used} sub={resetText(t, lim.five_hour.resets_at)} />}
      {lim?.seven_day && <Meter label={t('chat.usage_7d')} value={lim.seven_day.used} sub={resetText(t, lim.seven_day.resets_at)} />}
      {lim?.reached && <p className="chat-note chat-warn">{t('chat.usage_reached')}</p>}
      {lim && (lim.five_hour || lim.seven_day) && <p className="chat-note">{t(lim.account ? 'chat.usage_limits_note_codex' : lim.live ? 'chat.usage_limits_note_live' : 'chat.usage_limits_note')}</p>}
      {!lim?.live && lim?.fetched_at > 0 && <p className="chat-note">{t(lim.five_hour || lim.seven_day ? 'chat.usage_fetched' : 'chat.usage_stale', { ago: spanText(t, Date.now() - lim.fetched_at) })}</p>}
      <Button variant="secondary" onClick={onCompact} disabled={status?.phase !== 'idle' || !used}>
        {t('chat.usage_compact')}
      </Button>
    </>
  );
}

function ConfigTab({ id }) {
  const { t } = useT();
  const { data, error } = usePoll(`/instances/${id}/chat/config`);
  if (!data) return error ? <p className="chat-note">{errorText(t, error)}</p> : <LoaderCircle size={16} className="spin" />;
  return (
    <>
      <h4 className="chat-side-title">
        <ShieldCheck size={15} aria-hidden="true" /> {t('chat.config_rules')}
      </h4>
      {data.rules.length === 0 ? (
        <p className="chat-note">{t('chat.config_rules_empty')}</p>
      ) : (
        <ul className="chat-rules">
          {data.rules.map((r, n) => (
            // eslint-disable-next-line react/no-array-index-key -- 规则可能重复
            <li key={n} data-kind={r.kind}>
              <span className="badge">{t(`chat.rule_${r.kind}`)}</span>
              <code className="ellipsis" title={r.rule}>
                {r.rule}
              </code>
              <span className="chat-note">{t(`chat.src_${r.source}`)}</span>
            </li>
          ))}
        </ul>
      )}
      <h4 className="chat-side-title">
        <Plug size={15} aria-hidden="true" /> {t('chat.config_mcp')}
      </h4>
      {data.mcp.length === 0 ? (
        <p className="chat-note">{t('chat.config_mcp_empty')}</p>
      ) : (
        <ul className="chat-rules">
          {data.mcp.map((m) => (
            <li key={`${m.source}:${m.name}`}>
              <b className="ellipsis">{m.name}</b>
              <span className="chat-note">
                {m.type} · {t(`chat.src_${m.source}`)}
              </span>
            </li>
          ))}
        </ul>
      )}
      <p className="chat-note">{t('chat.config_note')}</p>
    </>
  );
}

const SIDE_TABS = ['changes', 'tasks', 'usage', 'config'];

const RES_ICONS = { link: Globe, search: Search, image: ImageIcon };
const ART_ICONS = { image: ImageIcon, pdf: FileText, video: FileText, audio: FileText, text: FileText, file: FileText };

/** 对话生成的图片的显示地址：本机直接用接口地址，远程（经 WebRTC 转发）先取成 blob */
function useGenSrc(url) {
  const [src, setSrc] = useState(plainLinksWork() ? `/api${url}` : null);
  useEffect(() => {
    if (plainLinksWork()) return undefined;
    let alive = true;
    let made = null;
    fetchRaw(url)
      .then((r) => r.blob())
      .then((b) => {
        made = URL.createObjectURL(b);
        if (alive) setSrc(made);
      })
      .catch(() => {});
    return () => {
      alive = false;
      if (made) URL.revokeObjectURL(made);
    };
  }, [url]);
  return src;
}

function GenImage({ src, name }) {
  return src ? (
    <a href={src} target="_blank" rel="noopener noreferrer" title={name}>
      <img src={src} alt={name} />
    </a>
  ) : (
    <span className="chat-gen-wait">
      <LoaderCircle size={16} className="spin" />
    </span>
  );
}

/**
 * Codex 对话的“资源”栏：上面是这段对话生成的图片（每张有自己的地址，可以查看原图、下载），
 * 下面是对话里用到的网页、搜索、看过的图片（从已经载入的对话条目里找，最近的在前）。
 */
function ResourcesTab({ id, items }) {
  const { t } = useT();
  const list = useMemo(() => conversationResources(items), [items]);
  const { data } = usePoll(`/instances/${id}/chat/files`, { deps: [items.length] });
  const files = data?.items ?? [];
  const made = useContext(ArtifactCtx).list;
  if (!list.length && !files.length && !made.length) return <p className="chat-note">{t('chat.res_empty')}</p>;
  return (
    <>
      {files.length > 0 && (
        <>
          <h4 className="chat-side-title">
            <ImageIcon size={15} aria-hidden="true" /> {t('chat.res_generated', { n: files.length })}
          </h4>
          <ul className="chat-gen">
            {files.map((f) => (
              <li key={f.name}>
                <GenFigure id={id} name={f.name} />
              </li>
            ))}
          </ul>
        </>
      )}
      {made.length > 0 && (
        <>
          <h4 className="chat-side-title">
            <FileText size={15} aria-hidden="true" /> {t('chat.res_made', { n: made.length })}
          </h4>
          <ul className="chat-rules chat-res">
            {made.map((a) => {
              const Icon = ART_ICONS[a.kind] ?? FileText;
              return (
                <li key={a.path}>
                  <Icon size={14} aria-hidden="true" />
                  <a className="ellipsis" href={artifactUrl(id, a.path, !a.viewable)} {...(a.viewable ? { target: '_blank', rel: 'noopener noreferrer' } : { download: a.name })} title={a.path}>
                    {a.name}
                  </a>
                  <a className="btn btn-plain btn-sm" href={artifactUrl(id, a.path, true)} download={a.name}>
                    {t('chat.res_download')}
                  </a>
                </li>
              );
            })}
          </ul>
        </>
      )}
      {list.length > 0 && (files.length > 0 || made.length > 0) && (
        <h4 className="chat-side-title">
          <Globe size={15} aria-hidden="true" /> {t('chat.res_used')}
        </h4>
      )}
      <ul className="chat-rules chat-res">
      {list.map((r) => {
        const Icon = RES_ICONS[r.kind] ?? Globe;
        return (
          <li key={`${r.kind}:${r.value}`}>
            <Icon size={14} aria-hidden="true" />
            {r.kind === 'link' ? (
              <a className="ellipsis" href={r.value} target="_blank" rel="noopener noreferrer" title={r.value}>
                {r.title || r.value}
              </a>
            ) : (
              <code className="ellipsis" title={r.value}>
                {r.value}
              </code>
            )}
            <span className="chat-note">{t(r.turn ? 'chat.res_this_turn' : `chat.res_${r.kind}`)}</span>
          </li>
        );
      })}
      </ul>
    </>
  );
}

function SidePanel({ id, tab, onTab, onClose, status, items, onTermTab, onCompact }) {
  const { t } = useT();
  // 资源栏先只给 Codex
  const tabs = useContext(AgentCtx).id === 'claude' ? SIDE_TABS : ['changes', 'resources', 'usage'];
  const bg = backgroundCounts(status);
  const running = (bg.subagent ?? 0) + (bg.shell ?? 0);
  return (
    <aside className="chat-side" aria-label={t('chat.side_toggle')}>
      <header className="chat-side-head">
        <nav className="chat-side-tabs" role="tablist">
          {tabs.map((k) => (
            <button key={k} type="button" role="tab" aria-selected={tab === k} onClick={() => onTab(k)}>
              {t(`chat.side_${k}`)}
              {k === 'tasks' && running > 0 && <span className="chat-side-count">{running}</span>}
            </button>
          ))}
        </nav>
        <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('action.close')} title={t('action.close')}>
          <X size={17} />
        </button>
      </header>
      <div className="chat-side-body">
        {tab === 'changes' && <ChangesTab id={id} items={items} onTab={onTermTab} />}
        {tab === 'tasks' && <TasksTab id={id} />}
        {tab === 'usage' && <UsageTab id={id} status={status} onCompact={onCompact} />}
        {tab === 'config' && <ConfigTab id={id} />}
        {tab === 'resources' && <ResourcesTab id={id} items={items} />}
      </div>
    </aside>
  );
}

/**
 * 运行中：屏幕上的状态行（例如“Symbioting… (6s · ↓ 274 tokens)”）和正在输出的那段回复（灰色）。
 * transcript 要等整段写完才有，写进来之后这里自动不再显示。
 */
/** 和终端里一样轮换的小星星（· ✢ ✳ ✶ ✻ ✽，再倒回来） */
const GLYPHS = ['·', '✢', '✳', '✶', '✻', '✽', '✻', '✶', '✳', '✢'];
function SpinGlyph() {
  const [n, setN] = useState(0);
  useEffect(() => {
    if (window.matchMedia?.('(prefers-reduced-motion: reduce)').matches) return undefined;
    const iv = setInterval(() => setN((x) => (x + 1) % GLYPHS.length), 120);
    return () => clearInterval(iv);
  }, []);
  return (
    <span className="chat-working-mark" aria-hidden="true">
      {GLYPHS[n]}
    </span>
  );
}

function Live({ status, items, since }) {
  const { t, i18n } = useT();
  // 已经跑了多久，每秒更新
  const [, tick] = useReducer((n) => n + 1, 0);
  useEffect(() => {
    const iv = setInterval(tick, 1000);
    return () => clearInterval(iv);
  }, []);
  const secs = since ? Math.max(0, Math.round((Date.now() - since) / 1000)) : 0;
  const preview = status?.preview ?? '';
  const last = [...items].reverse().find((i) => i.k === 'assistant');
  const lastText = (last?.blocks ?? []).filter((b) => b.t === 'text').map((b) => b.text).join('\n');
  const head = preview.slice(0, 30);
  const shown = preview && !(head && lastText.replace(/\s+/g, '').includes(head.replace(/\s+/g, '')));
  // Claude Code 的状态行（`Unraveling… (8s · ↓ 22 tokens)`）：中文界面把动词换成中文；Codex 的另有说法
  const act = status?.agent === 'claude' || !status?.agent ? spinnerText(status?.activity, i18n.language) : { text: activityText(t, status?.activity) ?? '', detail: '', orig: '' };
  return (
    <>
      {shown && <div className="chat-preview">{preview}</div>}
      <p className="chat-working">
        <SpinGlyph />
        <span className="ellipsis">{act.text || (status?.tool ? t('chat.running_tool', { tool: status.tool }) : t('chat.working'))}</span>
        {act.orig && <span className="chat-working-orig" lang="en">{act.orig}</span>}
        {act.detail && <span className="chat-working-detail">{act.detail}</span>}
        {secs > 0 && <span className="chat-working-time">{duration(secs * 1000)}</span>}
      </p>
    </>
  );
}

/**
 * 页面在后台时：需要确认、要到终端里操作、这一轮做完了，发浏览器通知，标题前面加个标记，回到页面时去掉。
 * `prev` 是 ChatView 记的上一个状态（这里只读）。
 */
function useAttention(id, phase, pendingKind, prev, shells, agent) {
  const { t } = useT(agent);
  const mark = useRef(null);
  useEffect(() => {
    const from = prev.current;
    if (from === phase || !document.hidden) return;
    const what = phase === 'approval' ? `notify_${pendingKind ?? 'permission'}` : phase === 'tui' ? 'notify_tui' : phase === 'idle' && from === 'running' ? 'notify_done' : null;
    if (!what) return;
    if (!mark.current) {
      mark.current = document.title;
      document.title = `● ${document.title}`;
    }
    if (typeof Notification !== 'undefined' && Notification.permission === 'granted') {
      try {
        // 这一轮做完了，但还有后台命令在跑：一起说
        const title = what === 'notify_done' && shells > 0 ? t('chat.notify_done_bg', { count: shells }) : t(`chat.${what}`);
        const n = new Notification(title, { body: mark.current, tag: `ll-chat-${id}` });
        n.onclick = () => {
          window.focus();
          n.close();
        };
      } catch {
        /* 有的浏览器（手机）不能直接 new Notification */
      }
    }
  }, [phase]);
  useEffect(() => {
    const back = () => {
      if (!document.hidden && mark.current) {
        if (document.title.startsWith('● ')) document.title = document.title.slice(2);
        mark.current = null;
      }
    };
    document.addEventListener('visibilitychange', back);
    return () => document.removeEventListener('visibilitychange', back);
  }, []);
}


/**
 * 停在底部时跟着新内容走。只有用户明确往上看（滚轮往上、手指往下拖、拖滚动条往上、按上翻键）才停止跟随，
 * 滚回到底部附近再接着跟。不能按“每次 scroll 事件时离底部多远”来判断：平滑滚动、惯性滚动的中途、
 * 内容在滚动过程中变高时都会误判，结果是滑到了底部又被弹回去、跟不上最新输出。
 * Markdown 是异步渲染的，高度会在之后才变，所以看尺寸（ResizeObserver）而不是看条目。
 */
function useFollow(list, inner, deps) {
  const stick = useRef(true);
  const [following, setFollowing] = useState(true);
  const pin = useCallback(() => {
    const el = list.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, []);
  useEffect(() => {
    const el = list.current;
    if (!el) return undefined;
    const set = (v) => {
      if (stick.current === v) return;
      stick.current = v;
      setFollowing(v);
    };
    let touchY = null;
    let dragging = false;
    let lastTop = el.scrollTop;
    const onWheel = (e) => e.deltaY < 0 && set(false);
    const onTouchStart = (e) => {
      touchY = e.touches[0]?.clientY ?? null;
    };
    const onTouchMove = (e) => {
      const y = e.touches[0]?.clientY;
      if (touchY != null && y != null && y > touchY + 3) set(false);
      touchY = y ?? touchY;
    };
    const onKey = (e) => ['ArrowUp', 'PageUp', 'Home'].includes(e.key) && set(false);
    // 拖滚动条：按下的位置就是滚动区域本身（不是里面的内容）
    const onDown = (e) => {
      dragging = e.target === el;
    };
    const onUp = () => {
      dragging = false;
    };
    const onScroll = () => {
      const gap = el.scrollHeight - el.scrollTop - el.clientHeight;
      if (gap < 24) set(true);
      else if (dragging && el.scrollTop < lastTop - 1) set(false);
      lastTop = el.scrollTop;
    };
    el.addEventListener('wheel', onWheel, { passive: true });
    el.addEventListener('touchstart', onTouchStart, { passive: true });
    el.addEventListener('touchmove', onTouchMove, { passive: true });
    el.addEventListener('keydown', onKey);
    el.addEventListener('pointerdown', onDown);
    window.addEventListener('pointerup', onUp);
    el.addEventListener('scroll', onScroll, { passive: true });
    let ro = null;
    if (typeof ResizeObserver !== 'undefined') {
      ro = new ResizeObserver(pin);
      ro.observe(el);
      if (inner.current) ro.observe(inner.current);
    }
    return () => {
      el.removeEventListener('wheel', onWheel);
      el.removeEventListener('touchstart', onTouchStart);
      el.removeEventListener('touchmove', onTouchMove);
      el.removeEventListener('keydown', onKey);
      el.removeEventListener('pointerdown', onDown);
      window.removeEventListener('pointerup', onUp);
      el.removeEventListener('scroll', onScroll);
      ro?.disconnect();
    };
  }, []);
  useEffect(pin, deps);
  const toBottom = useCallback(() => {
    stick.current = true;
    setFollowing(true);
    pin();
  }, []);
  return { following, toBottom, pin };
}

// ---------------- 整体 ----------------

/** 空白对话里的几个常用开头 */
const STARTERS = ['explain', 'review', 'tests'];

const SIDE_KEY = 'll-chat-side';
const readSide = () => {
  try {
    const v = localStorage.getItem(SIDE_KEY);
    return SIDE_TABS.includes(v) ? v : null;
  } catch {
    return null;
  }
};

/**
 * id：终端；active：当前显示的是“对话”标签；touch：手机（回车换行，用按钮发送）；
 * offline：终端所在的电脑连不上（对话照样显示，连不上时不能发送）；onTab(tab)：切到终端的别的标签（例如右侧“改动”里的“在 Git 里查看”）。
 * 对话页里不出现终端：Claude Code 的选择框显示成弹窗，界面做不了的就不做，用户自己切到“终端”标签去。
 */
export default function ChatView({ id, launch, active, touch, offline = false, onTab }) {
  const router = useRouter();
  const chat = useChat(id);
  const { status, items } = chat;
  // 状态还没取到（刚切过来、连不上）时按终端的启动类型，不能先显示成 Claude Code
  const agentId = status?.agent ?? (AGENT_NAMES[launch] ? launch : 'claude');
  const agentName = AGENT_NAMES[agentId] ?? AGENT_NAMES.claude;
  const { t } = useT(agentName);
  const caps = status?.caps;
  const agentCtx = useMemo(() => ({ id: agentId, name: agentName, caps: caps ?? {} }), [agentId, agentName, caps]);
  const phase = status?.phase ?? 'detached';
  const list = useRef(null);
  const inner = useRef(null);
  const todos = useMemo(() => latestTodos(items), [items]);
  const live = phase === 'running' || phase === 'approval';
  const bg = backgroundCounts(status);
  const [side, setSideState] = useState(readSide);
  const setSide = useCallback((v) => {
    setSideState(v);
    try {
      if (v) localStorage.setItem(SIDE_KEY, v);
      else localStorage.removeItem(SIDE_KEY);
    } catch {
      /* 记不住就只在这次打开里有效 */
    }
  }, []);
  // 模型菜单的选项、上下文窗口：模型变了重新取
  const modelKey = `${status?.actual_model ?? ''}|${status?.model ?? ''}|${phase === 'detached'}`;
  const { data: models } = usePoll(`/instances/${id}/chat/models`, { enabled: caps?.models !== false && status != null && phase !== 'detached', deps: [modelKey] });
  const { data: usage } = usePoll(`/instances/${id}/chat/usage`, { enabled: caps?.usage !== false && status != null, deps: [status?.actual_model, status?.context] });
  // 产出的文件：对话有新条目时重新取（只有 Codex 有）
  const { data: arts } = usePoll(`/instances/${id}/chat/artifacts`, { enabled: agentId === 'codex' && status != null, deps: [items.length] });
  const artifactCtx = useMemo(() => {
    const list = arts?.items ?? [];
    const links = {};
    for (const a of list) {
      const url = artifactUrl(id, a.path, !a.viewable);
      // 文字里的写法，加上完整路径的两种斜杠（Codex 引用文件时用正斜杠，路径里可能有空格）
      for (const m of [...(a.mentions ?? []), a.path, a.path.replaceAll('\\', '/')]) links[m] = url;
    }
    return { list, links: list.length ? links : null };
  }, [arts, id]);
  // 这一轮从什么时候开始跑的（运行中显示用了多久）；等审批时接着算
  const [since, setSince] = useState(null);
  useEffect(() => {
    if (phase === 'running' || phase === 'approval') setSince((x) => x ?? Date.now());
    else setSince(null);
  }, [phase]);
  const prev = useRef(phase);
  useAttention(id, phase, status?.pending?.kind, prev, bg.shell ?? 0, agentName);
  useEffect(() => {
    prev.current = phase;
  }, [phase]);
  const { following, toBottom } = useFollow(list, inner, [items, status?.pending?.id, active, side]);
  const toast = useToast();
  const onAction = (a) => {
    if (a === 'history') {
      if (router?.navigate) router.navigate('/history');
      else window.location.assign('/history');
    } else if (SIDE_TABS.includes(a)) setSide(a);
  };
  const compact = () => api.post(`/instances/${id}/chat/input`, { text: '/compact', images: [] }).catch((e) => toast(errorText(t, e), 'error'));
  const loading = useRef(false);
  // 往上翻到顶：加载更早的，保持看着的位置不动
  const onScroll = async (e) => {
    const el = e.currentTarget;
    if (el.scrollTop < 40 && chat.start > 0 && !loading.current) {
      loading.current = true;
      const before = el.scrollHeight;
      try {
        await chat.loadOlder();
      } finally {
        requestAnimationFrame(() => {
          el.scrollTop += el.scrollHeight - before;
          loading.current = false;
        });
      }
    }
  };
  if (chat.error && !status) {
    return (
      <div className="chat-view">
        <div className="chat-detached">
          <b>{t('chat.unavailable')}</b>
          <p className="muted">{errorText(t, chat.error)}</p>
        </div>
      </div>
    );
  }
  return (
    <AgentCtx.Provider value={agentCtx}>
      <ArtifactCtx.Provider value={artifactCtx}>
    <div className="chat-view" data-side={side ? '' : undefined}>
      <div className="chat-col">
        <div className="chat-main">
          <div className="chat-list" ref={list} onScroll={onScroll} tabIndex={-1}>
            <div className="chat-items" ref={inner}>
              {chat.start > 0 && <p className="chat-event">{t('chat.older')}</p>}
              {!items.length && status && phase !== 'detached' && (
                <div className="chat-empty">
                  <span className="chat-empty-mark" aria-hidden="true">
                    <Asterisk size={40} />
                  </span>
                  <p className="chat-empty-title">{t('chat.empty')}</p>
                  {/* 几个常用的开头：点了填进输入框，可以改了再发 */}
                  <div className="chat-starters">
                    {STARTERS.map((k) => (
                      <button key={k} type="button" className="chat-starter" onClick={() => window.dispatchEvent(new CustomEvent(`ll-chat-fill:${id}`, { detail: t(`chat.starter.${k}`) }))}>
                        {t(`chat.starter.${k}`)}
                      </button>
                    ))}
                  </div>
                  <p className="chat-note">{t('chat.beta_note')}</p>
                </div>
              )}
              <Rows id={id} items={items} live={live} />
              {phase === 'running' && <Live status={status} items={items} since={since} />}
              {/* 推荐的下一句放在对话最后：点一下填进输入框（电脑上也可以按 Tab） */}
              {phase === 'idle' && status?.suggestion && items.length > 0 && (
                <div className="chat-next-row">
                  <button type="button" className="chat-next" onClick={() => window.dispatchEvent(new CustomEvent(`ll-chat-fill:${id}`, { detail: status.suggestion }))} title={t('chat.suggest_hint')}>
                    <Sparkle size={14} aria-hidden="true" />
                    <span className="chat-next-label">{t('chat.next_step')}</span>
                    <span className="ellipsis">{status.suggestion}</span>
                    <ArrowRight size={14} aria-hidden="true" />
                  </button>
                </div>
              )}
              {status?.pending && <PendingCard id={id} pending={status.pending} active={active} />}
              {/* 这一轮做完了，后台还有命令、子代理在跑 */}
              {phase === 'idle' && (bg.shell > 0 || bg.subagent > 0) && (
                <button type="button" className="chat-event chat-bg-note" onClick={() => setSide('tasks')}>
                  <LoaderCircle size={13} className="spin" aria-hidden="true" />
                  {[bg.subagent > 0 && t('chat.bg_agents_still', { count: bg.subagent }), bg.shell > 0 && t('chat.bg_shells_still', { count: bg.shell })].filter(Boolean).join(t('chat.and'))}
                  <span className="chat-more">{t('chat.view')}</span>
                </button>
              )}
              {status?.stats?.failed > 0 && (
                <p className="chat-event">
                  <TriangleAlert size={13} aria-hidden="true" /> {t('chat.format_warning', { version: status.version ?? '?' })}
                </p>
              )}
            </div>
          </div>
          {!following && (
            <button type="button" className="chat-to-bottom" onClick={toBottom} aria-label={t('chat.to_bottom')}>
              <ArrowDown size={16} />
            </button>
          )}
          {status && <SideToggle side={side} onSide={setSide} />}
          {phase === 'tui' && <TuiDialog id={id} dialog={status?.dialog ?? null} limit={status?.limit} />}
        </div>
        <div className="chat-dock">
          {todos && todos.some((x) => x.status !== 'completed') && (
            <details className="chat-dock-todos">
              <summary>
                <ListTodo size={14} aria-hidden="true" /> {t('chat.todos', { done: todos.filter((x) => x.status === 'completed').length, total: todos.length })}
                <span className="ellipsis chat-note">{todos.find((x) => x.status === 'in_progress')?.activeForm ?? ''}</span>
              </summary>
              <TodoList todos={todos} />
            </details>
          )}
          {phase === 'detached' && <Detached id={id} status={status} />}
          {status?.limit?.waiting && phase !== 'tui' && <LimitWait id={id} waiting={status.limit.waiting} />}
          <StatusNotice status={status} online={chat.online} offline={offline} />
          <Composer id={id} status={status} phase={phase} touch={touch} offline={offline && !chat.online} models={models} usage={usage} onAction={onAction} onSide={setSide} />
        </div>
      </div>
      {side && <SidePanel id={id} tab={side} onTab={setSide} onClose={() => setSide(null)} status={status} items={items} onTermTab={(tab) => onTab?.(tab)} onCompact={compact} />}
    </div>
      </ArtifactCtx.Provider>
    </AgentCtx.Provider>
  );
}
