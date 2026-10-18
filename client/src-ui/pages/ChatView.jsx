// Claude Code 对话界面（docs/CLAUDE_UI.md §5）：同一个 tmux 会话里的交互式 Claude Code 的第二个视图。
// 内容来自 transcript，状态来自 hooks（服务端经 SSE 推过来）；输入、停止、审批都送回同一个进程。
// 界面处理不了的（/model 选择器、登录、计划确认之外的对话框……）一键切到“终端”标签，在终端里操作完再回来。
import {
  ArrowDown,
  Bot,
  Brain,
  Check,
  ChevronDown,
  ChevronRight,
  CircleStop,
  FilePen,
  FileText,
  Globe,
  ListTodo,
  LoaderCircle,
  Paperclip,
  Search,
  Send,
  ShieldCheck,
  SquareTerminal,
  TriangleAlert,
  Wrench,
  X,
} from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { COMMANDS, EMPTY, applyEvent, contentText, diffLines, diffStat, latestTodos, opensTui, questionAnswers, resultsById, toolSummary, toolUseIds } from '../shared/chat.js';
import { errorText } from '../shared/i18n.js';
import Markdown from '../shared/Markdown.jsx';
import { runUpload } from '../shared/uploader.js';
import { Button, useToast } from '../shared/ui.jsx';

// ---------------- 数据 ----------------

/** 服务端状态里的 `agent` → 文案里的 {{agent}}；老版本服务端不带 agent，就是 Claude Code */
const AGENT_NAMES = { claude: 'Claude Code', codex: 'Codex' };
const agentName = (status) => AGENT_NAMES[status?.agent] ?? AGENT_NAMES.claude;
const AgentName = createContext(AGENT_NAMES.claude);
/** 服务端给的能力（`status.caps`）：界面只按它显示按钮。老版本服务端不带 caps，就是全部可用（Claude Code） */
const NO_CAPS = {};
const Caps = createContext(NO_CAPS);

