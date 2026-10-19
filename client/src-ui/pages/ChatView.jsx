// Claude Code 对话界面（docs/CLAUDE_UI.md §5）：同一个 tmux 会话里的交互式 Claude Code 的第二个视图。
// 内容来自 transcript，状态来自 hooks（服务端经 SSE 推过来）；输入、停止、审批都送回同一个进程。
// 常用的设置（模型、思考强度、权限模式）、后台任务和子代理都能在这里操作；界面处理不了的（登录、少见的对话框……）
// 在对话下方展开同一个会话的终端，操作完自动收起，不用来回切换标签。
import {
  Activity,
  ArrowDown,
  ArrowLeft,
  Bot,
  Brain,
  Check,
  ChevronDown,
  ChevronRight,
  CircleDashed,
  CircleStop,
  Cpu,
  FilePen,
  FileText,
  Gauge,
  Globe,
  Layers,
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
import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import {
  COMMANDS,
  EFFORTS,
  EMPTY,
  MODE_CHOICES,
  MODEL_CHOICES,
  applyEvent,
  backgroundCounts,
  chatRows,
  contentText,
  diffLines,
  diffStat,
  duration,
  latestTodos,
  modelFamily,
  modelLabel,
  opensTui,
  questionAnswers,
  resultsById,
  toolCounts,
  toolSummary,
  toolUseIds,
} from '../shared/chat.js';
import { errorText } from '../shared/i18n.js';
import Markdown from '../shared/Markdown.jsx';
import PopMenu from '../shared/PopMenu.jsx';
import { runUpload } from '../shared/uploader.js';
import { Button, useToast } from '../shared/ui.jsx';

// ---------------- 数据 ----------------

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
  const { t } = useTranslation();
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
  const { t } = useTranslation();
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

/** 工具卡片里的“查看子代理的步骤” */
function SubAgent({ id, block, result }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
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
          <Rows id={id} items={items} live={!done} />
        </div>
      )}
    </div>
  );
}

