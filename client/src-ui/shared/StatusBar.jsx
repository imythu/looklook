// 宽屏底部的状态栏：左边连接（本机 / 远程和延迟），右边等待回复、运行中的会话数，
// 这台电脑上登录了的 Claude、Codex 账号的 5 小时用量（点开看回满时间和建议用哪个）。手机上不显示。
// 用量的取数和列表（useAccountsUsage、UsageRows）手机上也用：侧栏底部、新建会话、对话页圆环点开的面板。

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useSessionUi } from '../pages/Sessions.jsx';
import { api } from './api.js';
import { LaunchIcon } from './brands.jsx';
import { openPalette } from './Palette.jsx';
import { useRouter } from './router.jsx';
import { ConnChip, usePop } from './RelayStatus.jsx';
import { sessionState, useSessions } from './sessions.jsx';
import { spanText } from './time.js';

const USAGE_POLL_MS = 60000;
const AGENTS = [
  ['claude', 'Claude'],
  ['codex', 'Codex'],
];

/** 剩余百分比和颜色档：≥50 充足，20–50 省着用，<20 快没了 */
export function remainingOf(win) {
  const used = Number(win?.used);
  if (!Number.isFinite(used)) return null;
  const left = Math.max(0, Math.min(100, Math.round(100 - used)));
  return { left, level: left >= 50 ? 'ok' : left >= 20 ? 'warn' : 'low' };
}

// 几处同时显示时只轮询一份：有人在看才轮询，最后一个走了就停
const shared = { data: null, subs: new Set(), timer: null, at: 0 };
async function pollUsage() {
  if (document.hidden) return;
  try {
    shared.data = await api.get('/usage/accounts');
    shared.at = Date.now();
    shared.subs.forEach((f) => f(shared.data));
  } catch {
    // 旧版客户端没有这个接口、暂时连不上：不显示，下次再试
  }
}

/** 这台电脑登录的 Claude、Codex 账号的用量：`{ claude, codex }`，各自 `{ five_hour: { used, resets_at }, … }` 或 null */
export function useAccountsUsage() {
  const [data, setData] = useState(shared.data);
  useEffect(() => {
    shared.subs.add(setData);
    if (shared.subs.size === 1) {
      shared.timer = setInterval(pollUsage, USAGE_POLL_MS);
      window.addEventListener('focus', pollUsage);
    }
    if (Date.now() - shared.at > USAGE_POLL_MS / 2) pollUsage();
    return () => {
      shared.subs.delete(setData);
      if (shared.subs.size === 0) {
        clearInterval(shared.timer);
        window.removeEventListener('focus', pollUsage);
      }
    };
  }, []);
  return data;
}

/** 有 5 小时用量的账号：`[{ key, name, d, r }]`，r 见 remainingOf */
export function usageRows(data) {
  return AGENTS.map(([key, name]) => ({ key, name, d: data?.[key], r: remainingOf(data?.[key]?.five_hour) })).filter((x) => x.r);
}

/** 启动方式对应哪个账号的用量（claude / codex），别的（普通终端、OpenCode……）没有 */
export function usageAgent(launch) {
  const l = String(launch ?? '');
  return l.startsWith('claude') ? 'claude' : l.startsWith('codex') ? 'codex' : null;
}

export function Bar({ left, level, big = false }) {
  return (
    <span className={`sbar-meter ${big ? 'big' : ''}`} data-level={level} aria-hidden="true">
      <span style={{ width: `${left}%` }} />
    </span>
  );
}

/** 每个账号一行：名字、剩多少、长条、什么时候回满；两个都有且差得多时建议用哪个 */
export function UsageRows({ rows, tip = true }) {
  const { t } = useTranslation();
  // 两个都有时建议剩得多的那个接大任务
  const best = rows.length > 1 ? [...rows].sort((a, b) => b.r.left - a.r.left)[0] : null;
  const other = best && rows.find((x) => x !== best);
  return (
    <>
      {rows.map((x) => {
        const resets = Date.parse(x.d.five_hour.resets_at) - Date.now();
        const plan = x.d.plan ?? x.d.account?.plan;
        return (
          <div key={x.key} className="sbar-pop-row" data-level={x.r.level}>
            <div className="sbar-pop-name">
              <LaunchIcon launch={x.key} size={14} />
              <span>{x.name}</span>
              {plan && <span className="muted small">{String(plan)}</span>}
              <span className="grow" />
              <b className="sbar-pop-left">{t('sbar.left', { n: x.r.left })}</b>
            </div>
            <Bar {...x.r} big />
            <span className="muted small">
              {[t(`sbar.level_${x.r.level}`), resets > 0 ? t('sbar.resets', { in: spanText(t, resets) }) : null].filter(Boolean).join(' · ')}
            </span>
          </div>
        );
      })}
      {tip && best && best.r.left - other.r.left >= 15 && <div className="sbar-pop-tip">{t('sbar.suggest', { best: best.name, other: other.name })}</div>}
    </>
  );
}

