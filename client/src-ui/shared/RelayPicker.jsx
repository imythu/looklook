// 远程线路：默认自动选；列出每条线路从这台电脑测到的延迟，用户可以自己选一条，平台记住选择。
import { RefreshCw } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { Button, Card, ErrorNote, Note, Skeleton, useAction, useLoad, useToast } from './ui.jsx';

const AUTO = '';

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
  const { t } = useTranslation();
  const { status } = useStatus();
  const toast = useToast();
  const save = useAction();
  const [data, error, reload, loading] = useLoad(() => api.get('/relays'), []);
  if (!status.relay || status.relay.state === 'off') return null;

  const selected = data ? data.preferred ?? AUTO : null;
  const fallback = data?.preferred && data.current && data.preferred !== data.current;

  async function choose(name) {
    if (name === selected || save.busy) return;
    const r = await save.run(() => api.put('/relays/preference', { relay: name || null }));
    if (r) {
      toast(r.current && r.current !== data.current ? t('relays.switched', { name: r.current }) : t('relays.saved'));
      reload();
    }
  }

  return (
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
                {data.current === i.name && <span className="tag tag-ok">{t('relays.in_use')}</span>}
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
  );
}
