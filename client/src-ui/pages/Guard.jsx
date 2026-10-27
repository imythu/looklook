// 设置页“AI”分区的安全网（docs/PLAN_2026-10-27.md §3）：命令规则（AI 要运行的命令命中规则时要你确认，或者直接拒绝）
// 和每轮检查点的开关。所有电脑一份（同步集合 guard），这里只是编辑；判断在客户端（guard.rs、checkpoint.rs）。
import { X } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../shared/api.js';
import { Button, Card, ErrorNote, Switch, useAction, useLoad } from '../shared/ui.jsx';

export default function GuardCard() {
  const { t } = useTranslation();
  const [data, loadError, reload] = useLoad(() => api.get('/guard'));
  const { busy, error, run } = useAction();
  const [pattern, setPattern] = useState('');
  const [action, setAction] = useState('confirm');
  const g = data?.guard;
  const save = async (patch) => (await run(() => api.put('/guard', { ...g, ...patch }))) && (reload(), true);
  const add = async (e) => {
    e.preventDefault();
    if (pattern.trim() && (await save({ rules: [...g.rules, { pattern: pattern.trim(), action }] }))) setPattern('');
  };
  const toggle = (k, title, hint) => (
    <div className="between" style={{ flexWrap: 'nowrap', gap: 12 }}>
      <span>
        <b>{title}</b>
        <span className="small muted" style={{ display: 'block' }}>
          {hint}
        </span>
      </span>
      <Switch checked={g[k]} disabled={busy} onChange={(v) => save({ [k]: v })} label={title} />
    </div>
  );
  const actionSelect = (value, onChange, label) => (
    <select value={value} disabled={busy} onChange={(e) => onChange(e.target.value)} aria-label={label} style={{ width: 'auto', flexShrink: 0 }}>
      <option value="confirm">{t('guard.confirm')}</option>
      <option value="deny">{t('guard.deny')}</option>
    </select>
  );
  return (
    <Card title={t('guard.title')}>
      <ErrorNote error={loadError} />
      {g && (
        <div className="stack" style={{ gap: 14 }}>
          {toggle('checkpoints', t('guard.checkpoints'), t('guard.checkpoints_hint'))}
          <div className="nt-calm stack" style={{ gap: 14 }}>
            <b className="nt-calm-title">{t('guard.rules')}</b>
            <p className="small muted" style={{ margin: 0 }}>
              {t('guard.lead')}
            </p>
            {toggle('builtin', t('guard.builtin'), t('guard.builtin_hint'))}
            <div className="stack" style={{ gap: 8 }}>
              <b>{t('guard.mine')}</b>
              <span className="small muted">{t('guard.mine_hint')}</span>
              {g.rules.map((r, n) => (
                <div key={r.pattern} className="gd-rule">
                  <code className="ellipsis" title={r.pattern}>
                    {r.pattern}
                  </code>
                  {actionSelect(
                    r.action,
                    (v) =>
                      save({
                        rules: g.rules.map((x, k) => (k === n ? { ...x, action: v } : x)),
                      }),
                    r.pattern,
                  )}
                  <Button variant="ghost" icon={X} disabled={busy} aria-label={t('guard.remove', { pattern: r.pattern })} onClick={() => save({ rules: g.rules.filter((_, k) => k !== n) })} />
                </div>
              ))}
              <form className="gd-rule" onSubmit={add}>
                <input
                  className="mono"
                  value={pattern}
                  maxLength={200}
                  placeholder={t('guard.placeholder')}
                  aria-label={t('guard.pattern')}
                  onChange={(e) => setPattern(e.target.value)}
                  style={{ flex: 1, minWidth: 0 }}
                />
                {actionSelect(action, setAction, t('guard.action'))}
                <Button type="submit" variant="secondary" disabled={busy || !pattern.trim()}>
                  {t('guard.add')}
                </Button>
              </form>
            </div>
            <p className="small muted" style={{ margin: 0 }}>
              {t('guard.limits')}
            </p>
          </div>
          {data.sync_enabled && (
            <p className="small muted" style={{ margin: 0 }}>
              {t('guard.synced')}
            </p>
          )}
          <ErrorNote error={error} />
        </div>
      )}
    </Card>
  );
}
