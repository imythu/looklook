// 账户：显示名称、专属地址、可以使用到；续费与账户管理在看看网页上完成。
import { CalendarClock, ExternalLink, LogOut } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { Initial, useLogout, useStatus, webUrl } from '../App.jsx';
import { api } from '../shared/api.js';
import { daysLeft, formatDate } from '../shared/time.js';
import RelayStatus from '../shared/RelayStatus.jsx';
import { Button, Card, CopyButton, Time, useLoad } from '../shared/ui.jsx';

export default function Account() {
  const { t, i18n } = useTranslation();
  const { status } = useStatus();
  const logout = useLogout();
  // 刷新一次平台上的账户信息（昵称、到期时间可能在网页上改过）；连不上时显示本机缓存。
  useLoad(() => api.get('/account').catch(() => null), []);
  const s = status.account.session;
  const left = daysLeft(s.membership_expires_at);
  return (
    <>
      <div className="page-head">
        <h1>{t('account.title')}</h1>
      </div>
      <Card>
        <div className="profile">
          <Initial name={s.user.nickname} large />
          <span className="grow">
            <b style={{ fontSize: 18 }}>{s.user.nickname}</b>
            <span className="muted small" style={{ display: 'block' }}>
              {s.user.email}
            </span>
          </span>
        </div>
      </Card>
      <Card title={t('account.membership')}>
        <div className="between">
          <span>
            <span className="member-date">{formatDate(s.membership_expires_at, i18n.language)}</span>
            <span className={`small ${left > 30 ? 'muted' : 'warn-text'}`} style={{ display: 'block' }}>
              {left > 0 ? t('account.days_left', { n: left }) : t('account.expired')}
            </span>
          </span>
          <a className="btn btn-secondary" href={webUrl(status, '/membership')} target="_blank" rel="noopener noreferrer">
            <CalendarClock size={18} /> {t('account.renew')}
          </a>
        </div>
      </Card>
      <Card title={t('account.address')}>
        <p className="muted small">{t('account.address_hint')}</p>
        <div className="row">
          <input className="mono" readOnly value={s.user_host_url} onFocus={(e) => e.target.select()} />
          <CopyButton text={s.user_host_url} label={false} />
        </div>
      </Card>
      <Card title={t('account.connection')}>
        <RelayStatus />
        <dl className="kv">
          <dt>{t('account.username')}</dt>
          <dd className="mono">{s.user.username}</dd>
          <dt>{t('account.last_heartbeat')}</dt>
          <dd>
            <Time value={s.last_heartbeat_at} relative />
          </dd>
          <dt>{t('account.server')}</dt>
          <dd className="mono">{status.account.server}</dd>
        </dl>
      </Card>
      <div className="inline" style={{ marginBottom: 24 }}>
        <a className="btn btn-ghost" href={webUrl(status, '/account')} target="_blank" rel="noopener noreferrer">
          <ExternalLink size={18} /> {t('account.manage')}
        </a>
        <Button variant="danger" icon={LogOut} onClick={logout}>
          {t('nav.logout')}
        </Button>
      </div>
    </>
  );
}
