// 远程访问的引导：本机、局域网不登录也能用，在外面打开要看看账户（Beta 期间新用户注册免费一年），到期了要续费。
import { Check, Globe } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { useStatus, webUrl } from '../App.jsx';
import { Link } from './router.jsx';
import { daysLeft } from './time.js';

const POINTS = ['anywhere', 'network', 'machines', 'pages'];

/** 远程能不能用：远程打开时状态来自那台电脑；旧版本的电脑没有 `remote_allowed`，那时 `allowed` 就是它。 */
export function remoteAllowed(status) {
  return status.account.remote_allowed ?? status.account.allowed;
}

/** 远程访问能做什么（几条带勾的要点）。 */
export function RemotePoints() {
  const { t } = useTranslation();
  return (
    <ul className="upsell-points">
      {POINTS.map((k) => (
        <li key={k}>
          <Check size={15} aria-hidden="true" />
          <span>{t(`upsell.points.${k}`)}</span>
        </li>
      ))}
    </ul>
  );
}

/** 没登录时：讲清楚远程能做什么，引导注册、登录。`compact`：只留一句话和按钮（放在别的卡片里）。 */
export function RemoteUpsell({ compact = false }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  return (
    <div className={`upsell ${compact ? 'upsell-compact' : ''}`}>
      <div className="upsell-head">
        <Globe size={18} aria-hidden="true" />
        <b className="grow">{t('upsell.title')}</b>
        <span className="badge badge-ok">{t('upsell.offer')}</span>
      </div>
      <p className="muted small">{t('upsell.body')}</p>
      {!compact && <RemotePoints />}
      {!compact && <p className="small">{t('upsell.price')}</p>}
      <div className="upsell-actions">
        <Link to="/account" className="btn btn-primary">
          {t('upsell.cta')}
        </Link>
        {!compact && (
          <a className="btn btn-plain" href={webUrl(status, '/#pricing')} target="_blank" rel="noopener noreferrer">
            {t('upsell.compare')}
          </a>
        )}
      </div>
    </div>
  );
}

/** 登录了但远程暂停（会员到期等）：说明本机不受影响，引导续费。 */
export function RemotePaused() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const reason = status.account.reason;
  return (
    <div className="upsell upsell-paused">
      <div className="upsell-head">
        <Globe size={18} aria-hidden="true" />
        <b className="grow">{t(`upsell.paused.${reason}`, { defaultValue: t('upsell.paused.other') })}</b>
      </div>
      <p className="muted small">{t('upsell.paused_body')}</p>
      {reason === 'MEMBERSHIP_EXPIRED' && <p className="muted small">{t('upsell.paid_soon')}</p>}
      {reason === 'MEMBERSHIP_EXPIRED' && (
        <div className="upsell-actions">
          <Link to="/membership" className="btn btn-primary">
            {t('gate.renew')}
          </Link>
        </div>
      )}
    </div>
  );
}

/** 远程访问的状态：没登录 → 引导开通；暂停 → 引导续费；能用 → null（由调用方显示正常内容）。 */
export function RemoteGate({ compact = false }) {
  const { status } = useStatus();
  if (!status.account.logged_in) return <RemoteUpsell compact={compact} />;
  if (!remoteAllowed(status)) return <RemotePaused />;
  return null;
}

/** 会员快到期（14 天内）：提醒续费，免得哪天在外面突然打不开。 */
export function renewSoon(status) {
  const left = daysLeft(status.account.session?.membership_expires_at);
  return status.account.logged_in && remoteAllowed(status) && left !== null && left <= 14 ? left : null;
}