function UsageGroup({ data }) {
  const { t } = useTranslation();
  const { open, setOpen, ref } = usePop();
  const rows = usageRows(data);
  if (!rows.length) return null;
  const title = rows.map((x) => t('sbar.usage_line', { name: x.name, n: x.r.left })).join('，');
  return (
    <span className="sbar-usage-wrap" ref={ref}>
      <button type="button" className="sbar-item sbar-usage" onClick={() => setOpen(!open)} aria-expanded={open} aria-haspopup="dialog" title={title} aria-label={title}>
        {rows.map((x) => (
          <span key={x.key} className="sbar-agent" data-level={x.r.level}>
            <LaunchIcon launch={x.key} size={11} />
            <Bar {...x.r} />
            <span className="sbar-pct">{x.r.left}%</span>
          </span>
        ))}
      </button>
      {open && (
        <div className="sbar-pop" role="dialog" aria-label={t('sbar.usage_title')}>
          <div className="sbar-pop-head">
            <b>{t('sbar.usage_title')}</b>
            <span className="muted small">{t('sbar.usage_scope')}</span>
          </div>
          <UsageRows rows={rows} />
        </div>
      )}
    </span>
  );
}

/** 正在显示的会话面板告诉状态栏用哪套颜色（`--term-*`，见 TerminalView 的 TermPane） */
export const setStatusChrome = (vars) => window.dispatchEvent(new CustomEvent('ll-chrome', { detail: vars }));

export default function StatusBar() {
  const { t } = useTranslation();
  // 和会话顶栏一样跟着当前标签换颜色：会话页里由正在显示的面板通知，别的页面用管理台配色
  const { path } = useRouter();
  const [paneChrome, setPaneChrome] = useState(null);
  useEffect(() => {
    const on = (e) => setPaneChrome(e.detail ?? null);
    window.addEventListener('ll-chrome', on);
    return () => window.removeEventListener('ll-chrome', on);
  }, []);
  const chrome = path.startsWith('/s/') ? paneChrome ?? undefined : undefined;
  const s = useSessions();
  const ui = useSessionUi();
  const usage = useAccountsUsage();
  const items = s?.items ?? [];
  const waiting = items.filter((i) => sessionState(i) === 'attn');
  const running = items.filter((i) => sessionState(i) === 'working').length;
  return (
    <footer className="sbar" aria-label={t('sbar.label')} style={chrome}>
      {/* 本机打开、没开远程通道时 ConnChip 什么都不显示，这里写“本机” */}
      <span className="sbar-conn" data-label={t('conn.local')}>
        <ConnChip />
      </span>
      <span className="grow" />
      {waiting.length > 0 && (
        <button type="button" className="sbar-item sbar-wait" onClick={() => ui.openSession(waiting[0].id)} title={t('sbar.waiting_hint')}>
          {t('sbar.waiting', { n: waiting.length })}
        </button>
      )}
      {running > 0 && <span className="sbar-item">{t('sbar.running', { n: running })}</span>}
      <UsageGroup data={usage} />
      <button type="button" className="sbar-item sbar-cmd" onClick={openPalette} title={t('pal.title')}>
        <kbd className="pal-kbd">Ctrl K</kbd>
      </button>
    </footer>
  );
}

/** 手机侧栏底部：这台电脑登录的账号的 5 小时额度（宽屏看底部状态栏）。侧栏打开时才挂上，不打开不轮询 */
export function SideUsage() {
  const { t } = useTranslation();
  const rows = usageRows(useAccountsUsage());
  if (!rows.length) return null;
  return (
    <section className="side-usage" aria-label={t('sbar.usage_title')}>
      <div className="side-usage-head muted small">{t('sbar.usage_title')}</div>
      <UsageRows rows={rows} tip={false} />
    </section>
  );
}