/** Codex 屏幕上的 “Working (12s • esc to interrupt)” → 正在工作（12 秒） */
function activityText(t, activity) {
  const m = /\bWorking \((\d+)s\b/.exec(activity ?? '');
  return m ? t('chat.codex_working', { s: m[1] }) : activity;
}

/** 和 useTranslation 一样，但文案里的 {{agent}} 换成这个会话的 AI 工具 */
function useT(agent) {
  const { t } = useTranslation();
  const ctx = useContext(AgentName);
  const name = agent ?? ctx;
  return { t: useCallback((key, opts) => t(key, { agent: name, ...opts }), [t, name]) };
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

const TOOL_ICONS = { Bash: SquareTerminal, Read: FileText, Edit: FilePen, MultiEdit: FilePen, Write: FilePen, NotebookEdit: FilePen, Grep: Search, Glob: Search, WebFetch: Globe, WebSearch: Globe, TodoWrite: ListTodo, Task: Bot, Agent: Bot };
const DIFF_TOOLS = ['Edit', 'MultiEdit', 'Write'];
const DIFF_LIMIT = 400;

function Pre({ text, className = '' }) {
  if (!text) return null;
  return <pre className={`chat-pre ${className}`}>{text}</pre>;
}

function DiffView({ lines }) {
  const { t } = useT();
  const [all, setAll] = useState(false);
  const shown = all ? lines : lines.slice(0, DIFF_LIMIT);
  return (
    <div className="chat-diff">
      {shown.map((l, n) => (
        // eslint-disable-next-line react/no-array-index-key -- 行没有别的标识
        <div key={n} className={`chat-diff-line d${l.t === '+' ? 'add' : l.t === '-' ? 'del' : l.t === '@' ? 'hunk' : 'ctx'}`}>
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
      <Pre text={full ?? result.content} className={result.is_error ? 'err' : ''} />
      {result.images > 0 && <p className="chat-note">{t('chat.images', { n: result.images })}</p>}
      {result.truncated && full == null && (
        <button type="button" className="chat-more" onClick={more} disabled={busy}>
          {t('chat.load_full')}
        </button>
      )}
    </>
  );
}

/**
 * 子代理自己的步骤（它的 transcript 单独存放）：点开后加载，没结束时每 2 秒刷新。
 * 后台运行的子代理一开始就有结果（“已在后台启动”），所以按内容判断是否结束：最后一条是文字回复就算结束。
 */
function SubAgent({ id, block, result }) {
  const { t } = useT();
  const [open, setOpen] = useState(false);
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const agent = result?.extra?.agentId ?? '';
  const last = data?.items?.[data.items.length - 1];
  const done = Boolean(last && last.k === 'assistant' && last.blocks?.some((b) => b.t === 'text') && !last.blocks?.some((b) => b.t === 'tool_use'));
  useEffect(() => {
    if (!open) return undefined;
    let alive = true;
    let timer = null;
    const load = async () => {
      try {
        const d = await api.get(`/instances/${id}/chat/agent?tool=${encodeURIComponent(block.id)}&agent=${encodeURIComponent(agent)}`);
        if (!alive) return;
        setData(d);
        setError(null);
        const end = d.items?.[d.items.length - 1];
        const finished = end?.k === 'assistant' && end.blocks?.some((b) => b.t === 'text') && !end.blocks?.some((b) => b.t === 'tool_use');
        if (!finished) timer = setTimeout(load, 2000);
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
  }, [open, agent]);
  const items = data?.items ?? [];
  const results = useMemo(() => resultsById(items), [items]);
  const used = useMemo(() => toolUseIds(items), [items]);
  return (
    <div className="chat-sub">
      <button type="button" className="chat-more" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />} {t('chat.sub_steps')}
        {open && !done && <LoaderCircle size={13} className="spin" aria-hidden="true" />}
      </button>
      {open && (
        <div className="chat-sub-body">
          {error && !data && <p className="chat-note">{t('chat.sub_waiting')}</p>}
          {items.map((it) => (it.k === 'tool_result' && used.has(it.tool_use_id) ? null : <Item key={it.key} id={id} item={it} results={results} live={!done} />))}
        </div>
      )}
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
    case 'Edit':
    case 'MultiEdit':
    case 'Write':
      return (
        <>
          <p className="chat-note mono">{input.file_path}</p>
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
          {result && <Markdown text={result.content || ''} />}
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
      return (
        <>
          {Object.keys(input).length > 0 && <Pre text={JSON.stringify(input, null, 2)} />}
          {block.truncated && <p className="chat-note">{t('chat.input_truncated')}</p>}
          <ResultText id={id} result={result} />
        </>
      );
  }
}

function ToolCard({ id, block, result, live }) {
  const { t } = useT();
  const diff = DIFF_TOOLS.includes(block.name) ? diffLines(block.name, block.input, result?.extra) : null;
  const stat = diff ? diffStat(diff) : null;
  // 改动默认展开（看改了什么是最常见的需要），太长的不展开
  const [open, setOpen] = useState(() => Boolean(diff && diff.length <= 40) || block.name === 'TodoWrite');
  const Icon = TOOL_ICONS[block.name] ?? Wrench;
  const state = result ? (result.is_error ? 'err' : 'ok') : live ? 'run' : 'idle';
  const name = block.name.startsWith('mcp__') ? block.name.split('__').slice(1).join(' · ') : block.name;
  return (
    <div className="chat-tool" data-state={state}>
      <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
        <Icon size={15} aria-hidden="true" />
        <b>{name}</b>
        <span className="chat-tool-sum ellipsis">{toolSummary(block.name, block.input)}</span>
        {stat && (stat.add > 0 || stat.del > 0) && (
          <span className="chat-stat">
            <i className="add">+{stat.add}</i> <i className="del">−{stat.del}</i>
          </span>
        )}
        <span className="chat-tool-state" title={t(`chat.tool_${state}`)}>
          {state === 'run' ? <LoaderCircle size={14} className="spin" /> : state === 'ok' ? <Check size={14} /> : state === 'err' ? <X size={14} /> : null}
        </span>
      </button>
      {open && (
        <div className="chat-tool-body">
          <ToolBody id={id} block={block} result={result} />
        </div>
      )}
    </div>
  );
}

function Thinking({ text }) {
  const { t } = useT();
  return (
    <details className="chat-thinking">
      <summary>
        <Brain size={14} aria-hidden="true" /> {t('chat.thinking')}
      </summary>
      <Pre text={text} />
    </details>
  );
}

function Item({ id, item, results, live }) {
  const { t } = useT();
  switch (item.k) {
    case 'user':
      return (
        <div className="chat-user">
          <div className="chat-bubble">
            {item.text}
            {item.images > 0 && <span className="chat-note">{t('chat.images', { n: item.images })}</span>}
          </div>
        </div>
      );
    case 'assistant':
      return (
        <div className="chat-assistant">
          {item.blocks.map((b, n) => {
            const key = b.id ?? `${item.key}:${n}`;
            if (b.t === 'text') return <Markdown key={key} text={b.text} />;
            if (b.t === 'thinking') return <Thinking key={key} text={b.text} />;
            if (b.t === 'tool_use') return <ToolCard key={key} id={id} block={b} result={results.get(b.id)} live={live} />;
            return (
              <p key={key} className="chat-note">
                {t('chat.unknown_block', { type: b.raw_type })}
              </p>
            );
          })}
        </div>
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

// ---------------- 审批 ----------------

function QuestionForm({ pending, onAnswer, onTerminal, busy }) {
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
          {(q.options ?? []).map((o) => (
            <label key={o.label} className="chat-q-opt">
              <input type={q.multiSelect ? 'checkbox' : 'radio'} name={`q${n}`} checked={(picked[n]?.labels ?? []).includes(o.label)} onChange={() => toggle(n, o.label, q.multiSelect)} />
              <span>
                <b>{o.label}</b>
                {o.description && o.description !== o.label && <span className="chat-note">{o.description}</span>}
              </span>
            </label>
          ))}
          <input className="chat-q-other" placeholder={t('chat.q_other')} value={picked[n]?.other ?? ''} onChange={(e) => setPicked((p) => ({ ...p, [n]: { ...p[n], other: e.target.value } }))} />
        </fieldset>
      ))}
      <div className="chat-approve-acts">
        <Button busy={busy} disabled={!ready} onClick={() => onAnswer('answer', { answers })}>
          {t('chat.q_submit')}
        </Button>
        <Button variant="ghost" icon={SquareTerminal} onClick={onTerminal}>
          {t('chat.in_terminal')}
        </Button>
      </div>
    </>
  );
}

function ApprovalBar({ id, pending, onTerminal }) {
  const { t } = useT();
  const caps = useContext(Caps);
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [denying, setDenying] = useState(false);
  const [reason, setReason] = useState('');
  const [open, setOpen] = useState(false);
  useEffect(() => {
    setDenying(false);
    setReason('');
    setOpen(false);
  }, [pending.id]);
  const answer = async (decision, extra = {}) => {
    setBusy(true);
    try {
      if (decision === 'terminal') onTerminal();
      await api.post(`/instances/${id}/chat/permission`, { id: pending.id, decision, ...extra });
    } catch (e) {
      // 已经在终端里答过了：审批条会随状态更新消失
      if (e.code !== 'CHAT_STALE') toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const plan = async (choice) => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/plan`, { choice });
    } catch (e) {
      toast(errorText(t, e), 'error');
      if (e.code === 'CHAT_SCREEN_CHANGED') onTerminal();
    } finally {
      setBusy(false);
    }
  };
  const input = pending.input ?? {};
  const dirs = (pending.suggestions ?? []).filter?.((s) => s.type === 'addDirectories') ?? [];
  let body;
  if (pending.kind === 'question') {
    body = <QuestionForm pending={pending} onAnswer={answer} onTerminal={() => answer('terminal')} busy={busy} />;
  } else if (pending.kind === 'plan') {
    body = (
      <>
        <div className="chat-plan">
          <Markdown text={input.plan ?? ''} />
        </div>
        <div className="chat-approve-acts">
          <Button busy={busy} onClick={() => plan(1)}>
            {t('chat.plan_auto')}
          </Button>
          <Button variant="secondary" busy={busy} onClick={() => plan(2)}>
            {t('chat.plan_manual')}
          </Button>
          <Button variant="ghost" icon={SquareTerminal} onClick={() => answer('terminal')}>
            {t('chat.plan_change')}
          </Button>
        </div>
      </>
    );
  } else {
    const diff = DIFF_TOOLS.includes(pending.tool) ? diffLines(pending.tool, input) : null;
    body = (
      <>
        {pending.tool === 'Bash' ? (
          <Pre text={`$ ${input.command ?? ''}`} className="cmd" />
        ) : (
          <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
            {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
            <span className="chat-tool-sum ellipsis">{toolSummary(pending.tool, input) || t('chat.details')}</span>
          </button>
        )}
        {open && (diff ? <DiffView lines={diff} /> : <Pre text={JSON.stringify(input, null, 2)} />)}
        {input.description && <p className="chat-note">{input.description}</p>}
        {denying ? (
          <div className="chat-approve-acts">
            <input className="chat-q-other grow" autoFocus placeholder={t('chat.deny_reason')} value={reason} onChange={(e) => setReason(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && answer('deny', { message: reason })} />
            <Button variant="danger" busy={busy} onClick={() => answer('deny', { message: reason })}>
              {t('chat.deny')}
            </Button>
            <Button variant="ghost" onClick={() => setDenying(false)}>
              {t('action.cancel')}
            </Button>
          </div>
        ) : (
          <div className="chat-approve-acts">
            <Button busy={busy} onClick={() => answer('allow')}>
              {t('chat.allow')}
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
            <Button variant="danger" onClick={() => setDenying(true)}>
              {t('chat.deny')}
            </Button>
            <Button variant="ghost" icon={SquareTerminal} onClick={() => answer('terminal')}>
              {t('chat.in_terminal')}
            </Button>
          </div>
        )}
      </>
    );
  }
  const title = pending.kind === 'question' ? t('chat.q_title') : pending.kind === 'plan' ? t('chat.plan_title') : t('chat.perm_title', { tool: pending.tool });
  return (
    <section className="chat-approve" aria-live="polite">
      <header>
        {pending.kind === 'permission' ? <ShieldCheck size={16} aria-hidden="true" /> : <Bot size={16} aria-hidden="true" />}
        <b>{title}</b>
      </header>
      {body}
    </section>
  );
}

// ---------------- 输入框 ----------------

let attachSeq = 0;

function Composer({ id, phase, touch, onTerminal }) {
  const { t } = useT();
  const caps = useContext(Caps);
  const toast = useToast();
  const [text, setText] = useState('');
  const [files, setFiles] = useState([]);
  const [sending, setSending] = useState(false);
  const area = useRef(null);
  const picker = useRef(null);
  const disabled = phase === 'tui' || phase === 'approval' || phase === 'detached';
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.35))}px`;
  }, [text]);
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
  const send = async () => {
    const body = text.trim();
    const images = files.filter((f) => f.path).map((f) => f.path);
    if ((!body && !images.length) || sending || uploading || disabled) return;
    // 第一次发消息时（有用户操作）申请通知权限：需要确认、做完了在后台也能知道
    if (typeof Notification !== 'undefined' && Notification.permission === 'default') Notification.requestPermission().catch?.(() => {});
    setSending(true);
    try {
      const r = await api.post(`/instances/${id}/chat/input`, { text: body, images });
      setText('');
      files.forEach((f) => URL.revokeObjectURL(f.url));
      setFiles([]);
      if (r?.delivered === false) toast(t('chat.not_delivered'), 'error');
      if (opensTui(body)) onTerminal(true);
    } catch (e) {
      toast(errorText(t, e), 'error');
      if (e.code === 'CHAT_BUSY') onTerminal(true);
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
    if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing && !touch) {
      e.preventDefault();
      send();
    }
  };
  // 输入 / 时：内置的常用命令 + 这个项目和用户自己的命令、技能（第一次用到时取）
  const [custom, setCustom] = useState(null);
  const wantSlash = /^\/\S*$/.test(text);
  useEffect(() => {
    if (wantSlash && custom === null) {
      setCustom([]);
      api.get(`/instances/${id}/chat/commands`).then((r) => setCustom(r?.items ?? []), () => {});
    }
  }, [wantSlash]);
  const slash = useMemo(() => {
    if (!wantSlash) return [];
    const q = text.toLowerCase();
    const builtin = COMMANDS.map(([name, tui]) => ({ name, tui, description: t(`chat.cmd.${name.slice(1)}`) }));
    const names = new Set(builtin.map((c) => c.name));
    const mine = (custom ?? []).filter((c) => !names.has(c.name)).map((c) => ({ ...c, tui: false }));
    const all = [...builtin, ...mine];
    const hit = all.filter((c) => c.name.toLowerCase().startsWith(q));
    return (hit.length ? hit : all.filter((c) => c.name.toLowerCase().includes(q.slice(1)))).slice(0, 40);
  }, [wantSlash, text, custom]);
  return (
    <div className="chat-composer">
      {slash.length > 0 && (
        <ul className="chat-slash" role="listbox">
          {slash.map((c) => (
            <li key={c.name}>
              <button
                type="button"
                onClick={() => {
                  setText(`${c.name} `);
                  area.current?.focus();
                }}
              >
                <b>{c.name}</b>
                <span className="chat-note ellipsis">{c.description}</span>
                {c.tui && <span className="badge">{t('chat.cmd_terminal')}</span>}
                {c.source && <span className="badge">{t(`chat.cmd_source_${c.source}`)}</span>}
              </button>
            </li>
          ))}
        </ul>
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
      <div className="chat-input-row">
        <button type="button" className="btn btn-plain btn-icon" onClick={() => picker.current?.click()} disabled={disabled || caps.images === false} aria-label={t('chat.attach')} title={caps.images === false ? t('chat.no_images') : t('chat.attach')}>
          <Paperclip size={18} />
        </button>
        <input ref={picker} type="file" accept="image/*" multiple hidden onChange={(e) => {
          addFiles([...e.target.files]);
          e.target.value = '';
        }} />
        <textarea
          ref={area}
          rows={1}
          value={text}
          disabled={disabled}
          placeholder={t(disabled ? `chat.placeholder_${phase}` : touch ? 'chat.placeholder_touch' : 'chat.placeholder')}
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
        {phase === 'running' && (
          <button type="button" className="btn btn-plain btn-icon chat-stop" onClick={stop} aria-label={t('chat.stop')} title={t('chat.stop')}>
            <CircleStop size={20} />
          </button>
        )}
        <button type="button" className="btn btn-primary btn-icon chat-send" onClick={send} disabled={disabled || sending || uploading || (!text.trim() && !files.some((f) => f.path))} aria-label={t('chat.send')} title={t('chat.send')}>
          {sending ? <LoaderCircle size={18} className="spin" /> : <Send size={18} />}
        </button>
      </div>
    </div>
  );
}

// ---------------- 状态 ----------------

const MODES = ['default', 'acceptEdits', 'plan', 'auto', 'bypassPermissions', 'dontAsk'];

function StatusLine({ status, online, onTerminal }) {
  const { t } = useT();
  if (!status) return null;
  const { phase, tool, mode, error, reason } = status;
  const label = phase === 'detached' && reason === 'not_attached' ? 'chat.phase_not_attached' : `chat.phase_${phase}`;
  return (
    <div className="chat-status" data-phase={phase}>
      <span className="chat-status-dot" aria-hidden="true" />
      <span className="grow ellipsis">
        {!online ? t('chat.reconnecting') : phase === 'running' && tool ? t('chat.running_tool', { tool }) : t(label)}
        {error && <span className="chat-status-err"> · {error}</span>}
      </span>
      {mode && <span className="badge">{t(`chat.mode_${MODES.includes(mode) ? mode : 'other'}`, { mode })}</span>}
      <button type="button" className="btn btn-plain chat-term-btn" onClick={() => onTerminal(false)}>
        <SquareTerminal size={15} aria-hidden="true" />
        {t('chat.open_terminal')}
      </button>
    </div>
  );
}

/** Claude Code 没在运行或没接上对话界面时的说明 */
function Detached({ id, status, onTerminal }) {
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
  // 开启对话界面之前就在运行、或者在终端里手动开的：重启这个终端，带着 hooks 重新启动
  const restart = () => run(`/instances/${id}/restart`);
  return (
    <div className="chat-detached">
      <b>{t(`chat.detached_${reason}_title`, { defaultValue: t('chat.detached_starting_title') })}</b>
      <p className="muted">{t(`chat.detached_${reason}_body`, { defaultValue: '' })}</p>
      <div className="chat-approve-acts">
        {(reason === 'not_running' || reason === 'ended') && (
          <Button busy={busy} onClick={relaunch}>
            {t(status?.session_id ? 'chat.relaunch_resume' : 'chat.relaunch')}
          </Button>
        )}
        {reason === 'not_attached' && !confirming && (
          <Button onClick={() => setConfirming(true)}>{t('chat.restart_attach')}</Button>
        )}
        {reason === 'not_attached' && confirming && (
          <>
            <p className="chat-note" style={{ flexBasis: '100%', margin: 0 }}>{t('chat.restart_attach_confirm')}</p>
            <Button variant="danger" busy={busy} onClick={restart}>
              {t('chat.restart_attach')}
            </Button>
            <Button variant="ghost" onClick={() => setConfirming(false)}>
              {t('action.cancel')}
            </Button>
          </>
        )}
        <Button variant="ghost" icon={SquareTerminal} onClick={() => onTerminal(false)}>
          {t('chat.open_terminal')}
        </Button>
      </div>
    </div>
  );
}

/** TUI 上是“信任这个文件夹吗”：在对话界面里直接确认（服务端会先核对屏幕上还是这个对话框） */
function TrustFolder({ id, dialog, onTerminal }) {
  const { t } = useT();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const trust = async () => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/dialog`, { kind: 'trust_folder' });
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="chat-detached">
      <b>{t('chat.trust_title')}</b>
      {dialog.path && <p className="chat-note mono" style={{ wordBreak: 'break-all' }}>{dialog.path}</p>}
      <p className="muted">{t('chat.trust_body')}</p>
      <div className="chat-approve-acts">
        <Button busy={busy} onClick={trust}>
          {t('chat.trust_ok')}
        </Button>
        <Button variant="ghost" icon={SquareTerminal} onClick={onTerminal}>
          {t('chat.open_terminal')}
        </Button>
      </div>
    </div>
  );
}

/**
 * 运行中：屏幕上的状态行（例如“Symbioting… (6s · ↓ 274 tokens)”）和正在输出的那段回复（灰色）。
 * transcript 要等整段写完才有，写进来之后这里自动不再显示。
 */
function Live({ status, items }) {
  const { t } = useT();
  const preview = status?.preview ?? '';
  const last = [...items].reverse().find((i) => i.k === 'assistant');
  const lastText = (last?.blocks ?? []).filter((b) => b.t === 'text').map((b) => b.text).join('\n');
  const head = preview.slice(0, 30);
  const shown = preview && !(head && lastText.replace(/\s+/g, '').includes(head.replace(/\s+/g, '')));
  return (
    <>
      {shown && <div className="chat-preview">{preview}</div>}
      <p className="chat-working">
        <LoaderCircle size={14} className="spin" aria-hidden="true" /> {activityText(t, status?.activity) || t('chat.working')}
      </p>
    </>
  );
}

/**
 * 页面在后台时：需要确认、要到终端里操作、这一轮做完了，发浏览器通知，标题前面加个标记，回到页面时去掉。
 * `prev` 是 ChatView 记的上一个状态（这里只读）。
 */
function useAttention(id, phase, pendingKind, prev, agent) {
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
        const n = new Notification(t(`chat.${what}`), { body: mark.current, tag: `ll-chat-${id}` });
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

// ---------------- 整体 ----------------

/**
 * id：终端；active：当前显示的是“对话”标签；touch：手机（回车换行，用按钮发送）；
 * onTerminal(auto)：切到“终端”标签（auto=true 表示是界面自己切过去的，操作完会自动切回来）；onBack()：切回“对话”。
 */
export default function ChatView({ id, active, touch, onTerminal, onBack }) {
  const chat = useChat(id);
  const { status, items } = chat;
  const agent = agentName(status);
  const { t } = useT(agent);
  const phase = status?.phase ?? 'detached';
  const dialogKind = status?.dialog?.kind ?? null;
  const list = useRef(null);
  const stick = useRef(true);
  const [atBottom, setAtBottom] = useState(true);
  const results = useMemo(() => resultsById(items), [items]);
  const usedIds = useMemo(() => toolUseIds(items), [items]);
  const todos = useMemo(() => latestTodos(items), [items]);
  const live = phase === 'running' || phase === 'approval';
  // 界面自己切去终端的（/model 之类），终端里操作完回到输入框后切回来
  const autoTerm = useRef(false);
  const goTerminal = useCallback(
    (auto) => {
      autoTerm.current = Boolean(auto);
      onTerminal();
    },
    [onTerminal],
  );
  const prev = useRef(phase);
  useAttention(id, phase, status?.pending?.kind, prev, agent);
  useEffect(() => {
    if (prev.current !== phase) {
      if (phase === 'tui' && active && !dialogKind) goTerminal(true);
      if (prev.current === 'tui' && (phase === 'idle' || phase === 'running') && autoTerm.current && !active) {
        autoTerm.current = false;
        onBack();
      }
      prev.current = phase;
    }
  }, [phase, active, dialogKind]);
  // 停在底部时跟着新内容走（Markdown 是异步渲染的，内容高度会在之后变化，所以看尺寸而不是看条目）
  const inner = useRef(null);
  useEffect(() => {
    const el = list.current;
    if (!el || !inner.current || typeof ResizeObserver === 'undefined') return undefined;
    const ro = new ResizeObserver(() => {
      if (stick.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(inner.current);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  useEffect(() => {
    const el = list.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [items, status?.pending?.id, active]);
  const onScroll = async (e) => {
    const el = e.currentTarget;
    const bottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
    stick.current = bottom;
    setAtBottom(bottom);
    if (el.scrollTop < 40 && chat.start > 0) {
      const before = el.scrollHeight;
      await chat.loadOlder();
      requestAnimationFrame(() => {
        el.scrollTop += el.scrollHeight - before;
      });
    }
  };
  const toBottom = () => {
    stick.current = true;
    if (list.current) list.current.scrollTop = list.current.scrollHeight;
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
    <AgentName.Provider value={agent}>
      <Caps.Provider value={status?.caps ?? NO_CAPS}>
      <div className="chat-view">
        <div className="chat-list" ref={list} onScroll={onScroll}>
          <div className="chat-items" ref={inner}>
            {status?.untested_version && (
              <p className="chat-event chat-warn">
                <TriangleAlert size={13} aria-hidden="true" /> {t('chat.untested_version', { version: status.version })}
              </p>
            )}
            {chat.start > 0 && <p className="chat-event">{t('chat.older')}</p>}
            {!items.length && status && phase !== 'detached' && <p className="chat-empty">{t('chat.empty')}</p>}
            {items.map((item) => (item.k === 'tool_result' && usedIds.has(item.tool_use_id) ? null : <Item key={item.key} id={id} item={item} results={results} live={live} />))}
            {phase === 'running' && <Live status={status} items={items} />}
            {status?.stats?.failed > 0 && (
              <p className="chat-event">
                <TriangleAlert size={13} aria-hidden="true" /> {t('chat.format_warning', { version: status.version ?? '?' })}
              </p>
            )}
          </div>
        </div>
        {!atBottom && (
          <button type="button" className="chat-to-bottom" onClick={toBottom} aria-label={t('chat.to_bottom')}>
            <ArrowDown size={16} />
          </button>
        )}
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
          {status?.pending && <ApprovalBar id={id} pending={status.pending} onTerminal={() => goTerminal(true)} />}
          {phase === 'detached' && <Detached id={id} status={status} onTerminal={goTerminal} />}
          {phase === 'tui' && dialogKind === 'trust_folder' && <TrustFolder id={id} dialog={status.dialog} onTerminal={() => goTerminal(true)} />}
          {phase === 'tui' && dialogKind !== 'trust_folder' && (
            <div className="chat-tui">
              <span className="grow">{t('chat.tui_hint')}</span>
              <Button icon={SquareTerminal} onClick={() => goTerminal(true)}>
                {t('chat.open_terminal')}
              </Button>
            </div>
          )}
          <StatusLine status={status} online={chat.online} onTerminal={goTerminal} />
          <Composer id={id} phase={phase} touch={touch} onTerminal={goTerminal} />
        </div>
      </div>
      </Caps.Provider>
    </AgentName.Provider>
  );
}
