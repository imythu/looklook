// 会话页的“对话”看法：同一个 AI 终端（Codex、Claude Code、DSH 终端版），换成像聊天软件一样的显示
// （和手机上的“终端 / 对话 / 改动”切换一样）。
// - 消息和 AI 做的每一步来自工具自己的会话记录（客户端 src/convo/live.rs），步骤的说明和风险由客户端按规则给出
//   （src/explain.rs），认不出的老实写“运行了一个程序”并给出原命令。
// - “等你同意”来自终端屏幕（src/screen.rs）；点按钮时客户端先确认屏幕上还是同一个提示，再按对应的键。
// - 回复照样送进终端（和终端下面的输入框一样走粘贴），所以随时切回“终端”看到的是同一段对话。
import { AlertTriangle, Check, ChevronRight, Circle, Loader2, ShieldCheck, SquareTerminal, X } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import Composer, { sendToTerminal } from '../shared/Composer.jsx';
import { termSend } from '../shared/direct.js';
import { errorText } from '../shared/i18n.js';
import Markdown from '../shared/Markdown.jsx';
import { Button, useToast } from '../shared/ui.jsx';

const POLL_MS = 2000;

/** 每两秒问一次（页面在后台或不是当前看法时不问）；只取新增的条目。 */
function useChat(id, active) {
  const [state, setState] = useState({ data: null, entries: [], base: 0, error: null });
  const ref = useRef(state);
  ref.current = state;
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!active) return undefined;
    let alive = true;
    let timer;
    const load = async () => {
      if (document.hidden) {
        timer = setTimeout(load, POLL_MS);
        return;
      }
      const cur = ref.current;
      const have = cur.data ? cur.base + cur.entries.length : null;
      try {
        const d = await api.get(`/instances/${id}/chat${have !== null ? `?after=${have}` : ''}`);
        if (!alive) return;
        // 前面被截掉的位置变了（对话很长）或网页手里的对不上：整份换掉
        const keep = cur.data && d.base === cur.base && d.from >= d.base ? cur.entries.slice(0, d.from - d.base) : null;
        if (keep === null && have !== null && d.from !== d.base) {
          ref.current = { ...cur, data: null };
          timer = setTimeout(load, 0);
          return;
        }
        setState({ data: d, entries: [...(keep ?? []), ...(d.entries ?? [])], base: d.base ?? 0, error: null });
      } catch (error) {
        if (alive) setState((s) => ({ ...s, error }));
      }
      if (alive) timer = setTimeout(load, POLL_MS);
    };
    load();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [id, active, tick]);
  return { ...state, refresh: () => setTick((n) => n + 1) };
}

/** 把连着的步骤合成一组；用户 / AI 的话各自一条。 */
function group(entries) {
  const out = [];
  for (const e of entries) {
    if (e.kind === 'step') {
      const last = out[out.length - 1];
      if (last?.kind === 'steps') last.steps.push(e);
      else out.push({ kind: 'steps', steps: [e] });
    } else out.push(e);
  }
  return out;
}

function fileList(files, t) {
  if (!files?.length) return '';
  const names = files.slice(0, 3).map((f) => f.split(/[\\/]/).pop());
  return files.length > 3 ? t('chat.files_more', { names: names.join(t('convo.sep')), n: files.length }) : names.join(t('convo.sep'));
}

/** 一步的说明：“查看了文件”“修改了 a.rs”… */
export function stepText(t, s) {
  const files = fileList(s.files, t);
  const text = t(`chat.action.${s.action}`, { files, tool: s.tool, defaultValue: t('chat.action.tool', { tool: s.tool }) });
  return s.status === 'stopped' ? t('chat.step_stopped', { text }) : text;
}

function StepIcon({ status }) {
  if (status === 'done') return <Check size={15} className="chat-ok" aria-hidden="true" />;
  if (status === 'error') return <X size={15} className="chat-err" aria-hidden="true" />;
  if (status === 'stopped') return <X size={15} className="muted" aria-hidden="true" />;
  return <Loader2 size={15} className="chat-spin" aria-hidden="true" />;
}

