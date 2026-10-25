// 设置在电脑之间同步（docs/SETTINGS_SYNC.md §7）：
// - SyncCard：设置页“通用”里的开关和各台电脑的同步情况；
// - SyncLabel / SyncMark：同步项旁边的小标记，提醒改了会影响别的电脑（关掉同步时不显示）。
import { ArrowLeftRight } from 'lucide-react';
import { useEffect } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { relativeTime } from './time.js';
import { Card, ErrorNote, Switch, useAction, useLoad } from './ui.jsx';

export function SyncMark() {
  const { t } = useTranslation();
  const { status } = useStatus();
  if (status.sync_enabled === false) return null;
  return (
    <span className="sync-mark" title={t('sync.mark')} aria-label={t('sync.mark')}>
      <ArrowLeftRight size={12} aria-hidden="true" />
    </span>
  );
}

export function SyncLabel({ text }) {
  return (
    <>
      {text} <SyncMark />
    </>
  );
}

function peerLine(t, p) {
  if (p.old) return t('sync.peer_old', { name: p.name });
  if (p.last) return t('sync.peer_synced', { name: p.name, when: relativeTime(t, new Date(p.last).toISOString()) });
  if (!p.online) return t('sync.peer_offline', { name: p.name });
  return t('sync.peer_pending', { name: p.name });
}

export function SyncCard() {
  const { t } = useTranslation();
  const { reload: reloadStatus } = useStatus();
  const [data, loadError, reload] = useLoad(() => api.get('/sync'));
  const { busy, error, run } = useAction();

  useEffect(() => {
    const iv = setInterval(reload, 30000);
    return () => clearInterval(iv);
  }, []);

  const on = data?.enabled !== false;
  const peers = data?.peers ?? [];
  const synced = peers.filter((p) => p.last).length;
  const set = async (v) => {
    if (await run(() => api.put('/sync', { enabled: v }))) {
      reload();
      reloadStatus();
    }
  };

  let state;
  if (!on) state = t('sync.state_off');
  else if (data?.local_only) state = t('sync.state_local_only');
  else if (peers.length === 0) state = t('sync.state_alone');
  else if (synced === 0) state = t('sync.state_waiting');
  else state = t('sync.state_synced', { count: synced + 1 });

  return (
    <Card title={t('sync.title')}>
      <div className="stack" style={{ gap: 10 }}>
        <div className="between" style={{ flexWrap: 'nowrap', gap: 12 }}>
          <span>
            <b>{t('sync.toggle')}</b>
            <span className="small muted" style={{ display: 'block' }}>
              {t('sync.lead')}
            </span>
          </span>
          <Switch checked={on} disabled={busy || !data} onChange={set} label={t('sync.toggle')} />
        </div>
        {data && (
          <p className="small muted" style={{ margin: 0 }} title={on && peers.length > 0 ? peers.map((p) => peerLine(t, p)).join('\n') : undefined}>
            {state}
          </p>
        )}
        <ErrorNote error={error ?? loadError} />
      </div>
    </Card>
  );
}