function ToolBody({ id, block, result }) {
  const { t } = useTranslation();
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
  const { t } = useTranslation();
  const diff = DIFF_TOOLS.includes(block.name) ? diffLines(block.name, block.input, result?.extra) : null;
  const stat = diff ? diffStat(diff) : null;
  // 改动默认展开（看改了什么是最常见的需要），太长的不展开
  const [open, setOpen] = useState(() => Boolean(diff && diff.length <= 40) || block.name === 'TodoWrite');
  const Icon = TOOL_ICONS[block.name] ?? Wrench;
  const state = toolState(block, result, live);
  const name = block.name.startsWith('mcp__') ? block.name.split('__').slice(1).join(' · ') : block.name;
  return (
    <div className="chat-tool" data-state={state}>
      <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
        <Icon size={15} aria-hidden="true" />
        <b>{name}</b>
        <span className="chat-tool-sum ellipsis">{toolSummary(block.name, block.input)}</span>
        {block.input?.run_in_background === true && <span className="badge">{t('chat.agent_background')}</span>}
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

const GROUP_ICONS = { explore: Search, edit: FilePen, bash: SquareTerminal, web: Globe, agent: Bot };

const toolState = (block, result, live) => (result ? (result.is_error ? 'err' : 'ok') : live ? 'run' : 'idle');

/**
 * 连续的同类工具调用（读了 5 个文件、跑了 3 条命令……）合成一行，默认收起，点开再看每一次。
 * 标题：各工具的次数、涉及的文件或命令、改动的增删行数；有正在运行的显示转圈。
 */
function ToolGroup({ id, row, results, live }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const calls = row.calls;
  const states = calls.map((b) => toolState(b, results.get(b.id), live));
  const state = states.includes('run') ? 'run' : states.includes('err') ? 'err' : states.every((x) => x === 'ok') ? 'ok' : 'idle';
  const Icon = GROUP_ICONS[row.cls] ?? TOOL_ICONS[calls[0].name] ?? Wrench;
  const label = toolCounts(calls)
    .map(([name, n]) => t(`chat.count.${name}`, { n, name: name.startsWith('mcp__') ? name.split('__').slice(1).join(' · ') : name, defaultValue: '{{name}} ×{{n}}' }))
    .join(' · ');
  // 正在运行时显示最新的那一次，否则列出涉及的文件、命令
  const running = calls.find((b, n) => states[n] === 'run');
  const sums = running ? [toolSummary(running.name, running.input)] : [...new Set(calls.map((b) => toolSummary(b.name, b.input)).filter(Boolean))];
  let stat = null;
  if (row.cls === 'edit') {
    stat = { add: 0, del: 0 };
    for (const b of calls) {
      const d = diffStat(diffLines(b.name, b.input, results.get(b.id)?.extra));
      stat.add += d.add;
      stat.del += d.del;
    }
  }
  return (
    <div className="chat-tool chat-group" data-state={state}>
      <button type="button" className="chat-tool-head" onClick={() => setOpen(!open)} aria-expanded={open}>
        {open ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
        <Icon size={15} aria-hidden="true" />
        <b>{label}</b>
        <span className="chat-tool-sum ellipsis">{sums.join(', ')}</span>
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
        <div className="chat-group-body">
          {calls.map((b) => (
            <ToolCard key={b.id} id={id} block={b} result={results.get(b.id)} live={live} />
          ))}
        </div>
      )}
    </div>
  );
}

/** 一串条目：assistant 回复拆成文字、思考、工具调用，连续的同类工具调用合成一组（主对话和子代理共用）。 */
function Rows({ id, items, live }) {
  const { t } = useTranslation();
  const results = useMemo(() => resultsById(items), [items]);
  const rows = useMemo(() => chatRows(items, toolUseIds(items)), [items]);
  return rows.map((r) => {
    switch (r.k) {
      case 'text':
        return <Markdown key={r.key} text={r.block.text} />;
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
        return <Item key={r.key} id={id} item={r.item} />;
    }
  });
}

function Thinking({ text }) {
  const { t } = useTranslation();
  return (
    <details className="chat-thinking">
      <summary>
        <Brain size={14} aria-hidden="true" /> {t('chat.thinking')}
      </summary>
      <Pre text={text} />
    </details>
  );
}

/** 不是 assistant 回复的条目（用户消息、单独的工具结果、分隔线、提示……） */
function Item({ id, item }) {
  const { t } = useTranslation();
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

// ---------------- 审批 ----------------

function QuestionForm({ pending, onAnswer, onTerminal, busy }) {
  const { t } = useTranslation();
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
  const { t } = useTranslation();
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
            <Button variant="secondary" busy={busy} onClick={() => answer('allow_session')} title={t('chat.allow_session_hint')}>
              {t('chat.allow_session')}
            </Button>
            {dirs.length > 0 && (
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

/** 快捷设置：模型、思考强度、权限模式（不用到终端里打 /model、按 Shift+Tab） */
function QuickSettings({ id, status, onTerminal }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(null);
  const phase = status?.phase;
  const ready = phase === 'idle' || phase === 'running';
  const set = async (kind, value) => {
    setBusy(kind);
    try {
      await api.post(`/instances/${id}/chat/setting`, { kind, value });
    } catch (e) {
      toast(errorText(t, e), 'error');
      if (e.code === 'CHAT_BUSY') onTerminal(true);
    } finally {
      setBusy(null);
    }
  };
  // 模型、强度要等这一轮做完（运行中斜杠命令会排队）
  const idle = phase === 'idle';
  const fam = modelFamily(status?.model);
  const model = [
    ...MODEL_CHOICES.map((m) => ({ label: t(`chat.model_${m}`), checked: m === 'default' ? false : fam === m, onClick: () => set('model', m), disabled: !idle })),
    {
      label: t('chat.model_more'),
      icon: SquareTerminal,
      disabled: !idle,
      onClick: async () => {
        try {
          await api.post(`/instances/${id}/chat/input`, { text: '/model', images: [] });
          onTerminal(true);
        } catch (e) {
          toast(errorText(t, e), 'error');
        }
      },
    },
  ];
  const effort = EFFORTS.map((x) => ({ label: t(`chat.effort_${x}`), checked: status?.effort === x, onClick: () => set('effort', x), disabled: !idle }));
  const mode = MODE_CHOICES.map((m) => ({ label: t(`chat.mode_${m}`), checked: status?.mode === m, hint: t(`chat.mode_hint_${m}`), onClick: () => set('mode', m) }));
  const chip = 'btn btn-plain chat-chip';
  const modeText = status?.mode ? t(`chat.mode_${MODE_CHOICES.includes(status.mode) || status.mode === 'dontAsk' ? status.mode : 'other'}`, { mode: status.mode }) : t('chat.mode');
  return (
    <>
      <PopMenu items={model} className={chip} icon={busy === 'model' ? LoaderCircle : Cpu} text={modelLabel(status?.model) || t('chat.model')} label={t('chat.model_pick')} disabled={!ready} />
      <PopMenu items={effort} className={chip} icon={busy === 'effort' ? LoaderCircle : Gauge} text={status?.effort ? t(`chat.effort_${status.effort}`) : t('chat.effort')} label={t('chat.effort_pick')} disabled={!ready} />
      <PopMenu items={mode} className={`${chip} chat-chip-mode`} icon={busy === 'mode' ? LoaderCircle : ShieldCheck} text={modeText} label={t('chat.mode_pick')} disabled={!ready} />
    </>
  );
}

function Composer({ id, status, phase, touch, onTerminal }) {
  const { t } = useTranslation();
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
      <div className="chat-input-box">
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
        <div className="chat-input-tools">
          <button type="button" className="btn btn-plain btn-icon" onClick={() => picker.current?.click()} disabled={disabled} aria-label={t('chat.attach')} title={t('chat.attach')}>
            <Paperclip size={18} />
          </button>
          <QuickSettings id={id} status={status} onTerminal={onTerminal} />
          <span className="grow" />
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
    </div>
  );
}

// ---------------- 状态 ----------------

/** 状态、后台任务（还在运行的子代理、后台命令）、展开终端 */
function StatusLine({ status, online, termOpen, onTerminal, onCloseTerminal, onTasks, tasksOpen }) {
  const { t } = useTranslation();
  if (!status) return null;
  const { phase, tool, error, reason } = status;
  const label = phase === 'detached' && reason === 'not_attached' ? 'chat.phase_not_attached' : `chat.phase_${phase}`;
  const bg = backgroundCounts(status);
  return (
    <div className="chat-status" data-phase={phase}>
      <span className="chat-status-dot" aria-hidden="true" />
      <span className="grow ellipsis">
        {!online ? t('chat.reconnecting') : phase === 'running' && tool ? t('chat.running_tool', { tool }) : t(label)}
        {error && <span className="chat-status-err"> · {error}</span>}
      </span>
      {bg.subagent > 0 && (
        <button type="button" className="btn btn-plain chat-term-btn chat-bg-on" onClick={onTasks}>
          <Bot size={15} aria-hidden="true" />
          {t('chat.bg_agents', { count: bg.subagent })}
        </button>
      )}
      {bg.shell > 0 && (
        <button type="button" className="btn btn-plain chat-term-btn chat-bg-on" onClick={onTasks}>
          <Activity size={15} aria-hidden="true" />
          {t('chat.bg_shells', { count: bg.shell })}
        </button>
      )}
      {!bg.subagent && !bg.shell && (
        <button type="button" className="btn btn-plain chat-term-btn" onClick={onTasks} aria-pressed={tasksOpen} title={t('chat.tasks_title')}>
          <Layers size={15} aria-hidden="true" />
          <span className="chat-hide-narrow">{t('chat.tasks')}</span>
        </button>
      )}
      <button type="button" className="btn btn-plain chat-term-btn" onClick={() => (termOpen ? onCloseTerminal() : onTerminal(false))} aria-pressed={termOpen} title={t(termOpen ? 'chat.close_terminal' : 'chat.open_terminal_hint')}>
        <SquareTerminal size={15} aria-hidden="true" />
        {t('chat.open_terminal')}
      </button>
    </div>
  );
}

const STATE_ICONS = { running: LoaderCircle, done: Check, completed: Check, failed: X, killed: CircleStop, stopped: CircleStop, ended: CircleDashed };

function TaskState({ state }) {
  const { t } = useTranslation();
  const Icon = STATE_ICONS[state] ?? CircleDashed;
  return (
    <span className="chat-task-state" data-state={state} title={t(`chat.state_${state}`, { defaultValue: state })}>
      <Icon size={14} className={state === 'running' ? 'spin' : ''} aria-hidden="true" />
    </span>
  );
}

/** 后台命令：命令、状态、输出（展开后运行中每 2 秒刷新）、停止 */
function ShellTask({ id, shell, onTerminal }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [open, setOpen] = useState(false);
  const [out, setOut] = useState(null);
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
  const stop = async () => {
    setStopping(true);
    try {
      await api.post(`/instances/${id}/chat/task/stop`, { tool: shell.tool_use_id });
    } catch (e) {
      toast(errorText(t, e), 'error');
      if (e.code === 'CHAT_SCREEN_CHANGED' || e.code === 'CHAT_BUSY') onTerminal(false);
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
          <Button variant="ghost" className="chat-task-stop" busy={stopping} onClick={stop}>
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
  const { t } = useTranslation();
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

/**
 * “任务”面板（多代理视图）：这个会话的子代理（运行中的在前）和后台命令。打开时每 2 秒刷新。
 * 点子代理看它的每一步；后台命令可以看输出、停止。
 */
function TasksPanel({ id, onClose, onTerminal }) {
  const { t } = useTranslation();
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  const [agentId, setAgentId] = useState(null);
  const [, tick] = useReducer((n) => n + 1, 0);
  useEffect(() => {
    let alive = true;
    let timer = null;
    const load = async () => {
      try {
        const d = await api.get(`/instances/${id}/chat/tasks`);
        if (!alive) return;
        setData(d);
        setError(null);
      } catch (e) {
        if (alive) setError(e);
      }
      if (alive) timer = setTimeout(load, document.hidden ? 6000 : 2000);
    };
    load();
    const iv = setInterval(tick, 1000);
    return () => {
      alive = false;
      clearTimeout(timer);
      clearInterval(iv);
    };
  }, [id]);
  useEffect(() => {
    const esc = (e) => e.key === 'Escape' && (agentId ? setAgentId(null) : onClose());
    document.addEventListener('keydown', esc);
    return () => document.removeEventListener('keydown', esc);
  }, [agentId]);
  const agents = data?.agents ?? [];
  const shells = [...(data?.shells ?? [])].reverse();
  const agent = agents.find((a) => a.id === agentId);
  const runningAgents = agents.filter((a) => a.state === 'running').length;
  const runningShells = shells.filter((s) => s.state === 'running').length;
  return (
    <aside className="chat-side" aria-label={t('chat.tasks_title')}>
      {agent ? (
        <AgentDetail id={id} agent={agent} onBack={() => setAgentId(null)} />
      ) : (
        <>
          <header className="chat-side-head">
            <Layers size={17} aria-hidden="true" />
            <b className="grow">{t('chat.tasks_title')}</b>
            <button type="button" className="btn btn-plain chat-term-btn" onClick={() => onTerminal(false)} title={t('chat.tasks_in_terminal_hint')}>
              <SquareTerminal size={15} aria-hidden="true" />
              {t('chat.open_terminal')}
            </button>
            <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('action.close')} title={t('action.close')}>
              <X size={18} />
            </button>
          </header>
          <div className="chat-side-body">
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
                              {last && <span className="chat-note ellipsis mono">{a.state === 'running' ? '▸ ' : ''}{last}</span>}
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
                {shells.length === 0 ? (
                  <p className="chat-note">{t('chat.shells_empty')}</p>
                ) : (
                  <ul className="chat-tasks">
                    {shells.map((s) => (
                      <ShellTask key={s.tool_use_id} id={id} shell={s} onTerminal={onTerminal} />
                    ))}
                  </ul>
                )}
              </>
            )}
          </div>
        </>
      )}
    </aside>
  );
}

/** Claude Code 没在运行或没接上对话界面时的说明 */
function Detached({ id, status, onTerminal }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const reason = status?.reason ?? 'starting';
  const relaunch = async () => {
    setBusy(true);
    try {
      await api.post(`/instances/${id}/chat/relaunch`);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
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
        <Button variant="ghost" icon={SquareTerminal} onClick={() => onTerminal(false)}>
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
  const { t } = useTranslation();
  const preview = status?.preview ?? '';
  const last = [...items].reverse().find((i) => i.k === 'assistant');
  const lastText = (last?.blocks ?? []).filter((b) => b.t === 'text').map((b) => b.text).join('\n');
  const head = preview.slice(0, 30);
  const shown = preview && !(head && lastText.replace(/\s+/g, '').includes(head.replace(/\s+/g, '')));
  return (
    <>
      {shown && <div className="chat-preview">{preview}</div>}
      <p className="chat-working">
        <LoaderCircle size={14} className="spin" aria-hidden="true" /> {status?.activity || t('chat.working')}
      </p>
    </>
  );
}

/**
 * 页面在后台时：需要确认、要到终端里操作、这一轮做完了，发浏览器通知，标题前面加个标记，回到页面时去掉。
 * `prev` 是 ChatView 记的上一个状态（这里只读）。
 */
function useAttention(id, phase, pendingKind, prev, shells) {
  const { t } = useTranslation();
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

// ---------------- 整体 ----------------

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

/**
 * id：终端；active：当前显示的是“对话”标签；touch：手机（回车换行，用按钮发送）；
 * termOpen：对话下面展开着终端；onTerminal(auto)：展开终端（auto=true 表示是界面自己展开的，操作完会自动收起）；
 * onBack()：收起终端。
 */
export default function ChatView({ id, active, touch, termOpen, onTerminal, onBack }) {
  const { t } = useTranslation();
  const chat = useChat(id);
  const { status, items } = chat;
  const phase = status?.phase ?? 'detached';
  const list = useRef(null);
  const inner = useRef(null);
  const todos = useMemo(() => latestTodos(items), [items]);
  const live = phase === 'running' || phase === 'approval';
  const [tasks, setTasks] = useState(false);
  const bg = backgroundCounts(status);
  // 界面自己展开的终端（/model 之类），终端里操作完回到输入框后收起
  const autoTerm = useRef(false);
  const goTerminal = useCallback(
    (auto) => {
      autoTerm.current = Boolean(auto);
      onTerminal(Boolean(auto));
    },
    [onTerminal],
  );
  const prev = useRef(phase);
  useAttention(id, phase, status?.pending?.kind, prev, bg.shell ?? 0);
  useEffect(() => {
    if (prev.current !== phase) {
      if (phase === 'tui' && active && !termOpen) goTerminal(true);
      if (prev.current === 'tui' && (phase === 'idle' || phase === 'running') && autoTerm.current) {
        autoTerm.current = false;
        onBack();
      }
      prev.current = phase;
    }
  }, [phase, active]);
  const { following, toBottom } = useFollow(list, inner, [items, status?.pending?.id, active, termOpen]);
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
    <div className="chat-view">
      <div className="chat-main">
        <div className="chat-list" ref={list} onScroll={onScroll} tabIndex={-1}>
          <div className="chat-items" ref={inner}>
            {chat.start > 0 && <p className="chat-event">{t('chat.older')}</p>}
            {!items.length && status && phase !== 'detached' && <p className="chat-empty">{t('chat.empty')}</p>}
            <Rows id={id} items={items} live={live} />
            {phase === 'running' && <Live status={status} items={items} />}
            {/* 这一轮做完了，后台还有命令、子代理在跑（Claude Code 在终端里显示“1 shell still running”） */}
            {phase === 'idle' && (bg.shell > 0 || bg.subagent > 0) && (
              <button type="button" className="chat-event chat-bg-note" onClick={() => setTasks(true)}>
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
        {tasks && <TasksPanel id={id} onClose={() => setTasks(false)} onTerminal={goTerminal} />}
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
        {status?.pending && <ApprovalBar id={id} pending={status.pending} onTerminal={() => goTerminal(true)} />}
        {phase === 'detached' && <Detached id={id} status={status} onTerminal={goTerminal} />}
        {phase === 'tui' && (
          <div className="chat-tui">
            <span className="grow">{t(termOpen ? 'chat.tui_hint_open' : 'chat.tui_hint')}</span>
            {!termOpen && (
              <Button icon={SquareTerminal} onClick={() => goTerminal(true)}>
                {t('chat.open_terminal')}
              </Button>
            )}
          </div>
        )}
        <StatusLine status={status} online={chat.online} termOpen={termOpen} onTerminal={goTerminal} onCloseTerminal={onBack} tasksOpen={tasks} onTasks={() => setTasks(!tasks)} />
        {/* 在下面的终端里操作选择框时输入框用不了，把地方让给对话和终端 */}
        {!(phase === 'tui' && termOpen) && <Composer id={id} status={status} phase={phase} touch={touch} onTerminal={goTerminal} />}
      </div>
    </div>
  );
}