function Steps({ steps }) {
  const { t } = useTranslation();
  const done = steps.filter((s) => s.status === 'done').length;
  return (
    <details className="chat-steps">
      <summary>
        <ChevronRight size={15} className="chat-chev" aria-hidden="true" />
        <span className="grow">{steps.length === 1 ? stepText(t, steps[0]) : t('chat.steps', { n: steps.length })}</span>
        {steps.length > 1 && <span className="small muted">{t('chat.steps_done', { done, n: steps.length })}</span>}
      </summary>
      <ul>
        {steps.map((s, i) => (
          <li key={i}>
            <StepIcon status={s.status} />
            <span className="grow">
              {stepText(t, s)}
              {s.risk === 'high' && <span className="badge badge-danger chat-risk">{t('chat.risk.high')}</span>}
              {s.command && <code className="mono chat-cmd">{s.command}</code>}
            </span>
          </li>
        ))}
      </ul>
    </details>
  );
}

const RISK_BADGE = { low: 'badge-ok', medium: 'badge-warn', high: 'badge-danger', unknown: 'badge-warn' };

/** 等你同意 / 等你选一项的卡片。 */
function PromptCard({ id, prompt, onDone, onTerminal }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(null);
  const choose = async (choice, n) => {
    setBusy(`${choice}${n ?? ''}`);
    try {
      await api.post(`/instances/${id}/approve`, { choice, n, fingerprint: prompt.fingerprint });
      onDone();
    } catch (e) {
      toast(e?.code === 'STALE_PROMPT' ? t('chat.stale') : errorText(t, e), 'error');
      onDone();
    } finally {
      setBusy(null);
    }
  };
  if (prompt.kind === 'other') {
    return (
      <section className="chat-prompt" aria-live="polite">
        <b>{t('chat.choice_title')}</b>
        <p className="chat-q">{prompt.question}</p>
        <div className="chat-options">
          {prompt.options.map((o) => (
            <Button key={o.n} variant={o.n === 1 ? 'primary' : 'secondary'} busy={busy === `option${o.n}`} disabled={Boolean(busy)} onClick={() => choose('option', o.n)}>
              {o.label}
            </Button>
          ))}
        </div>
      </section>
    );
  }
  const yes = prompt.options[0]?.label?.startsWith('Yes');
  const session = prompt.options[1]?.label?.startsWith('Yes');
  const no = prompt.options.some((o) => o.label.startsWith('No'));
  const risk = prompt.risk ?? 'unknown';
  return (
    <section className={`chat-prompt chat-prompt-${risk}`} aria-live="polite">
      <b className="chat-prompt-title">
        {risk === 'high' ? <AlertTriangle size={18} aria-hidden="true" /> : <ShieldCheck size={18} aria-hidden="true" />}
        {t(`chat.ask.${prompt.kind}`)}
      </b>
      <p className="chat-q">{t(`chat.what.${prompt.action}`, { defaultValue: t('chat.what.run') })}</p>
      <span className={`badge ${RISK_BADGE[risk]}`}>{t(`chat.risk_long.${risk}`)}</span>
      {prompt.reason && <p className="small muted chat-reason">{t('chat.reason', { reason: prompt.reason })}</p>}
      <div className="chat-options">
        {yes && (
          <Button busy={busy === 'once'} disabled={Boolean(busy)} onClick={() => choose('once')}>
            {t('chat.allow')}
          </Button>
        )}
        {no && (
          <Button variant="secondary" busy={busy === 'deny'} disabled={Boolean(busy)} onClick={() => choose('deny')}>
            {t('chat.deny')}
          </Button>
        )}
        {session && risk !== 'high' && (
          <Button variant="ghost" busy={busy === 'session'} disabled={Boolean(busy)} onClick={() => choose('session')} title={prompt.options[1].label}>
            {t('chat.allow_session')}
          </Button>
        )}
      </div>
      {prompt.command && (
        <p className="small muted chat-reason">
          {t('chat.command')} <code className="mono">{prompt.command}</code>
        </p>
      )}
      <button type="button" className="link-btn small" onClick={onTerminal}>
        {t('chat.see_terminal')}
      </button>
    </section>
  );
}

