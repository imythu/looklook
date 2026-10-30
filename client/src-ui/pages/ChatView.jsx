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
  Copy,
  Eye,
  EyeOff,
  Maximize2,
  Minimize2,
  CircleDashed,
  CircleStop,
  CornerDownRight,
  ExternalLink,
  FilePen,
  FileText,
  FolderGit2,
  GitBranch,
  GitFork,
  Globe,
  History,
  Hourglass,
  Image as ImageIcon,
  Download,
  Expand,
  ZoomIn,
  ZoomOut,
  Layers,
  ListTodo,
  LoaderCircle,
  MessageCircleQuestion,
  PanelRight,
  Plug,
  Plus,
  Search,
  ShieldAlert,
  ShieldCheck,
  Sparkle,
  Square,
  SquareTerminal,
  TriangleAlert,
  WifiOff,
  Wrench,
  X,
  Zap,
  UserRound,
} from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { api, deviceApi, fetchRaw, plainLinksWork, terminalDevice } from '../shared/api.js';
import { chatEventsMode, onChatEventsMode, subscribeChat } from '../shared/chatEvents.js';
import { spinnerText } from '../shared/spinner.js';
import {
  CODEX_COMMANDS,
  CODEX_ONLY,
  COMMANDS,
  EFFORTS,
  EMPTY,
  MODE_CHOICES,
  TERMINAL_ONLY,
  applyEvent,
  backgroundCounts,
  chatRows,
  conversationAgents,
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
  maskEmail,
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
import { copyText } from '../shared/chatCopy.js';
import ErrorBoundary from '../shared/ErrorBoundary.jsx';
import { errorText } from '../shared/i18n.js';
import Markdown from '../shared/Markdown.jsx';
import { useRouter } from '../shared/router.jsx';
import { useSessions } from '../shared/sessions.jsx';
import { formatTime, parseInstant, relativeTime, spanText } from '../shared/time.js';
import { dropFirst, retryFirst, useFirstSend } from '../shared/firstSend.js';
import { runUpload } from '../shared/uploader.js';
import { inputHistory, rememberInput, searchInputs } from '../shared/inputHistory.js';
import PopMenu from '../shared/PopMenu.jsx';
import { Button, CopyButton, useConfirm, useLoad, useToast } from '../shared/ui.jsx';
import { KindIcon, profileName } from './ClaudeProfiles.jsx';
import { FindBar, MessageSheet, OutlineTab, UserActs, fillInput, useChatKeys, useLongPress } from './ChatTools.jsx';
import { LaunchIcon } from '../shared/brands.jsx';
import { ChatHowTo, NotifyAsk } from './ChatTips.jsx';
import { endpoints as fileEndpoints, Preview as FilePreview } from './FileBrowser.jsx';
import { imeComposing } from '../shared/ime.js';
import { shownFocus } from '../shared/termFrame.js';

// ---------------- 数据 ----------------

/** 服务端状态里的 `agent` → 文案里的 {{agent}}；老版本服务端不带 agent，就是 Claude Code */
const AGENT_NAMES = { claude: 'Claude Code', codex: 'Codex' };
/** 这个对话的 AI 工具和它支持的操作（`status.caps`；老版本服务端不带 caps，就是全部可用）：界面只按它显示按钮 */
const AgentCtx = createContext({ id: 'claude', name: AGENT_NAMES.claude, caps: {} });
/** 这段对话产出的文件（Codex）：list 给资源栏，links（文字里的写法 → 地址）给回复里的路径变成链接 */
const ArtifactCtx = createContext({ list: [], links: null, fileUrl: null });

/** 在右侧面板里打开一个子代理的步骤：`openAgent({ id, tool_use_id, … })`（ChatView 提供；对话里的子代理卡片、运行中的子代理条用） */
const SideCtx = createContext({ openAgent: null });

/** 产出的文件的地址：图片、PDF、音视频、文本直接在浏览器里看，其余下载 */
const artifactUrl = (id, path, download) => `/api/instances/${id}/chat/artifact?path=${encodeURIComponent(path)}${download ? '&download=1' : ''}`;
/**
 * 回复里链接到这台电脑上其他文件（不是这段对话产出的）：文字和图片和“文件”标签一样用预览打开（浏览器里直接看），
 * 其他（PDF、Office、压缩包……）预览只会显示成乱码，直接下载
 */
const localFileUrl = (id, path) => {
  const kind = /\.(pdf|docx?|xlsx?|pptx?|zip|7z|rar|gz|tgz|tar|bz2|xz|dmg|exe|msi|pkg|apk|mp3|mp4|mov|wav|webm|sqlite|db)$/i.test(path) ? 'download' : 'preview';
  return `/api/instances/${id}/${kind}?path=${encodeURIComponent(path)}`;
};

/** 和 useTranslation 一样，但文案里的 {{agent}} 换成这个对话的 AI 工具（`name` 给了就用它，否则看上层的 AgentCtx） */
function useT(name) {
  const { t, i18n } = useTranslation();
  const ctx = useContext(AgentCtx);
  const agent = name ?? ctx.name;
  return { t: useCallback((key, opts) => t(key, { agent, ...opts }), [t, agent]), i18n };
}

/**
 * Codex（app-server）运行中的那一行：`thinking`、`compacting`、重连提示换成中文（原文跟在后面），
 * 正在跑的命令放进后面的小胶囊（主文字说在做什么）。拆法和 Claude 的 `spinnerText` 一样：`{ text, detail, orig }`
 */
const RETRY_RE = /^Reconnecting\.*\s*(\d+)\s*\/\s*(\d+)/i;
function codexActivity(t, activity, tool) {
  const s = (activity ?? '').trim();
  if (s === 'compacting') return { text: t('chat.compacting'), detail: '', orig: '' };
  if (s === 'thinking') return { text: t('chat.codex_thinking'), detail: '', orig: '' };
  if (s === 'waiting_agents') return { text: t('chat.codex_waiting_agents'), detail: '', orig: '' };
  const retry = s.match(RETRY_RE);
  if (retry) return { text: t('chat.codex_retry', { n: retry[1], total: retry[2] }), detail: '', orig: s };
  if (s && tool === 'Bash') return { text: t('chat.codex_running_cmd'), detail: s, orig: '', code: true };
  if (s && tool === 'mcp') return { text: t('chat.running_tool', { tool: s }), detail: '', orig: '' };
  if (!s && tool === 'Edit') return { text: t('chat.codex_editing'), detail: '', orig: '' };
  if (!s && tool === 'WebSearch') return { text: t('chat.codex_searching'), detail: '', orig: '' };
  return { text: s, detail: '', orig: '' };
}

/**
 * 订阅对话事件（怎么收见 shared/chatEvents.js）。多路 WebSocket 断线、订阅出错时它自己重连、重新订阅，这里取一次快照拿到错误；
 * SSE 连不上时 EventSource 自己重连，服务端拒绝（4xx）时它会放弃，这里隔几秒再试，并取一次快照拿到错误。
 * WebSocket 用不了、退回 SSE 时只给显示着的对话（`active`）开，免得把浏览器同一地址的 6 条连接占满。
 */
function useChat(id, active) {
  const [state, dispatch] = useReducer((s, a) => applyEvent(s, a.type, a.data), EMPTY);
  const [online, setOnline] = useState(false);
  const [error, setError] = useState(null);
  const [mode, setMode] = useState(chatEventsMode);
  useEffect(() => onChatEventsMode(setMode), []);
  const listen = mode === 'sse' || (mode === 'fallback' && active);
  useEffect(() => {
    if (mode !== 'mux' && !listen) return undefined;
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
    const on = (type, data) => {
      if (type === 'snapshot') {
        setOnline(true);
        setError(null);
      }
      if (type === 'reset') snapshot();
      else dispatch({ type, data });
    };
    if (mode === 'mux') {
      const off = subscribeChat(id, {
        event: on,
        down: () => {
          if (!alive) return;
          setOnline(false);
          snapshot();
        },
      });
      return () => {
        alive = false;
        off();
      };
    }
    const connect = () => {
      es = new EventSource(`/api/instances/${encodeURIComponent(id)}/chat/events`, { withCredentials: true });
      for (const type of ['snapshot', 'status', 'items', 'reset']) {
        es.addEventListener(type, (e) => {
          let data;
          try {
            data = JSON.parse(e.data);
          } catch {
            return; /* 坏数据：等下一个快照 */
          }
          on(type, data);
        });
      }
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
  }, [id, mode, listen]);
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

/** 一块文字，右上角有复制（电脑上悬停出现，手机上一直在） */
function Copyable({ text, children }) {
  if (!text) return children;
  return (
    <div className="chat-copyable">
      {children}
      <span className="chat-copyable-btn">
        <CopyButton text={text} label={false} variant="plain" />
      </span>
    </div>
  );
}

/** 工具结果、参数里的一段文字：整段是 JSON 时格式化显示，网址可以点；可以复制 */
function RichText({ text, className = '' }) {
  if (!text) return null;
  const json = parseJson(text);
  if (json) return <JsonBlock value={json} raw={text} className={className} />;
  return (
    <Copyable text={text}>
      <pre className={`chat-pre ${className}`}>
        <Linked text={text} />
      </pre>
    </Copyable>
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
        // Codex 的子代理最后是“这一轮用了几秒”，由服务端说做没做完
        if (!(d.done ?? agentDone(d.items))) timer = setTimeout(load, 2000);
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
  return { data, items, error, done: data?.done ?? agentDone(items) };
}

/** 子代理的步骤一次先画这么多（每一步都可能展开成 diff），更早的点了再显示 */
const SUB_STEPS = 25;

/**
 * 工具卡片里子代理的步骤：卡片展开时就加载，左边一条竖线串起来；步骤多时只画最近的，更早的点了再显示。
 * 时间线和“再显示更多”参照 CloudCLI UI 的 SubagentTimeline.tsx。
 */
function SubAgent({ id, block, result }) {
  const { t } = useT();
  const { openAgent } = useContext(SideCtx);
  const [open, setOpen] = useState(true);
  // Codex 的子代理：子会话 id 在参数里，还没交回结果时也能看步骤
  const agent = result?.extra?.agentId ?? block.input?.agent_thread_id;
  const { data, items, error, done } = useAgentSteps(id, { agent, tool: block.id, enabled: open });
  return (
    <div className="chat-sub">
      <div className="chat-sub-acts">
        <button type="button" className="chat-more" onClick={() => setOpen(!open)} aria-expanded={open}>
          {open ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />} {t('chat.sub_steps')}
          {open && !done && <LoaderCircle size={13} className="spin" aria-hidden="true" />}
        </button>
        {openAgent && (
          <button type="button" className="chat-more" onClick={() => openAgent({ id: agent ?? null, tool_use_id: block.id, description: block.input?.description, type: block.input?.subagent_type, model: block.input?.model, prompt: block.input?.prompt })}>
            <PanelRight size={13} aria-hidden="true" /> {t('chat.agent_open')}
          </button>
        )}
      </div>
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
          <Copyable text={input.command ?? ''}>
            <Pre text={`$ ${input.command ?? ''}`} className="cmd" />
          </Copyable>
          <ResultText id={id} result={result} />
        </>
      );
    case 'Read':
      return <ReadBody id={id} block={block} result={result} />;
    case 'Edit':
    case 'MultiEdit':
    case 'Write':
      return <EditBody id={id} block={block} result={result} />;
    case 'TodoWrite':
      return <TodoList todos={input.todos ?? []} />;
    case 'Task':
    case 'Agent':
      return (
        <>
          {/* Codex 的任务说明是加密的，没有 prompt */}
          {input.prompt && <Pre text={input.prompt} />}
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

/** 改动：diff，下面可以打开改后的文件（停在第一处改动） */
function EditBody({ id, block, result }) {
  const { t } = useT();
  const { open: openFile, cwd } = useContext(PreviewCtx);
  const lines = diffLines(block.name, block.input ?? {}, result?.extra);
  const path = block.input?.file_path && absPath(block.input.file_path, cwd);
  // 第一段里改后的行：有行号才知道在哪
  let focus = null;
  if (block.name !== 'Write') {
    for (const l of lines) {
      if (l.t === '@' && focus) break;
      if (l.t === '+' && l.no) focus = { from: focus?.from ?? l.no, to: l.no };
    }
  }
  return (
    <>
      <DiffView lines={lines} />
      {result?.is_error && <ResultText id={id} result={result} />}
      {openFile && path && result && !result.is_error && (
        <div className="chat-read-acts">
          <button type="button" className="chat-more" onClick={() => openFile({ ep: fileEndpoints(id), path, focus })}>
            <Expand size={13} aria-hidden="true" /> {t('chat.edit_open')}
          </button>
        </div>
      )}
    </>
  );
}

const IMAGE_FILE = /\.(png|jpe?g|gif|webp|bmp|svg|avif|ico)$/i;

/**
 * 读文件（Claude 的 Read；Codex 看图片也记成 Read）展开后：图片直接显示，点开看大图；
 * 别的文件没有读到的文字时（Codex）直接内嵌预览，有的时候（Claude）先显示读到的那段，可以切到预览整个文件。
 * 下面一行可以全屏查看（页面里的文件预览）。
 */
function ReadBody({ id, block, result }) {
  const { t } = useT();
  const { open: openFile, cwd } = useContext(PreviewCtx);
  const path = block.input?.file_path && absPath(block.input.file_path, cwd);
  const text = Boolean(result?.content?.trim());
  const [whole, setWhole] = useState(!text);
  const numbered = useMemo(() => (text && !result.is_error ? readLines(result.content) : null), [text, result]);
  if (!path) return <ResultText id={id} result={result} />;
  const ep = fileEndpoints(id);
  const image = IMAGE_FILE.test(path);
  // 读的是哪几行：参数里的 offset、limit，没有时按读到的行号
  const from = Number(block.input?.offset) || numbered?.lines[0]?.no || 0;
  const to = block.input?.limit ? from + Number(block.input.limit) - 1 : numbered?.lines[numbered.lines.length - 1]?.no ?? from;
  const focus = from > 1 || block.input?.limit ? { from: Math.max(1, from), to } : null;
  const open = (line) => openFile?.({ ep, path, focus: line ? { from: line, to: line } : focus });
  return (
    <>
      {image ? (
        <ReadImage id={id} path={path} />
      ) : whole ? (
        <div className="chat-read-preview">
          <FilePreview ep={ep} entry={{ name: fileName(path), path }} sep={pathSep(path)} numbered focus={focus} onClose={() => (text ? setWhole(false) : open())} />
        </div>
      ) : numbered ? (
        <ReadLines id={id} result={result} read={numbered} onLine={openFile ? open : null} />
      ) : (
        <ResultText id={id} result={result} />
      )}
      <div className="chat-read-acts">
        {!image && text && (
          <button type="button" className="chat-more" onClick={() => setWhole(!whole)}>
            {whole ? t('chat.read_show_read') : t('chat.read_show_file')}
          </button>
        )}
        {openFile && (
          <button type="button" className="chat-more" onClick={() => open()}>
            <Expand size={13} aria-hidden="true" /> {t('chat.read_full')}
          </button>
        )}
      </div>
    </>
  );
}

/** Claude 读文件的结果是带行号的（`     1→…` 或 `1\t…`）：拆成行号和文字，后面不是这种格式的（提醒之类）单独留着 */
const READ_LINE = /^\s*(\d+)(?:→|\t)(.*)$/;
function readLines(content) {
  const all = content.split('\n');
  const lines = [];
  let n = 0;
  for (; n < all.length; n++) {
    const m = all[n].match(READ_LINE);
    if (!m) break;
    lines.push({ no: Number(m[1]), text: m[2] });
  }
  if (lines.length < 2) return null;
  return { lines, rest: all.slice(n).join('\n').trim() };
}

/** 读到的那段：带行号的代码块，点行号在大窗里打开、停在那一行 */
function ReadLines({ id, result, read, onLine }) {
  const { t } = useT();
  const text = read.lines.map((l) => l.text).join('\n');
  const [all, setAll] = useState(false);
  const shown = all ? read.lines : read.lines.slice(0, DIFF_LIMIT);
  return (
    <>
      <Copyable text={text}>
        <div className="chat-pre chat-read-lines">
          {shown.map((l) => (
            <div key={l.no}>
              {onLine ? (
                <button type="button" className="chat-read-no" onClick={() => onLine(l.no)} title={t('chat.read_open_line', { n: l.no })}>
                  {l.no}
                </button>
              ) : (
                <span className="chat-read-no">{l.no}</span>
              )}
              <span>{l.text || ' '}</span>
            </div>
          ))}
        </div>
      </Copyable>
      {!all && read.lines.length > DIFF_LIMIT && (
        <button type="button" className="chat-more" onClick={() => setAll(true)}>
          {t('chat.show_all_lines', { n: read.lines.length })}
        </button>
      )}
      {result.truncated && <ResultText id={id} result={{ ...result, content: '' }} />}
    </>
  );
}

/** 读到的图片：缩略图，点开看大图（能缩放、下载） */
function ReadImage({ id, path }) {
  const { t } = useT();
  const url = `/instances/${id}/download?path=${encodeURIComponent(path)}`;
  const src = useGenSrc(url);
  const [big, setBig] = useState(false);
  const [broken, setBroken] = useState(false);
  if (broken) return <p className="chat-note">{t('chat.read_image_gone')}</p>;
  return (
    <>
      <button type="button" className="chat-read-img" onClick={() => setBig(true)} disabled={!src} aria-label={t('chat.res_view')} title={path}>
        {src ? <img src={src} alt={fileName(path)} loading="lazy" decoding="async" onError={() => setBroken(true)} /> : <LoaderCircle size={16} className="spin" aria-hidden="true" />}
      </button>
      {big && <Lightbox urls={[url]} names={[fileName(path)]} index={0} onIndex={() => {}} onClose={() => setBig(false)} />}
    </>
  );
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
  const [big, setBig] = useState(false);
  return (
    <figure className="chat-gen-fig">
      <button type="button" className="chat-gen-zoom" onClick={() => setBig(true)} disabled={!src} aria-label={t('chat.res_view')} title={t('chat.res_view')}>
        <GenImage src={src} name={name} />
      </button>
      {big && <Lightbox urls={[genFileUrl(id, name)]} names={[name]} index={0} onIndex={() => {}} onClose={() => setBig(false)} />}
      <figcaption className="chat-gen-acts">
        <span className="chat-note ellipsis" title={caption || name}>
          {caption || name}
        </span>
        {src && (
          <button type="button" className="btn btn-plain btn-sm" onClick={() => setBig(true)}>
            {t('chat.res_view')}
          </button>
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
  const { links, fileUrl } = useContext(ArtifactCtx);
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
        if (live && !next) return <Markdown key={r.key} text={r.block.text} links={links} fileUrl={fileUrl} />;
        if (next && next.k !== 'item') return <Markdown key={r.key} text={r.block.text} links={links} fileUrl={fileUrl} />;
        let from = n;
        while (from > 0 && rows[from - 1].k !== 'item') from--;
        const all = rows.slice(from, n + 1).filter((x) => x.k === 'text').map((x) => x.block.text).join('\n\n');
        // 这一轮用了多久跟在复制按钮后面（不再单独占一行）
        const turn = next?.item?.k === 'turn' ? next.item : null;
        return <ReplyEnd key={r.key} text={r.block.text} all={all} turn={turn} />;
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

/** 一段回复的最后一段文字，下面是复制和用时；手机上长按这一段弹出复制菜单 */
function ReplyEnd({ text, all, turn }) {
  const { t } = useT();
  const { links, fileUrl } = useContext(ArtifactCtx);
  const [sheet, setSheet] = useState(false);
  const press = useLongPress(() => setSheet(true));
  return (
    <div className="chat-reply-end" {...press}>
      <Markdown text={text} links={links} fileUrl={fileUrl} />
      <div className="chat-reply-acts">
        <CopyButton text={all} label={false} variant="plain" />
        {turn && <span className="chat-turn">{t('chat.turn', { s: Math.max(1, Math.round(turn.ms / 1000)) })}</span>}
      </div>
      {sheet && <MessageSheet text={all} onClose={() => setSheet(false)} />}
    </div>
  );
}

/** 用户消息：电脑上悬停出现复制、填回输入框；手机上长按弹出菜单 */
function UserItem({ id, item, thumbs }) {
  const { t } = useT();
  const [sheet, setSheet] = useState(false);
  const text = item.text.trim();
  const press = useLongPress(() => text && setSheet(true));
  return (
    <div className="chat-user" data-mid={item.mid ? '' : undefined} data-key={item.key}>
      {/* Claude 在忙时发的：这一步做完就并进了这一轮 */}
      {item.mid && (
        <span className="chat-mid-tag">
          <CornerDownRight size={12} aria-hidden="true" />
          {t('chat.mid_tag')}
        </span>
      )}
      {/* 图片小图：从记录里取（Claude 的 transcript、Codex 的 rollout 里都有图片本身） */}
      {thumbs && <UserImages id={id} itemKey={item.key} n={item.images} />}
      {(text || (item.images > 0 && !thumbs)) && (
        <div className="chat-bubble" {...press}>
          {item.text}
          {item.images > 0 && !thumbs && <span className="chat-note">{t('chat.images', { n: item.images })}</span>}
        </div>
      )}
      {text && <UserActs id={id} text={text} />}
      {sheet && <MessageSheet text={text} onFill={() => fillInput(id, text)} onClose={() => setSheet(false)} />}
    </div>
  );
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

/** 全屏看大图：点空白处、Esc 关闭；多张时左右切换（按钮、方向键、手机上左右滑）。
 *  点图片放大到原始大小（可以拖动滚动看细节），再点回到适应屏幕；`names` 给了就显示文件名、能下载 */
function Lightbox({ urls, names, index, onIndex, onClose }) {
  const { t } = useT();
  const src = useGenSrc(urls[index]);
  const name = names?.[index];
  const many = urls.length > 1;
  const [zoom, setZoom] = useState(false);
  useEffect(() => setZoom(false), [index]);
  const go = (d) => onIndex((index + d + urls.length) % urls.length);
  const touch = useRef(null);
  useEffect(() => {
    const key = (e) => {
      if (e.key === 'Escape') onClose();
      else if (many && e.key === 'ArrowLeft') go(-1);
      else if (many && e.key === 'ArrowRight') go(1);
      else if (e.key === '+' || e.key === '=' || e.key === '-' || e.key === '0') setZoom(e.key === '+' || e.key === '=');
      else return;
      e.preventDefault();
      e.stopPropagation();
    };
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  });
  return createPortal(
    <div
      className={`chat-lightbox ${zoom ? 'zoomed' : ''}`}
      role="dialog"
      aria-modal="true"
      aria-label={name ?? t('chat.image_n', { n: index + 1, total: urls.length })}
      onClick={(e) => e.target === e.currentTarget && onClose()}
      onTouchStart={(e) => (touch.current = e.touches[0].clientX)}
      onTouchEnd={(e) => {
        const dx = e.changedTouches[0].clientX - (touch.current ?? 0);
        if (many && !zoom && Math.abs(dx) > 50) go(dx < 0 ? 1 : -1);
      }}
    >
      <div className="chat-lightbox-bar">
        {many && <span className="chat-lightbox-count">{index + 1} / {urls.length}</span>}
        {name && <span className="chat-lightbox-name ellipsis" title={name}>{name}</span>}
        <span className="grow" />
        {src && (
          <button type="button" className="chat-lightbox-btn" onClick={() => setZoom(!zoom)} aria-pressed={zoom} aria-label={t(zoom ? 'chat.image_fit' : 'chat.image_actual')} title={t(zoom ? 'chat.image_fit' : 'chat.image_actual')}>
            {zoom ? <ZoomOut size={18} /> : <ZoomIn size={18} />}
          </button>
        )}
        {src && name && (
          <a className="chat-lightbox-btn" href={src} download={name} aria-label={t('chat.res_download')} title={t('chat.res_download')}>
            <Download size={18} />
          </a>
        )}
        {src && (
          <a className="chat-lightbox-btn" href={src} target="_blank" rel="noopener noreferrer" aria-label={t('chat.res_open')} title={t('chat.res_open')}>
            <ExternalLink size={18} />
          </a>
        )}
        <button type="button" className="chat-lightbox-btn" onClick={onClose} aria-label={t('action.close')} title={t('action.close')} autoFocus>
          <X size={20} />
        </button>
      </div>
      {src ? (
        <div className="chat-lightbox-stage" onClick={(e) => e.target === e.currentTarget && onClose()}>
          <img key={src} className="chat-lightbox-img" src={src} alt={name ?? ''} onClick={() => setZoom(!zoom)} />
        </div>
      ) : (
        <LoaderCircle size={28} className="spin chat-lightbox-wait" aria-hidden="true" />
      )}
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

/** 链接地址 → 预览用的接口和文件路径：`/api/instances/{id}/preview|download?path=`（对话里提到的文件）、
 *  `/api/instances/{id}/chat/artifact?path=`（Codex 产出的文件） */
function previewTarget(href) {
  let u;
  try {
    u = new URL(href, window.location.origin);
  } catch {
    return null;
  }
  if (u.origin !== window.location.origin) return null;
  const m = u.pathname.match(/^\/api\/instances\/([^/]+)\/(preview|download|chat\/artifact)$/);
  const path = u.searchParams.get('path');
  if (!m || !path) return null;
  const id = decodeURIComponent(m[1]);
  if (m[2] !== 'chat/artifact') return { ep: fileEndpoints(id), path };
  const art = (p, dl) => `/instances/${id}/chat/artifact?path=${encodeURIComponent(p)}${dl ? '&download=1' : ''}`;
  return { ep: { preview: (p) => art(p), download: (p) => art(p, true), plain: plainLinksWork() }, path };
}

/** 路径用的分隔符、文件名（Windows 路径里只有反斜杠） */
const pathSep = (path) => (path.includes('\\') && !path.includes('/') ? '\\' : '/');
const fileName = (path) => path.split(/[\\/]/).pop() || path;

/**
 * 打开页面里的文件预览：`open({ ep, path, focus?, list? })`（focus：标出的行 { from, to }；list：前后翻的那组文件）；
 * `cwd` 是对话所在的目录，补全相对路径；`files`：对话里读过、改过的文件（ChatView 提供）
 */
const PreviewCtx = createContext({ open: null, cwd: null, files: null });

/** 相对路径按对话所在的目录补全 */
function absPath(path, cwd) {
  if (!cwd || /^([\\/]|[A-Za-z]:[\\/]|~)/.test(path)) return path;
  const sep = pathSep(cwd);
  return `${cwd.replace(/[\\/]+$/, '')}${sep}${path.replace(/^\.[\\/]/, '')}`;
}

/** 对话里读过、改过的文件（按最后一次碰到的先后，最近的在后），文件查看器里前后翻用 */
function conversationFiles(items, cwd) {
  const out = new Map();
  for (const it of items) {
    if (it.k !== 'assistant') continue;
    for (const b of it.blocks ?? []) {
      const p = b.t === 'tool_use' && FILE_TOOLS.has(b.name) && (b.input?.file_path ?? b.input?.notebook_path);
      if (!p || typeof p !== 'string') continue;
      const abs = absPath(p, cwd);
      out.delete(abs);
      out.set(abs, true);
    }
  }
  return [...out.keys()];
}
const FILE_TOOLS = new Set(['Read', 'Edit', 'MultiEdit', 'Write', 'NotebookEdit']);

/**
 * 对话里点开的文件：全屏（手机）/ 居中大窗（电脑）。文字带行号，`focus` 那几行标出来并滚过去（读的那段、改的地方）；
 * 能复制路径；`list`（同一组文件：改动列表，或者对话里读过、改过的文件）里有它时，标题栏和 ← → 前后翻。
 */
function FileViewer({ target, onTarget, onClose }) {
  const { t } = useT();
  const { files } = useContext(PreviewCtx);
  const { path, ep, focus } = target;
  const sep = pathSep(path);
  const name = fileName(path);
  const list = (target.list ?? files ?? []).includes(path) ? target.list ?? files : null;
  const at = list ? list.indexOf(path) : -1;
  const go = useCallback(
    (d) => {
      const next = list?.[at + d];
      if (next) onTarget({ ep, path: next, list: target.list });
    },
    [list, at, ep, target.list, onTarget],
  );
  useEffect(() => {
    const key = (e) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        onClose();
        return;
      }
      // ← →：前后翻（在输入框里、按着修饰键时不管）
      if ((e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') || e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || e.target.closest?.('input, textarea')) return;
      if (!list || list.length < 2) return;
      e.preventDefault();
      e.stopPropagation();
      go(e.key === 'ArrowLeft' ? -1 : 1);
    };
    window.addEventListener('keydown', key, true);
    return () => window.removeEventListener('keydown', key, true);
  }, [onClose, go, list]);
  const extra = (
    <>
      {list && list.length > 1 && (
        <span className="chat-fv-nav">
          <button type="button" className="btn btn-plain btn-icon" onClick={() => go(-1)} disabled={at <= 0} aria-label={t('chat.file_prev')} title={`${t('chat.file_prev')} (←)`}>
            <ChevronLeft size={17} />
          </button>
          <span className="chat-note">
            {at + 1}/{list.length}
          </span>
          <button type="button" className="btn btn-plain btn-icon" onClick={() => go(1)} disabled={at >= list.length - 1} aria-label={t('chat.file_next')} title={`${t('chat.file_next')} (→)`}>
            <ChevronRight size={17} />
          </button>
        </span>
      )}
      <CopyPath path={path} />
    </>
  );
  return createPortal(
    <div className="chat-file-viewer" role="dialog" aria-modal="true" aria-label={name} onClick={(e) => e.target === e.currentTarget && onClose()}>
      <FilePreview key={`${path}:${focus?.from ?? ''}`} ep={ep} entry={{ name, path }} sep={sep} onClose={onClose} numbered focus={focus} extra={extra} />
    </div>,
    document.body,
  );
}

/** 复制文件的完整路径（图标按钮，复制好了打个勾） */
function CopyPath({ path }) {
  const { t } = useT();
  const [done, setDone] = useState(false);
  useEffect(() => {
    if (!done) return undefined;
    const timer = setTimeout(() => setDone(false), 1500);
    return () => clearTimeout(timer);
  }, [done]);
  const copy = async () => {
    if (await copyText(path)) setDone(true);
    else window.prompt('', path);
  };
  return (
    <button type="button" className="btn btn-plain btn-icon" onClick={copy} aria-label={t('chat.copy_path')} title={`${t('chat.copy_path')}\n${path}`}>
      {done ? <Check size={16} /> : <Copy size={16} />}
    </button>
  );
}

/** 对话里点到的这台电脑上的文件：在页面里预览（和“文件”页同一个预览），不用跳到新标签页或者直接下载。
 *  按着 Ctrl / ⌘ / Shift 点、或者中键点，照旧按链接本来的方式打开 */
function usePreviewLinks() {
  const [target, setTarget] = useState(null);
  const onClick = useCallback((e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target.closest?.('a[href]');
    if (!a) return;
    const tgt = previewTarget(a.getAttribute('href'));
    if (!tgt) return;
    e.preventDefault();
    setTarget(tgt);
  }, []);
  const close = useCallback(() => setTarget(null), []);
  const viewer = target && <FileViewer target={target} onTarget={setTarget} onClose={close} />;
  return { onClick, open: setTarget, viewer };
}

/** 不是 assistant 回复的条目（用户消息、单独的工具结果、分隔线、提示……） */
function Item({ id, item }) {
  const { t } = useT();
  const { id: agent, caps } = useContext(AgentCtx);
  // 老版本的 Codex 端取不出图片（不带 caps.user_images）：照旧只写几张
  const thumbs = item.images > 0 && (agent === 'claude' || caps?.user_images === true);
  switch (item.k) {
    case 'user':
      return <UserItem id={id} item={item} thumbs={thumbs} />;
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
      return (
        <Copyable text={item.text}>
          <Pre text={item.text} className="chat-command" />
        </Copyable>
      );
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
      if (e.key !== 'Enter' || e.shiftKey || imeComposing(e)) return;
      const el = shownFocus();
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
    // 命令规则拦下的命令要点按钮确认，不能顺手按回车放过去
    if (kind === 'permission' && !pending.guard) answer('allow');
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
        {pending.guard && (
          <p className="chat-guard">
            <ShieldAlert size={14} aria-hidden="true" />
            <span>{t('chat.guard_hit', { rule: pending.guard })}</span>
          </p>
        )}
        <div className="chat-acts">
          <Button busy={busy} onClick={() => answer('allow')}>
            {pending.guard ? t('chat.guard_allow') : t('chat.allow')} {!pending.guard && <kbd className="chat-kbd">↵</kbd>}
          </Button>
          {caps.allow_session !== false && !pending.guard && (
            <Button variant="secondary" busy={busy} onClick={() => answer('allow_session')} title={t('chat.allow_session_hint')}>
              {t('chat.allow_session')}
            </Button>
          )}
          {caps.allow_dirs !== false && !pending.guard && dirs.length > 0 && (
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

// ---------------- Codex 要先处理的：待确认的 hook、还没登录 ----------------

/**
 * 终端里的 Codex 启动时会停下来问、对话页原本看不到的事（docs/CODEX_UI.md）：
 * - 还没登录：对话发不出去，登录要在终端里做（Codex 自己的登录页）；
 * - 工作目录里新的、改过的 hook（用户的、项目 `.codex/` 的、插件的）：Codex 只运行信任过的，没信任的在对话里也不运行，
 *   终端里启动时会问（“Hooks need review”）。在这里列出来，信任或跳过；不挡着输入。
 * 打开对话、每一轮做完时重新看一次（hook 文件随时可能被改）。
 */
function CodexSetup({ id, phase, onTab }) {
  const { t } = useT();
  const [setup, setSetup] = useState(null);
  const load = useCallback(async () => {
    try {
      setSetup(await api.get(`/instances/${id}/chat/setup`));
    } catch {
      // 读不到（app-server 还没起来、旧版 Codex 没有 hooks/list）：不显示，下次再看
    }
  }, [id]);
  const idle = phase === 'idle';
  useEffect(() => {
    if (idle) load();
  }, [idle, load]);
  return (
    <>
      {setup?.login_required && (
        <p className="chat-event">
          <TriangleAlert size={13} aria-hidden="true" /> {t('chat.codex_login_required')}
          {onTab && (
            <button type="button" className="chat-more" onClick={() => onTab('term')}>
              {t('chat.codex_login_go')}
            </button>
          )}
        </p>
      )}
      {setup?.update && <CodexUpdate id={id} update={setup.update} onDone={load} />}
      {setup?.hooks?.length > 0 && <HooksCard id={id} items={setup.hooks} onDone={load} />}
    </>
  );
}

/**
 * Codex 有新版本（终端里的 TUI 不再自己弹更新提示，见 codex_app.rs 的 NO_UPDATE_PROMPT）。
 * npm 装的一键更新：和新建会话时装 Codex 是同一个后台任务，看得到进度；装好后对话空闲时自动换成新版本。
 */
function CodexUpdate({ id, update, onDone }) {
  const { t } = useT();
  const toast = useToast();
  const [job, setJob] = useState(null);
  const [busy, setBusy] = useState(false);
  const running = job?.state === 'running';
  useEffect(() => {
    if (!running) return undefined;
    const timer = setInterval(async () => {
      const j = await api.get('/tools/install/codex').catch(() => null);
      if (j) setJob(j);
    }, 1500);
    return () => clearInterval(timer);
  }, [running]);
  const act = async (install) => {
    setBusy(true);
    try {
      const r = await api.post(`/instances/${id}/chat/codex-update`, { install });
      if (install) setJob(r);
      else onDone();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  if (job?.state === 'ok') {
    return (
      <p className="chat-event">
        <Check size={13} aria-hidden="true" /> {t('chat.codex_updated', { version: update.latest })}
      </p>
    );
  }
  return (
    <div className="chat-event chat-update">
      <span>
        <Sparkle size={13} aria-hidden="true" /> {running ? t('chat.codex_updating', { version: update.latest }) : t('chat.codex_update', { latest: update.latest, current: update.current })}
        {!update.npm && !running && ` ${t('chat.codex_update_manual')}`}
      </span>
      {running && <LoaderCircle size={13} className="spin" aria-hidden="true" />}
      {!running && update.npm && (
        <button type="button" className="chat-more" disabled={busy} onClick={() => act(true)}>
          {job?.state === 'failed' ? t('chat.codex_update_retry') : t('chat.codex_update_now')}
        </button>
      )}
      {!running && (
        <button type="button" className="chat-more" disabled={busy} onClick={() => act(false)}>
          {t('chat.codex_update_skip')}
        </button>
      )}
      {job?.state === 'failed' && (
        <>
          <span className="chat-note">{t('chat.codex_update_failed')}</span>
          <Pre text={(job.output ?? '').split('\n').slice(-12).join('\n')} />
        </>
      )}
    </div>
  );
}

function HooksCard({ id, items, onDone }) {
  const { t } = useT();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const answer = async (trust) => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/hooks`, { trust, hooks: items.map((h) => ({ key: h.key, hash: h.hash })) });
      if (trust) toast(t('chat.hooks_trusted', { count: items.length }));
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
      onDone();
    }
  };
  return (
    <section className="chat-ask chat-hooks">
      <header>
        <ShieldAlert size={16} aria-hidden="true" />
        <b>{t('chat.hooks_title', { count: items.length })}</b>
      </header>
      <div className="chat-ask-body">
        <p className="chat-note">{t('chat.hooks_note')}</p>
        <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
          {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
          <span className="chat-tool-sum ellipsis">{open ? t('chat.hooks_hide') : t('chat.hooks_show')}</span>
        </button>
        {open &&
          items.map((h) => (
            <div key={h.key} className="chat-hook">
              <div className="chat-note">
                {t('chat.hooks_when', { event: t(`chat.hook_event.${h.event}`, { defaultValue: h.event }), matcher: h.matcher || '*' })}
                {h.modified && ` · ${t('chat.hooks_modified')}`}
              </div>
              <Pre text={h.command ? `$ ${h.command}` : h.type} className="cmd" />
              <div className="chat-note ellipsis" title={h.path}>
                {h.plugin ? t('chat.hooks_from_plugin', { plugin: h.plugin }) : h.path}
              </div>
            </div>
          ))}
        <div className="chat-acts">
          <Button busy={busy} onClick={() => answer(true)}>
            {t('chat.hooks_trust')}
          </Button>
          <Button variant="secondary" busy={busy} onClick={() => answer(false)}>
            {t('chat.hooks_skip')}
          </Button>
        </div>
      </div>
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

/** 额度什么时候重置：用量接口给的准确时间（用完的那个窗口；都没满时取 5 小时的），按浏览器时区显示。
 *  不拿 7 天的兜底：5 小时的刚重置完时接口里没有它的时间，会显示成几天后 */
function LimitReset({ id }) {
  const { data } = usePoll(`/instances/${id}/chat/usage`);
  const { five_hour: five, seven_day: week } = data?.limits ?? {};
  const full = [five, week].find((w) => w?.resets_at && w.used >= 100);
  const at = (full ?? five)?.resets_at;
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
        <SwitchProfile id={id} />
        <Button variant="ghost" onClick={cancel} busy={busy}>
          {t('chat.limit.cancel_wait')}
        </Button>
      </div>
    </div>
  );
}

/** 这一轮因为账号的问题停了（StopFailure 的 error，官方 hooks 文档）：订阅到期 / 欠费、账号被暂停、登录失效、余额不足、额度用完后没在等 */
const ACCOUNT_ERRORS = ['oauth_org_not_allowed', 'account_on_hold', 'billing_error', 'authentication_failed', 'rate_limit'];

/** 账号用不了、这一轮停下了：输入框上面一张卡片，可以换个账号接着这段对话 */
function AccountFail({ id, error }) {
  const { t } = useT();
  return (
    <div className="chat-limit is-stopped" role="status">
      <span className="chat-limit-head">
        <TriangleAlert size={15} aria-hidden="true" />
        {t(`chat.limit.fail_${error}`)}
      </span>
      <p className="chat-note">{t('chat.limit.fail_hint')}</p>
      <div className="chat-acts">
        <SwitchProfile id={id} manage />
      </div>
    </div>
  );
}

/** “换个账号继续”：这台电脑上另外的 Claude Code 账号 / 接口，选了就用它接着同一段对话。
 *  `goOn`（额度用完、账号用不了时）：起来后自动发 continue。`manage`：没有别的账号时也给一个去添加账号的按钮 */
function SwitchProfile({ id, manage, goOn = true }) {
  const { t } = useT();
  const { navigate } = useRouter();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [data] = useLoad(() => Promise.all([api.get('/claude-profiles'), api.get(`/instances/${id}`)]).catch(() => null), [id]);
  if (!data) return null;
  const [{ profiles, default: def }, inst] = data;
  // 容器里的会话不用看看的账号
  if (inst.container) return null;
  const cur = inst.claude_profile ?? def;
  const others = profiles.filter((p) => p.id !== cur);
  if (!others.length) {
    return manage ? (
      <Button variant="ghost" onClick={() => navigate('/settings#claude-accounts')}>
        <UserRound size={15} aria-hidden="true" /> {t('chat.limit.add_account')}
      </Button>
    ) : null;
  }
  const pick = async (p) => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/switch-profile`, { profile: p.id, continue: goOn });
      toast(t(goOn ? 'chat.limit.switched' : 'chat.limit.switched_plain', { name: profileName(t, p) }), 'ok');
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  return (
    <PopMenu
      className="btn btn-ghost"
      icon={UserRound}
      text={t(goOn ? 'chat.limit.switch' : 'chat.limit.switch_short')}
      label={t(goOn ? 'chat.limit.switch_hint' : 'chat.limit.switch_plain_hint')}
      disabled={busy}
      items={[
        ...others.map((p) => ({
          icon: (x) => <KindIcon p={p} {...x} />,
          label: profileName(t, p),
          hint: p.kind === 'api' ? p.base_url : p.id === def ? t('cprof.default') : undefined,
          onClick: () => pick(p),
        })),
        'sep',
        { label: t('cprof.manage'), onClick: () => navigate('/settings#claude-accounts') },
      ]}
    />
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

/** 以前说过的话（Ctrl+R）：搜索框 + 列表，上下键选、回车或点一下接到输入框里。 */
function InputsMenu({ all, onPick, onClose }) {
  const { t } = useT();
  const [q, setQ] = useState('');
  const [pick, setPick] = useState(0);
  const list = useMemo(() => searchInputs(all, q).slice(0, 100), [all, q]);
  const box = useRef(null);
  useEffect(() => setPick(0), [q]);
  useEffect(() => {
    box.current?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: 'nearest' });
  }, [pick]);
  const onKey = (e) => {
    if (imeComposing(e)) return;
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp' || (e.key === 'r' && e.ctrlKey)) {
      // Ctrl+R 再按一下：往更早的找（像命令行）
      e.preventDefault();
      if (list.length) setPick((p) => (p + (e.key === 'ArrowUp' ? -1 : 1) + list.length) % list.length);
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (list[pick]) onPick(list[pick].text);
    }
  };
  return (
    <Popover onClose={onClose} className="chat-pop-left chat-inputs" label={t('chat.inputs')}>
      <label className="chat-inputs-search">
        <Search size={14} aria-hidden="true" />
        <input autoFocus type="search" value={q} onChange={(e) => setQ(e.target.value)} onKeyDown={onKey} placeholder={t('chat.inputs_search')} aria-label={t('chat.inputs_search')} spellCheck={false} />
      </label>
      <div className="chat-inputs-list" ref={box} role="listbox">
        {list.map((x, n) => (
          <button key={x.text} type="button" role="option" className="chat-inputs-row" aria-selected={n === pick} onPointerMove={() => setPick(n)} onClick={() => onPick(x.text)} title={x.text}>
            <span className="chat-inputs-text">{x.text}</span>
            <small>{x.here ? t('chat.inputs_here') : x.at ? relativeTime(t, x.at) : ''}</small>
          </button>
        ))}
        {!list.length && <p className="chat-note chat-inputs-empty">{t(q.trim() ? 'chat.inputs_no_match' : 'chat.inputs_empty')}</p>}
      </div>
      <div className="chat-pop-h chat-inputs-foot">
        <span>{t('chat.inputs_foot')}</span>
      </div>
    </Popover>
  );
}

/**
 * /resume：这台电脑、这个项目里同一个工具以前的对话，选一个在这里接着（不离开这一页）。
 * 当前这段标出来、不能选；已经开在别的看看会话里的，点了打开那个会话。
 */
/**
 * Codex 的 Git 工作树（Codex 0.164 的 /new、/worktree，见 src/chat/codex_worktree.rs）：
 * kind=new：/new 时先问“新对话在哪运行”——当前目录（照常送 /new）或新建一个工作树；不是 Git 仓库就直接 /new。
 * kind=menu：/worktree——在新工作树里接着这段对话（fork）、开新对话，或者回到已有工作树里的那段对话。
 */
function WorktreeMenu({ id, status, kind, onClose }) {
  const { t } = useT();
  const toast = useToast();
  const [data, setData] = useState(null);
  const [busy, setBusy] = useState(null);
  const plain = async () => {
    setBusy('plain');
    try {
      await api.post(`/instances/${id}/chat/input`, { text: '/new', images: [] });
      onClose();
    } catch (e) {
      toast(errorText(t, e), 'error');
      setBusy(null);
    }
  };
  useEffect(() => {
    let alive = true;
    api.get(`/instances/${id}/chat/worktrees`).then(
      (r) => {
        if (!alive) return;
        // 不在 Git 仓库里没得选：/new 就是 /new
        if (kind === 'new' && !r?.available) plain();
        else setData(r ?? { items: [] });
      },
      (e) => alive && setData({ error: e }),
    );
    return () => {
      alive = false;
    };
  }, [id, kind]);
  const run = async (mode, root) => {
    if (busy) return;
    setBusy(root ?? mode);
    try {
      const r = await api.post(`/instances/${id}/chat/worktree`, root ? { mode, root } : { mode });
      if (mode !== 'open') toast(t('chat.wt_created', { path: r?.cwd ?? '' }), 'ok');
      onClose();
    } catch (e) {
      toast(errorText(t, e), 'error');
      setBusy(null);
    }
  };
  const running = status?.phase === 'running';
  const items = Array.isArray(data?.items) ? data.items : [];
  const name = (p) => String(p ?? '').split(/[\\/]/).filter(Boolean).slice(-2).join('/');
  const wait = (key) => (busy === key ? <LoaderCircle size={15} className="spin" /> : null);
  const title = t(kind === 'new' ? 'chat.wt_where' : 'chat.wt_title');
  return (
    <Popover onClose={onClose} className="chat-pop-left chat-resume" label={title}>
      <div className="chat-pop-h">
        <span>{title}</span>
        {data?.repo && <span className="ellipsis" title={data.repo}>{name(data.repo)}</span>}
      </div>
      {running && <p className="chat-note chat-resume-note">{t('chat.resume_running')}</p>}
      <div className="chat-inputs-list">
        {data === null ? (
          <LoaderCircle size={16} className="spin chat-pop-wait" />
        ) : data.error ? (
          <p className="chat-note chat-inputs-empty">{errorText(t, data.error)}</p>
        ) : !data.available ? (
          <p className="chat-note chat-inputs-empty">{t('chat.wt_no_repo')}</p>
        ) : (
          <>
            {kind === 'new' ? (
              <button type="button" className="chat-mi" disabled={running || !!busy} onClick={plain}>
                <FolderGit2 size={15} aria-hidden="true" />
                <span className="chat-mi-text">
                  <span>{t('chat.wt_here')}</span>
                  <small className="ellipsis">{t('chat.wt_here_hint')}</small>
                </span>
                {wait('plain')}
              </button>
            ) : (
              <button type="button" className="chat-mi" disabled={running || !!busy || !status?.session_id} onClick={() => run('fork')}>
                <GitFork size={15} aria-hidden="true" />
                <span className="chat-mi-text">
                  <span>{t('chat.wt_fork')}</span>
                  <small className="ellipsis">{t('chat.wt_fork_hint')}</small>
                </span>
                {wait('fork')}
              </button>
            )}
            <button type="button" className="chat-mi" disabled={running || !!busy} onClick={() => run('new')}>
              <Plus size={15} aria-hidden="true" />
              <span className="chat-mi-text">
                <span>{t(kind === 'new' ? 'chat.wt_new' : 'chat.wt_new_conv')}</span>
                <small className="ellipsis">{t('chat.wt_new_hint')}</small>
              </span>
              {wait('new')}
            </button>
            {kind === 'menu' && items.length > 0 && <div className="chat-pop-h"><span>{t('chat.wt_existing')}</span></div>}
            {kind === 'menu' &&
              items.map((w) => (
                <button key={w.root} type="button" className="chat-mi chat-resume-row" aria-current={w.current || undefined} disabled={w.current || !w.owner || running || !!busy} onClick={() => run('open', w.root)} title={w.cwd}>
                  <span className="chat-mi-text">
                    <span className="ellipsis">{name(w.root)}</span>
                    <small className="ellipsis">{[w.head?.slice(0, 7), !w.owner && t('chat.wt_no_owner')].filter(Boolean).join(' · ')}</small>
                  </span>
                  {busy === w.root ? <LoaderCircle size={15} className="spin" /> : w.current ? <span className="badge badge-brand">{t('chat.resume_here')}</span> : null}
                </button>
              ))}
          </>
        )}
      </div>
    </Popover>
  );
}

function ResumeMenu({ id, status, onClose }) {
  const { t } = useT();
  const { id: agentId } = useContext(AgentCtx);
  const sessions = useSessions();
  const { navigate } = useRouter();
  const toast = useToast();
  const [workdir, setWorkdir] = useState(() => sessions?.known(id)?.workdir ?? null);
  const [data, setData] = useState(null);
  const [q, setQ] = useState('');
  const [busy, setBusy] = useState(null);
  useEffect(() => {
    if (workdir) return undefined;
    let alive = true;
    api.get(`/instances/${id}`).then(
      (r) => alive && (r?.workdir ? setWorkdir(r.workdir) : setData({ items: [] })),
      (e) => alive && setData({ error: e }),
    );
    return () => {
      alive = false;
    };
  }, [id, workdir]);
  useEffect(() => {
    if (!workdir) return undefined;
    let alive = true;
    deviceApi(terminalDevice(id))
      .get(`/history?workdir=${encodeURIComponent(workdir)}&limit=50`)
      .then(
        (r) => alive && setData(r ?? { items: [] }),
        (e) => alive && setData({ error: e }),
      );
    return () => {
      alive = false;
    };
  }, [id, workdir]);
  const running = status?.phase === 'running';
  const isHere = (c) => c.instance_id === id || (status?.session_id && c.id === status.session_id);
  const words = q.trim().toLowerCase();
  const list = (Array.isArray(data?.items) ? data.items : []).filter((c) => c.agent === agentId && (!words || [c.title, c.last, c.branch].filter(Boolean).join('\n').toLowerCase().includes(words)));
  const go = async (c) => {
    if (isHere(c) || busy) return;
    if (c.instance_id) {
      onClose();
      navigate(`/s/${c.instance_id}`);
      return;
    }
    setBusy(c.id);
    try {
      await api.post(`/instances/${id}/chat/input`, { text: `/resume ${c.id}`, images: [] });
      onClose();
    } catch (e) {
      toast(errorText(t, e), 'error');
      setBusy(null);
    }
  };
  return (
    <Popover onClose={onClose} className="chat-pop-left chat-resume" label={t('chat.resume_title')}>
      <div className="chat-pop-h">
        <span>{t('chat.resume_title')}</span>
        {workdir && <span className="ellipsis" title={workdir}>{workdir.split(/[\\/]/).filter(Boolean).pop()}</span>}
      </div>
      {(Array.isArray(data?.items) ? data.items.length : 0) > 6 && (
        <label className="chat-inputs-search">
          <Search size={14} aria-hidden="true" />
          <input autoFocus type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('chat.resume_search')} aria-label={t('chat.resume_search')} spellCheck={false} />
        </label>
      )}
      {running && <p className="chat-note chat-resume-note">{t('chat.resume_running')}</p>}
      <div className="chat-inputs-list">
        {data === null ? (
          <LoaderCircle size={16} className="spin chat-pop-wait" />
        ) : data.error ? (
          <p className="chat-note chat-inputs-empty">{errorText(t, data.error)}</p>
        ) : !list.length ? (
          <p className="chat-note chat-inputs-empty">{t(words ? 'chat.inputs_no_match' : 'chat.resume_empty')}</p>
        ) : (
          list.map((c) => {
            const here = isHere(c);
            const other = !here && c.instance_id;
            return (
              <button key={c.id} type="button" className="chat-mi chat-resume-row" aria-current={here || undefined} disabled={here || (running && !other) || (busy && busy !== c.id)} onClick={() => go(c)} title={c.last || c.title}>
                <span className="chat-mi-text">
                  <span className="ellipsis">{c.title}</span>
                  <small className="ellipsis">
                    {[c.branch, c.last].filter(Boolean).join(' · ')}
                  </small>
                </span>
                {busy === c.id ? (
                  <LoaderCircle size={15} className="spin" />
                ) : here ? (
                  <span className="badge badge-brand">{t('chat.resume_here')}</span>
                ) : other ? (
                  <span className="badge" title={t('chat.resume_other_hint')}>
                    <ExternalLink size={11} aria-hidden="true" /> {t('chat.resume_other')}
                  </span>
                ) : c.live ? (
                  <span className="badge badge-warn" title={t('hist.live')}>{t('hist.live_badge')}</span>
                ) : (
                  <small className="chat-resume-when">{relativeTime(t, c.updated_at)}</small>
                )}
              </button>
            );
          })
        )}
      </div>
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

/** 把光标放进对话输入框；别处正在输入（另一个输入框、弹窗）时不抢。 */
function claimFocus(el) {
  if (!el || el.disabled || !el.offsetParent) return;
  const a = shownFocus();
  const elsewhere = a && a !== el && !el.closest('.chat-composer')?.contains(a) && (a.matches?.('input, textarea, select, [contenteditable="true"]') || a.closest?.('[role="dialog"]'));
  if (!elsewhere) el.focus({ preventScroll: true });
}

function Composer({ id, active, status, phase, touch, offline, models, usage, items, onAction, onSide }) {
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
  // 外面（会话接不上时的“接以前的对话”）要打开以前的对话
  useEffect(() => {
    const open = () => setMenu('resume');
    window.addEventListener(`ll-chat-resume:${id}`, open);
    return () => window.removeEventListener(`ll-chat-resume:${id}`, open);
  }, [id]);
  // 电脑上：打开会话、切回这个标签、输入框从不可用变成可用（刚连上、等选择的框关掉了）时，光标放进输入框。
  // 以前只在切换的那一刻试一次，那时对话还没加载完、输入框还不可用，焦点就落空了。
  // 别处正在输入（侧栏搜索、弹窗里的框）时不抢。
  useEffect(() => {
    if (!active || disabled || touch) return undefined;
    const timer = setTimeout(() => claimFocus(area.current), 30);
    return () => clearTimeout(timer);
  }, [active, disabled, touch]);
  // 换成了新会话（/new、/clear）：回到空的首页，光标放回输入框。发出命令时输入框暂时不可用，焦点已经丢了，等能用了再放
  const sid = status?.session_id ?? null;
  const seenSid = useRef(sid);
  useEffect(() => {
    if (!sid || disabled || sid === seenSid.current) return;
    const was = seenSid.current;
    seenSid.current = sid;
    if (was && !touch) requestAnimationFrame(() => area.current?.focus());
  }, [sid, disabled]);
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
    const action = !images.length && slashAction(body, agentId);
    if (action) {
      setText('');
      menuFor(action);
      return;
    }
    // Codex 的 /new：在 Git 仓库里先问在哪运行（当前目录 / 新建工作树），菜单里再送
    if (agentId === 'codex' && !images.length && body.toLowerCase() === '/new') {
      setText('');
      setMenu('new');
      return;
    }
    // 通知权限不在这里申请：等第一次需要确认、或者跑完一轮长任务时说清楚为什么再问（ChatTips.jsx 的 NotifyAsk）
    setSending(true);
    // 先清空输入框（斜杠命令也一样）；没送出去时再放回来，不丢用户打的字
    inflight.current = body;
    setText('');
    hist.current = -1;
    if (body && agentId !== 'claude') rememberInput(body);
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
      // 点了发送按钮时焦点在按钮上：发完放回输入框，接着打下一句
      if (!touch) requestAnimationFrame(() => claimFocus(area.current));
    }
  };
  // 界面自己做的命令：/model 开模型菜单，/resume 开以前的对话（不离开这一页）
  const menuFor = (action) => {
    if (action === 'model') setMenu('model');
    else if (action === 'history') setMenu('resume');
    else if (action === 'worktree') setMenu('worktree');
    else onAction(action);
  };
  // 以前说过的话：Claude Code 用它自己的 history.jsonl（和终端里按上键翻的同一份），发出一句后重新取；
  // 取不到（别的工具、旧版客户端）用本机存着的
  const said = useMemo(() => (items ?? []).filter((x) => x.k === 'user').length, [items]);
  const [prompts, setPrompts] = useState(null);
  const hasStatus = status != null;
  useEffect(() => {
    if (agentId !== 'claude' || !hasStatus) return undefined;
    let alive = true;
    api.get(`/instances/${id}/chat/prompts`).then(
      (r) => alive && Array.isArray(r?.items) && setPrompts(r.items),
      () => {},
    );
    return () => {
      alive = false;
    };
  }, [id, agentId, hasStatus, said, status?.session_id]);
  // Claude Code 的照它自己的顺序（这个项目里最新的在前，和终端里一样）；对话里新说的话一出现就重新取了
  const fromCli = agentId === 'claude' && prompts;
  const inputList = useMemo(() => (fromCli ? inputHistory([], prompts, status?.session_id) : inputHistory(items)), [fromCli, prompts, items, status?.session_id]);
  // 上下键翻以前说过的话：光标在第一行时往上、最后一行时往下；翻到头再往下回到原来打的字
  const hist = useRef(-1);
  const histDraft = useRef('');
  const recall = (dir, el) => {
    const before = el.value.slice(0, el.selectionStart);
    const after = el.value.slice(el.selectionEnd);
    if (el.selectionStart !== el.selectionEnd || (dir > 0 ? before.includes('\n') : after.includes('\n'))) return false;
    if (dir < 0 && hist.current < 0) return false;
    const list = inputList;
    const next = hist.current + dir;
    if (next >= list.length) return false;
    if (hist.current < 0) histDraft.current = text;
    hist.current = next;
    const value = next < 0 ? histDraft.current : list[next].text;
    setText(value);
    requestAnimationFrame(() => area.current?.setSelectionRange(value.length, value.length));
    return true;
  };
  // 历史里点的一句：接在输入框已有的字后面
  const appendInput = (s) => {
    setMenu(null);
    hist.current = -1;
    setText((cur) => (cur.trim() ? `${cur.replace(/\s+$/, '')}\n${s}` : s));
    requestAnimationFrame(() => {
      const el = area.current;
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    });
  };
  const stop = async () => {
    try {
      await api.post(`/instances/${id}/chat/interrupt`);
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  const onKey = (e) => {
    if (e.key === 'r' && e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
      // 像命令行：Ctrl+R 搜以前说过的话
      e.preventDefault();
      setMenu('inputs');
    } else if ((e.key === 'ArrowUp' || e.key === 'ArrowDown') && !slash.length && !e.shiftKey && !e.altKey && !e.ctrlKey && !e.metaKey && !imeComposing(e) && recall(e.key === 'ArrowUp' ? 1 : -1, e.currentTarget)) {
      e.preventDefault();
    } else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey) && !imeComposing(e) && canNow) {
      // 和 Claude Code 一样：Ctrl+Enter 立即发送
      e.preventDefault();
      send(true);
    } else if (e.key === 'Enter' && !e.shiftKey && !imeComposing(e) && !touch) {
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
    const builtin = COMMANDS.filter(([name]) => (agentId === 'codex' ? CODEX_COMMANDS.includes(name) : !CODEX_ONLY.includes(name))).map(([name, action]) => ({ name, action, description: t(`chat.cmd.${name.slice(1)}`) }));
    const names = new Set([...builtin.map((c) => c.name), ...TERMINAL_ONLY]);
    const mine = (custom ?? []).filter((c) => !names.has(c.name));
    const all = [...builtin, ...mine];
    const hit = all.filter((c) => c.name.toLowerCase().startsWith(q));
    return (hit.length ? hit : all.filter((c) => c.name.toLowerCase().includes(q.slice(1)))).slice(0, 40);
  }, [wantSlash, text, custom]);
  const pickSlash = (c) => {
    if (c.action) {
      setText('');
      menuFor(c.action);
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
  // Codex 这段对话在另一个 Git 工作树里跑（/worktree、/new 选了新建）：输入框上标出来，点开换
  const workdir = useSessions()?.known(id)?.workdir;
  const trim = (p) => String(p ?? '').replace(/[\\/]+$/, '');
  const elsewhere = agentId === 'codex' && status?.cwd && workdir && !trim(status.cwd).startsWith(trim(workdir)) ? status.cwd : null;
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
      {menu === 'inputs' && (
        <InputsMenu
          all={inputList}
          onPick={appendInput}
          onClose={() => {
            setMenu(null);
            area.current?.focus();
          }}
        />
      )}
      {menu === 'resume' && <ResumeMenu id={id} status={status} onClose={() => setMenu(null)} />}
      {(menu === 'new' || menu === 'worktree') && <WorktreeMenu id={id} status={status} kind={menu === 'new' ? 'new' : 'menu'} onClose={() => setMenu(null)} />}
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
              {q || <ImageIcon size={14} aria-label={t('chat.images', { n: 1 })} />}
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
          onChange={(e) => {
            hist.current = -1;
            setText(e.target.value);
          }}
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
          {/* 以前说过的话：点开搜（电脑上 Ctrl+R），选一句接到输入框里 */}
          <button type="button" className="btn btn-plain btn-icon chat-inputs-btn" data-pop-anchor="" aria-expanded={menu === 'inputs'} disabled={disabled} onClick={() => setMenu(menu === 'inputs' ? null : 'inputs')} aria-label={t('chat.inputs')} title={touch ? t('chat.inputs') : t('chat.inputs_hint')}>
            <History size={17} />
          </button>
          {elsewhere && (
            <button type="button" className="btn btn-plain chat-chip" data-pop-anchor="" aria-expanded={menu === 'worktree'} disabled={!ready} onClick={() => setMenu(menu === 'worktree' ? null : 'worktree')} title={elsewhere}>
              <GitBranch size={14} aria-hidden="true" />
              <span className="ellipsis">{t('chat.wt_chip')}</span>
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
  // 会话接不上：原因已经写在上面的说明里
  if (phase === 'detached' && reason === 'resume_failed') return null;
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
/**
 * 草稿页发出的第一句（shared/firstSend.js）：会话还没起来时先显示出来，下面一行“正在启动…”；
 * 起来了自动发出去。没起来时字不丢：重试，或者带着字回到草稿换个类型、项目。
 */
function FirstSend({ id, first, onBack }) {
  const { t } = useT();
  const [busy, setBusy] = useState(false);
  const failed = first.state === 'failed';
  const why = (f) => {
    if (f.kind === 'instance') return t(`instance.error.${f.code}`, { defaultValue: t('instance.error.other') });
    if (f.kind === 'detached') return t(`chat.detached_${f.reason}_title`, { defaultValue: t('chat.first_timeout') });
    if (f.kind === 'timeout') return t('chat.first_timeout');
    return f.error?.message === 'not_delivered' ? t('chat.not_delivered') : errorText(t, f.error);
  };
  const retry = async () => {
    setBusy(true);
    try {
      await retryFirst(id);
    } finally {
      setBusy(false);
    }
  };
  // 放回输入框：会话起来了（只是发送出错）时在这里接着发
  const toInput = () => {
    const x = dropFirst(id);
    if (x?.text) window.dispatchEvent(new CustomEvent(`ll-chat-fill:${id}`, { detail: x.text }));
  };
  return (
    <>
      <div className="chat-user">
        {first.files.length > 0 && (
          <div className="chat-user-imgs">
            {first.files.map((f) => (
              <span key={f.url} className="chat-user-img">
                <img src={f.url} alt="" />
              </span>
            ))}
          </div>
        )}
        {first.text && <div className="chat-bubble">{first.text}</div>}
      </div>
      {failed ? (
        <div className="chat-first-fail" role="alert">
          <b>{t(first.fail.kind === 'send' ? 'chat.first_send_failed' : 'chat.first_start_failed')}</b>
          <span>
            {why(first.fail)} {t('chat.first_kept')}
          </span>
          <span className="chat-acts">
            <Button variant="secondary" busy={busy} onClick={retry}>
              {t('chat.first_retry')}
            </Button>
            {first.fail.kind === 'send' ? (
              <Button variant="secondary" onClick={toInput}>
                {t('chat.first_to_input')}
              </Button>
            ) : (
              onBack && (
                <Button variant="secondary" onClick={onBack}>
                  {t('chat.first_back')}
                </Button>
              )
            )}
          </span>
        </div>
      ) : first.state !== 'sent' && (
        <p className="chat-first-wait">
          <LoaderCircle size={14} className="spin" aria-hidden="true" />
          {first.state === 'sending' ? t('chat.first_sending') : t('chat.first_waiting')}
          {first.state === 'waiting' && <span className="muted">{t('chat.first_waiting_note')}</span>}
        </p>
      )}
    </>
  );
}

function Detached({ id, status }) {
  const { t, i18n } = useT();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const reason = status?.reason ?? 'starting';
  const run = async (path, body) => {
    setBusy(true);
    try {
      await api.post(path, body);
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
  // 有的状态没有说明：不能用 defaultValue: ''，空的兜底值会让 i18next 显示键名（配置了 returnEmptyString: false）
  const bodyKey = `chat.detached_${key}_body`;
  const body = i18n.exists(bodyKey) ? t(bodyKey) : '';
  return (
    <div className="chat-detached">
      {(reason === 'starting' || reason === 'adopting' || reason === 'switching' || adoptable) && <LoaderCircle size={18} className="spin" aria-hidden="true" />}
      <b>{t(`chat.detached_${key}_title`, { defaultValue: t('chat.detached_starting_title') })}</b>
      {body && <p className="muted">{body}</p>}
      {reason === 'resume_failed' && (
        <>
          {status?.error && <p className="chat-note">{t('chat.detached_resume_failed_detail', { detail: status.error })}</p>}
          <div className="chat-acts">
            <Button busy={busy} onClick={() => run(`/instances/${id}/chat/input`, { text: '/new', images: [] })}>
              {t('chat.resume_failed_new')}
            </Button>
            <Button variant="ghost" onClick={() => window.dispatchEvent(new Event(`ll-chat-resume:${id}`))}>
              {t('chat.resume_failed_pick')}
            </Button>
            <Button variant="ghost" busy={busy} onClick={relaunch}>
              {t('chat.relaunch_app_server')}
            </Button>
          </div>
        </>
      )}
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
          {/* 上面一行放不下完整的命令：展开后完整显示，可以复制 */}
          <Copyable text={shell.command}>
            <Pre text={`$ ${shell.command}`} className="cmd" />
          </Copyable>
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
  const { wide, onWide } = useContext(SideCtx);
  const { data, items, error, done } = useAgentSteps(id, { agent: agent.id, tool: agent.tool_use_id, enabled: true });
  const live = (agent.state ?? 'running') === 'running' && !done;
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
        <TaskState state={live ? 'running' : done ? 'done' : agent.state ?? 'ended'} />
        <b className="grow ellipsis" title={agent.description || agent.type || ''}>
          {agent.description || agent.type || t('chat.agents_title')}
        </b>
        {onWide && (
          <button type="button" className="btn btn-plain btn-icon" onClick={() => onWide(!wide)} aria-pressed={wide} aria-label={t(wide ? 'chat.side_narrow' : 'chat.side_wide')} title={t(wide ? 'chat.side_narrow' : 'chat.side_wide')}>
            {wide ? <Minimize2 size={16} /> : <Maximize2 size={16} />}
          </button>
        )}
      </header>
      <div className="chat-side-body" ref={body}>
        <p className="chat-note">
          {[agent.type, agent.model && modelLabel(agent.model), agent.background ? t('chat.agent_background') : null, agent.worktree ? t('chat.agent_worktree') : null, t('chat.agent_steps', { count: stepCount(items) })].filter(Boolean).join(' · ')}
        </p>
        {/* 派给它的任务说明（Codex 的是加密的，没有） */}
        {agent.prompt && (
          <details className="chat-thinking chat-agent-prompt">
            <summary>{t('chat.agent_prompt')}</summary>
            <Pre text={agent.prompt} />
          </details>
        )}
        {error && !data && <p className="chat-note">{t('chat.sub_waiting')}</p>}
        <div className="chat-items" ref={steps}>
          <Rows id={id} items={items} live={live} />
        </div>
      </div>
    </div>
  );
}

/**
 * 输入框上面一条：正在跑的子代理（每个一枚，显示名字和正在做的那一步），点了在右侧面板里看它的步骤。
 * Claude 按服务端的任务列表（后台运行的子代理只有它知道还在不在跑），Codex 按对话条目。
 */
function AgentStrip({ id, items, live, bg, onOpen }) {
  const { t } = useT();
  const { id: agent } = useContext(AgentCtx);
  const fromItems = useMemo(() => conversationAgents(items, live).filter((a) => a.state === 'running'), [items, live]);
  const claude = agent === 'claude';
  const { data } = usePoll(`/instances/${id}/chat/tasks`, { enabled: claude && (bg.subagent > 0 || fromItems.length > 0), every: 3000 });
  const running = claude ? (data?.agents ?? []).filter((a) => a.state === 'running') : fromItems;
  // 服务端还没列出来（刚派出去）时先按对话条目
  const list = running.length || !claude ? running : fromItems;
  if (!list.length) return null;
  const shown = list.slice(0, 3);
  return (
    <div className="chat-agent-strip" role="group" aria-label={t('chat.agents_running', { count: list.length })}>
      <Bot size={14} aria-hidden="true" className="chat-agent-strip-icon" />
      {shown.map((a) => {
        const step = a.last_tool ? `${a.last_tool.name} ${toolSummary(a.last_tool.name, a.last_tool.input)}` : null;
        return (
          <button key={a.id ?? a.tool_use_id} type="button" className="chat-agent-chip" onClick={() => onOpen(a)} title={[a.description, step].filter(Boolean).join('\n')}>
            <LoaderCircle size={12} className="spin" aria-hidden="true" />
            <b className="ellipsis">{a.description || a.type || t('chat.agents_title')}</b>
            {step && <span className="ellipsis mono">{step}</span>}
          </button>
        );
      })}
      {list.length > shown.length && (
        <button type="button" className="chat-agent-chip chat-agent-more" onClick={() => onOpen(null)}>
          +{list.length - shown.length}
        </button>
      )}
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
  const { open: openFile, cwd } = useContext(PreviewCtx);
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
            {files.map((f) => {
              const abs = absPath(f.path, cwd);
              const open = () => openFile?.({ ep: fileEndpoints(id), path: abs, list: files.map((x) => absPath(x.path, cwd)) });
              return (
                <li key={f.path} title={f.path}>
                  <button type="button" className="chat-file-row" onClick={open} disabled={!openFile} aria-label={`${t('chat.file_open')} ${f.path}`}>
                    <FilePen size={14} aria-hidden="true" />
                    <span className="ellipsis mono">
                      {/* 文件名突出，目录淡一点 */}
                      <b>{fileName(f.path)}</b>
                      <span className="chat-file-dir">{relDir(abs, cwd)}</span>
                    </span>
                    <DiffStat stat={f} />
                  </button>
                </li>
              );
            })}
          </ul>
          <Button variant="secondary" icon={GitBranch} onClick={() => onTab('git')}>
            {t('chat.changes_git')}
          </Button>
        </>
      )}
    </>
  );
}

/** 文件所在的目录，相对对话所在的目录写（在里面时） */
function relDir(abs, cwd) {
  const dir = abs.slice(0, Math.max(0, abs.search(/[\\/][^\\/]*$/)));
  const base = cwd?.replace(/[\\/]+$/, '');
  if (base && dir === base) return '';
  if (base && dir.startsWith(base) && /[\\/]/.test(dir[base.length])) return dir.slice(base.length + 1);
  return dir.replace(/^\/(home|Users)\/[^/]+/, '~');
}

/** 子代理的步骤数（工具调用次数） */
const stepCount = (items) => items.reduce((n, it) => n + (it.k === 'assistant' ? (it.blocks ?? []).filter((b) => b.t === 'tool_use').length : 0), 0);

/** 选中的子代理：按 id 找，没有 id（还没交回结果）时按启动它的工具调用 */
const sameAgent = (a, sel) => Boolean(sel && ((sel.id && a.id === sel.id) || (sel.tool_use_id && a.tool_use_id === sel.tool_use_id)));

/** 子代理列表里的一行：状态、名字、类型 · 模型 · 步数 · 用时，运行中的显示正在做的那一步 */
function AgentRow({ a, onOpen }) {
  const { t } = useT();
  const last = a.last_tool ? `${a.last_tool.name} ${toolSummary(a.last_tool.name, a.last_tool.input)}` : a.last_text;
  const started = a.started ? Date.parse(a.started) : null;
  const end = a.state === 'running' ? Date.now() : a.updated;
  return (
    <li className="chat-task" data-state={a.state}>
      <button type="button" className="chat-task-head chat-task-main" onClick={() => onOpen(a)}>
        <TaskState state={a.state} />
        <span className="chat-task-text">
          <b className="ellipsis">{a.description || a.type || t('chat.agents_title')}</b>
          <span className="chat-note ellipsis">
            {[a.type, a.model && modelLabel(a.model), a.steps != null ? t('chat.agent_steps', { count: a.steps }) : null, a.runs > 1 ? t('chat.agent_runs', { count: a.runs }) : null, started && end ? duration(end - started) : null, a.background ? t('chat.agent_background') : null].filter(Boolean).join(' · ')}
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
}

/** 用一秒一跳的时钟刷新用时 */
function useTick(on = true) {
  const [, tick] = useReducer((n) => n + 1, 0);
  useEffect(() => {
    if (!on) return undefined;
    const iv = setInterval(tick, 1000);
    return () => clearInterval(iv);
  }, [on]);
}

/**
 * Codex 的子代理（从对话条目里找，没有服务端的任务列表）：点开看它的每一步。
 * sel / onSel：选中的子代理（ChatView 管，对话里的卡片也能直接打开到这里）。
 */
function AgentsTab({ id, items, live, sel, onSel }) {
  const { t } = useT();
  const agents = useMemo(() => conversationAgents(items, live).reverse(), [items, live]);
  useTick(agents.some((a) => a.state === 'running'));
  if (sel) return <AgentDetail id={id} agent={agents.find((a) => sameAgent(a, sel)) ?? sel} onBack={() => onSel(null)} />;
  const running = agents.filter((a) => a.state === 'running').length;
  return (
    <>
      <h4 className="chat-side-title">
        <Bot size={15} aria-hidden="true" /> {t('chat.agents_title')}
        <span className="chat-note">{running ? t('chat.n_running', { count: running }) : agents.length || ''}</span>
      </h4>
      {agents.length === 0 ? (
        <p className="chat-note">{t('chat.agents_empty')}</p>
      ) : (
        <ul className="chat-tasks">
          {agents.map((a) => (
            <AgentRow key={a.id ?? a.tool_use_id} a={a} onOpen={onSel} />
          ))}
        </ul>
      )}
    </>
  );
}

/** 子代理和后台命令。打开时每 2 秒刷新；点子代理看它的每一步，后台命令可以看输出、停止。sel / onSel 同 AgentsTab */
function TasksTab({ id, sel, onSel }) {
  const { t } = useT();
  const { data, error } = usePoll(`/instances/${id}/chat/tasks`, { every: 2000 });
  useTick();
  const agents = data?.agents ?? [];
  const shells = [...(data?.shells ?? [])].reverse();
  // 对话里的卡片打开的、任务列表里还没有的（刚派出去）：先按卡片上的显示
  if (sel) return <AgentDetail id={id} agent={agents.find((a) => sameAgent(a, sel)) ?? sel} onBack={() => onSel(null)} />;
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
              {agents.map((a) => (
                <AgentRow key={a.id} a={a} onOpen={onSel} />
              ))}
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

/** 账号邮箱：默认打码，点眼睛显示完整的（截屏、共享屏幕时不露出来） */
function MaskedEmail({ email }) {
  const { t } = useT();
  const [show, setShow] = useState(false);
  const label = t(show ? 'chat.email_hide' : 'chat.email_show');
  return (
    <>
      <span className="ellipsis mono" title={show ? email : undefined}>
        {show ? email : maskEmail(email)}
      </span>
      <button type="button" className="btn btn-plain btn-icon" onClick={() => setShow(!show)} aria-pressed={show} aria-label={label} title={label}>
        {show ? <EyeOff size={15} /> : <Eye size={15} />}
      </button>
    </>
  );
}

function UsageTab({ id, status, onCompact }) {
  const { t } = useT();
  const { id: agentId, name: agentName } = useContext(AgentCtx);
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
      {/* Claude：订阅账号（data.account）；Codex：登录的账号（limits.account，ChatGPT 登录才有邮箱） */}
      {(data.account?.email || lim?.account) && (
        <dl className="chat-kv">
          <dt>{t('chat.usage_account')}</dt>
          <dd className="chat-account">{data.account?.email || lim?.account?.email ? <MaskedEmail email={data.account?.email || lim.account.email} /> : lim.account.type ?? '—'}</dd>
          {data.account?.org && (
            <>
              <dt>{t('chat.usage_org')}</dt>
              <dd className="ellipsis" title={data.account.org}>
                {data.account.org}
              </dd>
            </>
          )}
          {(lim?.account?.plan || lim?.plan) && (
            <>
              <dt>{t('chat.usage_plan')}</dt>
              <dd>{lim?.account?.plan || lim?.plan}</dd>
            </>
          )}
        </dl>
      )}
      {/* 随时能换：订阅到期这类情况不一定报成额度用完、账号出错（见 AccountFail） */}
      {agentId === 'claude' && status?.phase === 'idle' && (
        <div className="chat-acts">
          <SwitchProfile id={id} goOn={false} />
        </div>
      )}
      {lim?.five_hour && <Meter label={t('chat.usage_5h')} value={lim.five_hour.used} sub={resetText(t, lim.five_hour.resets_at)} />}
      {lim?.seven_day && <Meter label={t('chat.usage_7d')} value={lim.seven_day.used} sub={resetText(t, lim.seven_day.resets_at)} />}
      {lim?.reached && <p className="chat-note chat-warn">{t('chat.usage_reached')}</p>}
      {lim && (lim.five_hour || lim.seven_day) && <p className="chat-note">{lim.provider ? t('chat.usage_limits_note_provider', { provider: lim.provider }) : t(lim.account ? 'chat.usage_limits_note_codex' : lim.live ? 'chat.usage_limits_note_live' : 'chat.usage_limits_note')}</p>}
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

const SIDE_TABS = ['changes', 'outline', 'tasks', 'usage', 'config'];
const CODEX_TABS = ['changes', 'outline', 'agents', 'resources', 'usage'];

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
      // 取不到（图片已经没了等）：给一个空地址，<img> 会触发 onError，按坏图显示，不要一直转圈
      .catch(() => alive && setSrc('data:,'));
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

function SidePanel({ id, tab, onTab, onClose, status, items, onTermTab, onCompact, outline, agentSel, onAgentSel, wide }) {
  const { t } = useT();
  // 资源栏、子代理栏先只给 Codex（Claude 的子代理在“任务”里，和后台命令一起）
  const tabs = useContext(AgentCtx).id === 'claude' ? SIDE_TABS : CODEX_TABS;
  const bg = backgroundCounts(status);
  const running = (bg.subagent ?? 0) + (bg.shell ?? 0);
  const live = status?.phase === 'running' || status?.phase === 'approval';
  const codexRunning = useMemo(() => (tabs === CODEX_TABS ? conversationAgents(items, live).filter((a) => a.state === 'running').length : 0), [tabs, items, live]);
  // 换栏时回到列表
  const pick = (k) => {
    onAgentSel(null);
    onTab(k);
  };
  return (
    <aside className="chat-side" data-wide={wide && agentSel ? '' : undefined} aria-label={t('chat.side_toggle')}>
      <header className="chat-side-head">
        <nav className="chat-side-tabs" role="tablist">
          {tabs.map((k) => (
            <button key={k} type="button" role="tab" aria-selected={tab === k} onClick={() => pick(k)}>
              {t(`chat.side_${k}`)}
              {k === 'tasks' && running > 0 && <span className="chat-side-count">{running}</span>}
              {k === 'agents' && codexRunning > 0 && <span className="chat-side-count">{codexRunning}</span>}
            </button>
          ))}
        </nav>
        <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('action.close')} title={t('action.close')}>
          <X size={17} />
        </button>
      </header>
      <div className="chat-side-body">
        <ErrorBoundary resetKey={tab}>
          {tab === 'changes' && <ChangesTab id={id} items={items} onTab={onTermTab} />}
          {tab === 'tasks' && <TasksTab id={id} sel={agentSel} onSel={onAgentSel} />}
          {tab === 'agents' && <AgentsTab id={id} items={items} live={live} sel={agentSel} onSel={onAgentSel} />}
          {tab === 'usage' && <UsageTab id={id} status={status} onCompact={onCompact} />}
          {tab === 'config' && <ConfigTab id={id} />}
          {tab === 'resources' && <ResourcesTab id={id} items={items} />}
          {tab === 'outline' && <OutlineTab items={items} {...outline} />}
        </ErrorBoundary>
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

/** Codex 运行中：单色的 Codex 标志轻轻呼吸（docs/UI_REDESIGN_2026-10.md §3） */
function CodexGlyph() {
  return (
    <span className="chat-working-mark codex" aria-hidden="true">
      <LaunchIcon launch="codex" size={14} mono />
    </span>
  );
}

function Live({ status, items, since }) {
  const { t, i18n } = useT();
  const agent = useContext(AgentCtx).id;
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
  const act = status?.agent === 'claude' || !status?.agent ? spinnerText(status?.activity, i18n.language) : codexActivity(t, status?.activity, status?.tool);
  return (
    <>
      {shown && <div className="chat-preview">{preview}</div>}
      <p className="chat-working">
        {agent === 'codex' ? <CodexGlyph /> : <SpinGlyph />}
        <span className="ellipsis">{act.text || (status?.tool ? t('chat.running_tool', { tool: status.tool }) : t('chat.working'))}</span>
        {act.orig && <span className="chat-working-orig" lang="en">{act.orig}</span>}
        {act.detail && (
          <span className="chat-working-detail" data-code={act.code ? '' : undefined} title={act.code ? act.detail : undefined}>
            {act.detail}
          </span>
        )}
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
    return SIDE_TABS.includes(v) || CODEX_TABS.includes(v) ? v : null;
  } catch {
    return null;
  }
};

/**
 * id：终端；active：当前显示的是“对话”标签；touch：手机（回车换行，用按钮发送）；
 * offline：终端所在的电脑连不上（对话照样显示，连不上时不能发送）；onTab(tab)：切到终端的别的标签（例如右侧“改动”里的“在 Git 里查看”）。
 * 对话页里不出现终端：Claude Code 的选择框显示成弹窗，界面做不了的就不做，用户自己切到“终端”标签去。
 */
export default function ChatView({ id, launch, active, touch, offline = false, onTab, onDraftBack }) {
  const router = useRouter();
  const chat = useChat(id, active);
  const { status, items } = chat;
  // 草稿页发出的第一句：起来前先显示；发出去、记录里有了之后换成真的那条
  const firstRaw = useFirstSend(id);
  const first = firstRaw && !(firstRaw.state === 'sent' && items.some((x) => x.k === 'user')) ? firstRaw : null;
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
  // 对话里、资源栏里点到的文件在页面里预览
  const files = usePreviewLinks();
  const knownDir = useSessions()?.known(id)?.workdir;
  const cwd = status?.cwd ?? knownDir ?? null;
  const convFiles = useMemo(() => conversationFiles(items, cwd), [items, cwd]);
  const previewCtx = useMemo(() => ({ open: files.open, cwd, files: convFiles }), [files.open, cwd, convFiles]);
  // 右侧面板里打开着的子代理（对话里的卡片、运行中的子代理条、面板里的列表都能打开）；看步骤时面板可以加宽
  const [agentSel, setAgentSel] = useState(null);
  const [wide, setWide] = useState(false);
  const openAgent = useCallback(
    (a) => {
      setAgentSel(a);
      setSide(agentId === 'claude' ? 'tasks' : 'agents');
    },
    [agentId, setSide],
  );
  const sideCtx = useMemo(() => ({ openAgent, wide, onWide: setWide }), [openAgent, wide]);
  const artifactCtx = useMemo(() => {
    const list = arts?.items ?? [];
    const links = {};
    for (const a of list) {
      const url = artifactUrl(id, a.path, !a.viewable);
      // 文字里的写法，加上完整路径的两种斜杠（Codex 引用文件时用正斜杠，路径里可能有空格）
      for (const m of [...(a.mentions ?? []), a.path, a.path.replaceAll('\\', '/')]) links[m] = url;
    }
    return { list, links: list.length ? links : null, fileUrl: (p) => localFileUrl(id, p) };
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
  // 查找栏：已经开着时再按 Ctrl/⌘ F 是回到查找框
  const [finding, setFinding] = useState(false);
  const onFind = useCallback(() => {
    setFinding((was) => {
      if (was) window.dispatchEvent(new Event('ll-chat-find'));
      return true;
    });
  }, []);
  useChatKeys({ active, list, inner, onFind });
  const known = useSessions()?.known(id);
  const title = known?.agent_title || known?.name || '';
  const toast = useToast();
  const onAction = (a) => {
    if (SIDE_TABS.includes(a) || CODEX_TABS.includes(a)) setSide(a);
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
      <PreviewCtx.Provider value={previewCtx}>
      <SideCtx.Provider value={sideCtx}>
    <div className="chat-view" data-side={side ? '' : undefined} data-agent={agentId} onClick={files.onClick}>
      {files.viewer}
      <div className="chat-col">
        <div className="chat-main">
          {finding && <FindBar list={list} inner={inner} more={chat.start > 0} deps={[items, side]} onClose={() => setFinding(false)} />}
          <div className="chat-list" ref={list} onScroll={onScroll} tabIndex={-1}>
            <div className="chat-items" ref={inner}>
              {chat.start > 0 && <p className="chat-event">{t('chat.older')}</p>}
              {!items.length && status && phase !== 'detached' && !first && (
                <div className="chat-empty">
                  <span className="chat-empty-mark" aria-hidden="true">
                    {agentId === 'codex' ? <LaunchIcon launch="codex" size={40} mono /> : <Asterisk size={40} />}
                  </span>
                  <p className="chat-empty-title">{t('chat.empty')}</p>
                  {/* 几个常用的开头：点了填进输入框，可以改了再发 */}
                  <div className="chat-starters">
                    {STARTERS.map((k) => (
                      <button key={k} type="button" className="chat-starter" onClick={() => fillInput(id, t(`chat.starter.${k}`))}>
                        {t(`chat.starter.${k}`)}
                      </button>
                    ))}
                  </div>
                  <ChatHowTo agent={agentId} touch={touch} />
                  <p className="chat-note">{t('chat.beta_note')}</p>
                </div>
              )}
              <Rows id={id} items={items} live={live} />
              {first && <FirstSend id={id} first={first} onBack={onDraftBack} />}
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
                <button type="button" className="chat-event chat-bg-note" onClick={() => openAgent(null)}>
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
          {/* 第一句在等它起来：上面已经写着“正在启动”，这里不再说一遍 */}
          {phase === 'detached' && !first && <Detached id={id} status={status} />}
          {status?.limit?.waiting && phase !== 'tui' && <LimitWait id={id} waiting={status.limit.waiting} />}
          {agentId === 'claude' && phase === 'idle' && !status?.limit?.waiting && ACCOUNT_ERRORS.includes(status?.error) && <AccountFail id={id} error={status.error} />}
          {agentId === 'codex' && status != null && <CodexSetup id={id} phase={phase} onTab={onTab} />}
          {phase !== 'detached' && <AgentStrip id={id} items={items} live={live} bg={bg} onOpen={openAgent} />}
          <StatusNotice status={status} online={chat.online} offline={offline} />
          <NotifyAsk active={active} phase={phase} />
          <Composer id={id} active={active} status={status} phase={phase} touch={touch} offline={offline && !chat.online} models={models} usage={usage} items={items} onAction={onAction} onSide={setSide} />
        </div>
      </div>
      {side && (
        <SidePanel
          id={id}
          tab={side}
          onTab={setSide}
          onClose={() => {
            setSide(null);
            setAgentSel(null);
          }}
          status={status}
          items={items}
          onTermTab={(tab) => onTab?.(tab)}
          onCompact={compact}
          outline={{ list, inner, agent: agentName, title, more: chat.start > 0 }}
          agentSel={agentSel}
          onAgentSel={setAgentSel}
          wide={wide}
        />
      )}
    </div>
      </SideCtx.Provider>
      </PreviewCtx.Provider>
      </ArtifactCtx.Provider>
    </AgentCtx.Provider>
  );
}
