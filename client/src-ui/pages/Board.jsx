// 任务看板（docs/TASK_BOARD.md §5，样机 A）：所有电脑上的 AI 会话按“等你处理 → 正在干活 → 刚结束”分组，
// 一行一句话说清现状。点一行展开（宽屏在右侧详情栏），能直接允许 / 拒绝、回答选项、处理选择框、停止、知道了。
// 数据就是会话列表（每 5 秒取一次，sessions.jsx），操作复用对话界面的接口（按终端自动发给它所在的电脑）。
import { AlertTriangle, Check, ChevronRight, ExternalLink, Inbox, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api, terminalDevice } from '../shared/api.js';
import { boardSort, boardSpan, boardState, boardSummary, BOARD_GROUPS } from '../shared/board.js';
import { DeviceTag, LaunchIcon } from '../shared/brands.jsx';
import { useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { baseName, useSessions } from '../shared/sessions.jsx';
import { spinnerText } from '../shared/spinner.js';
import { spanText } from '../shared/time.js';
import { Link } from '../shared/router.jsx';
import { Button, Empty, Note, Skeleton, useConfirm, useToast } from '../shared/ui.jsx';
import { useSessionUi } from './Sessions.jsx';
import { useNarrow } from './TerminalView.jsx';

const AGENT_NAMES = { claude: 'Claude', codex: 'Codex' };
// 额度菜单里“等重置后自动继续”（Claude Code 的原文，同客户端 screen.rs 的 LIMIT_AUTO）
const LIMIT_AUTO = 'Wait here, then continue automatically';

/** 飞书通知配好了但这台电脑没连上（docs/TASK_BOARD.md §6.6）：看板顶部提示一行。旧版客户端没有这个接口时不提示。 */
function useFeishuDown() {
  const [down, setDown] = useState(false);
  useEffect(() => {
    let alive = true;
    const load = () =>
      api.get('/notify').then(
        (d) => alive && setDown(Boolean(d.feishu?.bound && !d.status?.connected && d.status?.error)),
        () => alive && setDown(false),
      );
    load();
    const id = setInterval(load, 60000);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, []);
  return down;
}

/** 每 30 秒重画一次（“等了 3 分钟”） */
function useNow() {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 30000);
    return () => clearInterval(id);
  }, []);
  return now;
}

function useActivityText() {
  const { t, i18n } = useTranslation();
  return (activity, tool) => {
    const s = (activity ?? '').trim();
    if (!s) return '';
    if (s === 'compacting') return t('chat.compacting');
    if (s === 'thinking') return t('chat.codex_thinking');
    if (s === 'waiting_agents') return t('chat.codex_waiting_agents');
    if (tool === 'Bash') return t('board.do_run', { detail: s });
    return spinnerText(s, i18n.language).text;
  };
}

/** 会话所在的电脑（只有一台时不显示） */
function useDeviceOf() {
  const devices = useDevices();
  return (i) => (devices.multi ? devices.byId?.(terminalDevice(i.id)) ?? null : null);
}

function StateIcon({ state, outcome }) {
  if (state === 'attn') return <span className="bd-dot bd-dot-attn" aria-hidden="true" />;
  if (state === 'working') return <span className="bd-dot bd-dot-working" aria-hidden="true" />;
  if (outcome === 'failed')
    return (
      <span className="bd-end bd-end-fail" aria-hidden="true">
        <X size={14} strokeWidth={2.6} />
      </span>
    );
  return (
    <span className="bd-end bd-end-ok" aria-hidden="true">
      <Check size={14} strokeWidth={2.6} />
    </span>
  );
}

