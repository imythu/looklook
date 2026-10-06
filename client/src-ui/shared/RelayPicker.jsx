// 远程线路（在“设置”里）：默认自动选——这台电脑在后台定期测速，自动用延迟最低的一条；
// 列出每条线路测到的延迟，用户可以自己选一条，平台记住选择。
// RelayHint：后台测到当前线路偏慢、有快得多的线路时，在页面顶部给一条可关掉的提示，告诉用户去哪里换。
import { Gauge, RefreshCw, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { Link, useRouter } from './router.jsx';
import { Button, Card, ErrorNote, Note, Skeleton, useAction, useLoad, useToast } from './ui.jsx';

export const RELAYS_ANCHOR = 'relays';
const HINT_KEY = 'looklook.relay-hint';
const HINT_SNOOZE_MS = 24 * 3600 * 1000;

const AUTO = '';

/** 所在地：“美国 · 加利福尼亚州” / “United States · California”。 */
export function placeName(loc, lang) {
  const pick = (n) => (n ? (String(lang).startsWith('zh') ? n.zh : n.en) || n.en || n.zh : '');
  return [pick(loc.country), pick(loc.region)].filter(Boolean).join(' · ');
}

function speed(ms) {
  if (ms == null) return null;
  return ms < 80 ? 'fast' : ms < 200 ? 'ok' : 'slow';
}

function Latency({ item }) {
  const { t } = useTranslation();
  const s = speed(item.latency_ms);
  if (!s) return <span className="muted small">{t(item.error === 'unsupported' ? 'relays.no_test' : 'relays.unreachable')}</span>;
  return (
    <span className={`latency latency-${s}`}>
      {t('relays.ms', { ms: item.latency_ms })} · {t(`relays.speed.${s}`)}
    </span>
  );
}

export default function RelayPicker() {
  const { t, i18n } = useTranslation();
  const { status } = useStatus();
  const toast = useToast();
  const save = useAction();
  const ref = useRef(null);
  const [data, error, reload, loading] = useLoad(() => api.get('/relays'), []);
  // 从提示里的“去设置换线路”过来：滚到这张卡片
  useEffect(() => {
    if (window.location.hash === `#${RELAYS_ANCHOR}`) setTimeout(() => ref.current?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 50);
  }, []);
  if (!status.relay || status.relay.state === 'off') return null;

  const selected = data ? (data.auto ? AUTO : data.preferred) : null;
  const fallback = !data?.auto && data?.preferred && data.current && data.preferred !== data.current;

  async function choose(name) {
    if (name === selected || save.busy) return;
    const r = await save.run(() => api.put('/relays/preference', { relay: name || null }));
    if (r) {
      toast(r.current && r.current !== data.current ? t('relays.switched', { name: r.current }) : t('relays.saved'));
      reload();
    }
  }

  return (
    <div ref={ref} id={RELAYS_ANCHOR} className="anchor">
    <Card
      title={t('relays.title')}
      actions={
        <Button variant="ghost" size="sm" icon={RefreshCw} busy={loading && Boolean(data)} onClick={reload}>
          {t('relays.retest')}
        </Button>
      }
    >
      <p className="small">{t('relays.intro')}</p>
      <p className="muted small">{t('relays.when_slow')}</p>
      {fallback && <Note kind="warn">{t('relays.fallback', { preferred: data.preferred, current: data.current })}</Note>}
      <ErrorNote error={error || save.error} />
      {!data && loading && (
        <>
          <p className="muted small">{t('relays.measuring')}</p>
          <Skeleton height={48} />
        </>
      )}
      {data && (
        <div className="relay-list" role="radiogroup" aria-label={t('relays.title')}>
          <label className={`relay-option ${selected === AUTO ? 'selected' : ''}`}>
            <input type="radio" name="relay" checked={selected === AUTO} disabled={save.busy} onChange={() => choose(AUTO)} />
            <span className="grow">
              <b>{t('relays.auto')}</b>
              <span className="muted small block">{t('relays.auto_hint')}</span>
            </span>
            {selected === AUTO && data.current && <span className="muted small">{t('relays.auto_now', { name: data.current })}</span>}
          </label>
          {data.items.map((i) => (
            <label key={i.name} className={`relay-option ${selected === i.name ? 'selected' : ''}`}>
              <input type="radio" name="relay" checked={selected === i.name} disabled={save.busy} onChange={() => choose(i.name)} />
              <span className="grow">
                <b>{t('relays.line', { name: i.name })}</b>
                {i.location && <span className="relay-where">{placeName(i.location, i18n.language)}</span>}
                {data.current === i.name && <span className="tag tag-ok">{t('relays.in_use')}</span>}
                {data.best === i.name && data.items.length > 1 && <span className="tag">{t('relays.fastest')}</span>}
                {i.address && <span className="mono small muted block">{i.address}</span>}
              </span>
              {loading ? <span className="muted small">{t('relays.measuring_short')}</span> : <Latency item={i} />}
            </label>
          ))}
          {data.items.length === 0 && <p className="muted small">{t('relays.none')}</p>}
        </div>
      )}
      <p className="muted small">{t('relays.latency_explain')}</p>
      <p className="muted small">{t('relays.switch_note')}</p>
    </Card>
    </div>
  );
}

function hintId(c) {
  return `${c.current}>${c.suggest}`;
}

function snoozed(c) {
  try {
    const v = JSON.parse(localStorage.getItem(HINT_KEY) || 'null');
    return v?.id === hintId(c) && Date.now() - v.at < HINT_SNOOZE_MS;
  } catch {
    return false;
  }
}

/** 线路偏慢的提示（页面顶部一条，可关掉；关掉后同样的情况 24 小时内不再提示）。在“设置”页不显示。 */
export function RelayHint() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const { path } = useRouter();
  const [, setTick] = useState(0);
  const c = status.relay_check;
  if (!c?.suggest || path === '/settings' || status.relay?.state !== 'connected' || snoozed(c)) return null;
  const best = c.items?.find((i) => i.name === c.suggest);
  const dismiss = () => {
    try {
      localStorage.setItem(HINT_KEY, JSON.stringify({ id: hintId(c), at: Date.now() }));
    } catch {
      /* 存不了就只是这次关掉 */
    }
    setTick((n) => n + 1);
  };
  const now = c.current_ms == null ? t('relays.hint_now_down', { name: c.current }) : t('relays.hint_now', { name: c.current, ms: c.current_ms });
  return (
    <div className="banner banner-info relay-hint" role="status">
      <Gauge size={18} aria-hidden="true" style={{ flex: 'none', marginTop: 2 }} />
      <span className="grow">
        {now}
        {t('relays.hint_better', { name: c.suggest, ms: best?.latency_ms })}
        {c.auto && <span className="muted"> {t('relays.hint_auto')}</span>}
      </span>
      <Link to={`/settings#${RELAYS_ANCHOR}`} className="btn btn-ghost">
        {t('relays.hint_go')}
      </Link>
      <button type="button" className="btn btn-plain btn-icon banner-x" onClick={dismiss} aria-label={t('relays.hint_dismiss')} title={t('relays.hint_dismiss')}>
        <X size={18} />
      </button>
    </div>
  );
}