function StateLine({ data }) {
  const { t } = useTranslation();
  if (!data?.running) return null;
  if (data.state === 'working')
    return (
      <p className="chat-state" role="status">
        <Loader2 size={15} className="chat-spin" aria-hidden="true" /> {t('chat.state.working')}
      </p>
    );
  if (data.state === 'unknown' && data.convo)
    return (
      <p className="chat-state" role="status">
        <Circle size={10} aria-hidden="true" /> {t('chat.state.unknown')}
      </p>
    );
  return null;
}

/**
 * `frame`：终端 iframe（回复走它，所以终端要一直挂着）；`onTerminal` / `onChanges`：切到另外两种看法。
 */
export default function ChatView({ id, inst, frame, active, touch, onTerminal }) {
  const { t } = useTranslation();
  const chat = useChat(id, active);
  const list = useRef(null);
  const groups = useMemo(() => group(chat.entries), [chat.entries]);
  const col = useRef(null);
  const stick = useRef(true);
  // 内容变高（新消息、Markdown 排好版、确认卡片出现）时：原来在底部就跟着滚到底
  useEffect(() => {
    const el = list.current;
    if (!el || !col.current || typeof ResizeObserver === 'undefined') return undefined;
    const ro = new ResizeObserver(() => {
      if (stick.current) el.scrollTop = el.scrollHeight;
    });
    ro.observe(col.current);
    return () => ro.disconnect();
  }, []);
  const onScroll = () => {
    const el = list.current;
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  };
  const d = chat.data;
  const chips = [
    { label: t('chat.chip.continue'), text: t('chat.chip.continue_text') },
    { label: t('chat.chip.simpler'), text: t('chat.chip.simpler_text') },
    { label: t('chat.chip.status'), text: t('chat.chip.status_text') },
  ];
  let body;
  if (d && !d.supported) {
    body = (
      <div className="chat-empty">
        <p>{t('chat.unsupported')}</p>
        <Button icon={SquareTerminal} onClick={onTerminal}>
          {t('chat.open_terminal')}
        </Button>
      </div>
    );
  } else if (!d && !chat.error) {
    body = <div className="chat-empty muted">{t('chat.loading')}</div>;
  } else {
    body = (
      <>
        {chat.error && !d && <p className="chat-empty">{errorText(t, chat.error)}</p>}
        {d && !d.convo && <div className="chat-empty muted">{t(d.running ? 'chat.no_convo' : 'chat.stopped')}</div>}
        {d?.base > 0 && <p className="small muted chat-skipped">{t('chat.skipped', { n: d.base })}</p>}
        {groups.map((g, i) =>
          g.kind === 'steps' ? (
            <Steps key={i} steps={g.steps} />
          ) : g.kind === 'user' ? (
            <div key={i} className="chat-user">
              {g.text}
            </div>
          ) : (
            <div key={i} className="chat-ai">
              <Markdown text={g.text} />
            </div>
          ),
        )}
        {d?.prompt && <PromptCard id={id} prompt={d.prompt} onDone={chat.refresh} onTerminal={onTerminal} />}
        {d?.state === 'unknown' && !d?.prompt && d?.running && !d?.convo && (
          <div className="chat-empty">
            <Button variant="secondary" icon={SquareTerminal} onClick={onTerminal}>
              {t('chat.open_terminal')}
            </Button>
          </div>
        )}
        <StateLine data={d} />
      </>
    );
  }
  return (
    <div className="chat-view">
      <div className="chat-scroll" ref={list} onScroll={onScroll}>
        <div className="chat-col" ref={col}>
          {body}
        </div>
      </div>
      {inst?.running && d?.supported !== false && (
        <div className="chat-compose">
          <div className="chips chat-chips">
            {chips.map((c) => (
              <button key={c.label} type="button" className="chip" onClick={() => sendToTerminal(frame, c.text, touch)}>
                {c.label}
              </button>
            ))}
            <button type="button" className="chip" onClick={() => termSend(frame, '\x1b', { focus: false })} title={t('chat.chip.pause_hint')}>
              {t('chat.chip.pause')}
            </button>
          </div>
          <Composer frame={frame} id={id} touch={touch} light placeholder={t('chat.placeholder')} />
        </div>
      )}
    </div>
  );
}