function Row({ i, state, selected, onSelect, now, act }) {
  const { t } = useTranslation();
  const deviceOf = useDeviceOf();
  const dev = deviceOf(i);
  const c = i.chat ?? {};
  const sum = boardSummary(t, i, act);
  const span = boardSpan(i, now);
  const title = c.title || i.name || baseName(i.workdir);
  const risk = state === 'attn' && c.attn?.risk === 'high';
  return (
    <button type="button" className={`bd-row bd-${state} ${selected ? 'selected' : ''}`} onClick={onSelect} aria-expanded={selected}>
      <StateIcon state={state} outcome={c.turn?.outcome} />
      <LaunchIcon launch={i.launch} size={18} className="bd-agent" />
      <span className="bd-text">
        <span className="bd-title-line">
          <b className="ellipsis">{title}</b>
          {span != null && <span className="bd-span">{t(`board.span_${state}`, { span: spanText(t, span) })}</span>}
        </span>
        <span className={`bd-sum ${sum.code ? 'mono' : ''}`} data-kind={sum.kind}>
          {risk && <span className="bd-risk">{t('board.risk_high')}</span>}
          <span className="ellipsis">{sum.text}</span>
        </span>
        <span className="bd-meta">
          <span className="ellipsis">{baseName(i.workdir) || i.workdir}</span>
          {dev && <DeviceTag device={dev} />}
        </span>
      </span>
      <ChevronRight className="bd-chev" size={16} aria-hidden="true" />
    </button>
  );
}

/** 展开后的内容：按状态给能直接做的事 */
function Peek({ i, state, onOpen }) {
  const { t } = useTranslation();
  const toast = useToast();
  const confirm = useConfirm();
  const [busy, setBusy] = useState('');
  const [other, setOther] = useState('');
  const c = i.chat ?? {};
  const a = c.attn ?? {};
  const agent = AGENT_NAMES[c.agent ?? i.launch] ?? i.launch;
  const run = async (key, fn) => {
    setBusy(key);
    try {
      await fn();
    } catch (e) {
      // 已经在别处处理过了：等下一次刷新自然消失
      if (e.code !== 'CHAT_STALE') toast(errorText(t, e), 'error');
    } finally {
      setBusy('');
    }
  };
  const answer = (decision, extra = {}) => run(decision, () => api.post(`/instances/${i.id}/chat/permission`, { id: a.pending_id, decision, ...extra }));
  const allow = async () => {
    if (a.risk === 'high' && !(await confirm({ title: t('board.confirm_high_title'), body: a.detail, confirm: t('board.allow'), danger: true }))) return;
    answer('allow');
  };
  const reply = (label) => answer('answer', { answers: { [a.question ?? a.detail]: label } });
  const choose = (index) => run(`d${index}`, () => api.post(`/instances/${i.id}/chat/dialog`, { index, title: a.title ?? '', label: a.options[index] }));
  const seen = () => run('seen', () => api.post(`/instances/${i.id}/chat/seen`, {}));
  const stop = async () => {
    if (!(await confirm({ title: t('board.stop_title', { agent }), body: t('board.stop_body'), confirm: t('board.stop'), danger: true }))) return;
    run('stop', () => api.post(`/instances/${i.id}/chat/interrupt`));
  };
  const open = (
    <Button variant="ghost" icon={ExternalLink} onClick={onOpen}>
      {t('board.open')}
    </Button>
  );

  let body = null;
  let actions = open;
  if (state === 'attn' && a.kind === 'permission') {
    body = (
      <>
        <p className="bd-peek-lead">{t('board.wants', { agent, tool: a.tool })}</p>
        {a.detail && <pre className="bd-code">{a.detail}</pre>}
        {a.risk === 'high' && (
          <p className="bd-warn">
            <AlertTriangle size={16} aria-hidden="true" />
            {t('board.risk_high_note')}
          </p>
        )}
      </>
    );
    actions = (
      <>
        <Button busy={busy === 'allow'} disabled={Boolean(busy) || a.pending_id == null} onClick={allow}>
          {t('board.allow')}
        </Button>
        <Button variant="ghost" className="bd-deny" busy={busy === 'deny'} disabled={Boolean(busy) || a.pending_id == null} onClick={() => answer('deny')}>
          {t('board.deny')}
        </Button>
        {open}
      </>
    );
  } else if (state === 'attn' && a.kind === 'question') {
    body = (
      <>
        <p className="bd-peek-lead">{a.detail}</p>
        {a.options?.length > 0 && (
          <div className="bd-options">
            {a.options.map((o) => (
              <button key={o} type="button" className="bd-option" disabled={Boolean(busy)} onClick={() => reply(o)}>
                {o}
              </button>
            ))}
          </div>
        )}
        {a.options?.length > 0 && (
          <form
            className="bd-other"
            onSubmit={(e) => {
              e.preventDefault();
              if (other.trim()) reply(other.trim());
            }}
          >
            <input value={other} onChange={(e) => setOther(e.target.value)} placeholder={t('board.other')} aria-label={t('board.other')} />
            <Button variant="ghost" type="submit" disabled={!other.trim() || Boolean(busy)}>
              {t('board.send')}
            </Button>
          </form>
        )}
      </>
    );
  } else if (state === 'attn' && a.kind === 'limit') {
    const auto = (a.options ?? []).findIndex((o) => o.startsWith(LIMIT_AUTO));
    body = <p className="bd-peek-lead">{c.limit?.waiting ? t('board.limit_waiting', { at: c.limit.waiting }) : t('board.limit_body')}</p>;
    actions = (
      <>
        {auto >= 0 && (
          <Button busy={busy === `d${auto}`} disabled={Boolean(busy)} onClick={() => choose(auto)}>
            {t('board.limit_wait')}
          </Button>
        )}
        <Button variant="ghost" onClick={onOpen}>
          {t('board.limit_switch')}
        </Button>
      </>
    );
  } else if (state === 'attn' && a.kind === 'dialog') {
    const opts = a.options ?? [];
    body = (
      <>
        <p className="bd-peek-lead">{a.title || t('board.dialog_unknown')}</p>
        {opts.length > 0 && opts.length <= 4 && (
          <div className="bd-options">
            {opts.map((o, n) => (
              <button key={o} type="button" className="bd-option" disabled={Boolean(busy)} onClick={() => choose(n)}>
                {o}
              </button>
            ))}
          </div>
        )}
      </>
    );
  } else if (state === 'attn') {
    body = <p className="bd-peek-lead">{a.detail ? t('board.plan', { p: a.detail }) : t('board.dialog_unknown')}</p>;
  } else if (state === 'working') {
    actions = (
      <>
        {open}
        <Button variant="ghost" className="bd-deny" busy={busy === 'stop'} disabled={Boolean(busy)} onClick={stop}>
          {t('board.stop')}
        </Button>
      </>
    );
  } else if (state === 'ended') {
    const turn = c.turn ?? {};
    body = turn.outcome === 'failed' ? <p className="bd-peek-lead bd-error">{turn.error || t('board.failed', { error: '' })}</p> : turn.summary ? <p className="bd-peek-lead">{turn.summary}</p> : null;
    actions = (
      <>
        {open}
        <Button variant="ghost" busy={busy === 'seen'} disabled={Boolean(busy)} onClick={seen}>
          {t('board.seen')}
        </Button>
      </>
    );
  }
  return (
    <div className="bd-peek">
      {body}
      <div className="bd-actions">{actions}</div>
    </div>
  );
}

export default function Board() {
  const { t } = useTranslation();
  const sessions = useSessions();
  const devices = useDevices();
  const ui = useSessionUi();
  const narrow = useNarrow('(max-width: 1099px)');
  const now = useNow();
  const act = useActivityText();
  const [pick, setPick] = useState(null);
  const [showRest, setShowRest] = useState(false);
  const feishuDown = useFeishuDown();

  const groups = useMemo(() => {
    const g = { attn: [], working: [], ended: [], idle: [], stopped: [] };
    for (const i of sessions.items) g[boardState(i)]?.push(i);
    for (const k of BOARD_GROUPS) g[k].sort(boardSort(k));
    return g;
  }, [sessions.items]);
  const offline = devices.multi ? devices.items.filter((d) => !d.online).length : 0;
  // 默认选中等得最久的那个；选中的不在了（处理掉了）也换成它
  const all = BOARD_GROUPS.flatMap((k) => groups[k]);
  const selected = all.find((i) => i.id === pick) ?? (narrow ? null : groups.attn[0] ?? all[0] ?? null);
  const selState = selected ? boardState(selected) : null;
  const openSession = (id) => ui.openSession(id);

  if (sessions.loading) {
    return (
      <div className="board stack">
        <Skeleton height={60} />
        <Skeleton height={180} />
      </div>
    );
  }
  const empty = all.length === 0;
  const counts = t('board.counts', { attn: groups.attn.length, working: groups.working.length });
  return (
    <div className="board">
      <header className="bd-head">
        <h1>{t('board.title')}</h1>
        {!empty && <span className="muted small">{counts}</span>}
      </header>
      {feishuDown && (
        <div className="bd-notify">
          <Note kind="warn">
            {t('board.feishu_down')} <Link to="/settings#notify">{t('board.feishu_down_fix')}</Link>
          </Note>
        </div>
      )}
      <div className={`bd-body ${!narrow && selected ? 'with-side' : ''}`}>
        <div className="bd-list">
          {empty && (
            <Empty icon={Inbox} title={t('board.empty_title')}>
              {t('board.empty_body')}
            </Empty>
          )}
          {BOARD_GROUPS.map(
            (k) =>
              groups[k].length > 0 && (
                <section key={k} className={`bd-group bd-group-${k}`}>
                  <h2>
                    {t(`board.group_${k}`)} <span>{groups[k].length}</span>
                  </h2>
                  {groups[k].map((i) => {
                    const on = selected?.id === i.id;
                    return (
                      <div key={i.id} className={`bd-item ${on && narrow ? 'open' : ''}`}>
                        <Row i={i} state={k} selected={on} now={now} act={act} onSelect={() => setPick(on && narrow ? '' : i.id)} />
                        {on && narrow && <Peek i={i} state={k} onOpen={() => openSession(i.id)} />}
                      </div>
                    );
                  })}
                </section>
              ),
          )}
          {(groups.idle.length > 0 || groups.stopped.length > 0 || offline > 0) && (
            <div className="bd-rest">
              <button type="button" className="bd-chip" onClick={() => setShowRest(!showRest)} aria-expanded={showRest}>
                {[groups.idle.length > 0 && t('board.rest_idle', { n: groups.idle.length }), groups.stopped.length > 0 && t('board.rest_stopped', { n: groups.stopped.length }), offline > 0 && t('board.rest_offline', { n: offline })].filter(Boolean).join(' · ')}
              </button>
              {showRest && (
                <div className="bd-rest-list">
                  {[...groups.idle, ...groups.stopped].map((i) => (
                    <button key={i.id} type="button" className="bd-rest-row" onClick={() => openSession(i.id)}>
                      <LaunchIcon launch={i.launch} size={16} />
                      <span className="ellipsis">{i.chat?.title || i.name || baseName(i.workdir)}</span>
                      <span className="muted small">{t(boardState(i) === 'idle' ? 'board.idle' : 'board.stopped')}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          )}
        </div>
        {!narrow && selected && (
          <aside className="bd-side" aria-label={t('board.detail')}>
            <div className="bd-side-head">
              <LaunchIcon launch={selected.launch} size={24} />
              <span className="grow">
                <b className="ellipsis">{selected.chat?.title || selected.name || baseName(selected.workdir)}</b>
                <span className="muted small ellipsis">{selected.workdir}</span>
              </span>
            </div>
            <Peek key={selected.id} i={selected} state={selState} onOpen={() => openSession(selected.id)} />
          </aside>
        )}
      </div>
    </div>
  );
}

/** 等你处理的会话个数（侧栏、顶栏的数字，标签页标题） */
export function useAttnCount() {
  const sessions = useSessions();
  return sessions ? sessions.items.filter((i) => boardState(i) === 'attn').length : 0;
}
