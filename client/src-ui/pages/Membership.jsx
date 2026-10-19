// 会员与邀请：到期时间、兑换续期码、会员记录、邀请码与已加入的朋友。数据来自看看平台（shared/platform.js）。
// 没有在线支付：续期码由官方发放，在这里兑换。
import { CalendarClock, CheckCircle2, CloudOff, Gift, History, Plus, Ticket, UserPlus } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus, webUrl } from '../App.jsx';
import { api } from '../shared/api.js';
import { platform, platformDirect } from '../shared/platform.js';
import { daysLeft, formatDate } from '../shared/time.js';
import { Button, Card, CopyButton, Empty, ErrorNote, Field, Note, Skeleton, Time, useAction, useConfirm, useLoad, useToast } from '../shared/ui.jsx';

/** 账户页面右上角：数据从哪里来；连不上平台时说明显示不了。 */
export function SourceTag({ error }) {
  const { t } = useTranslation();
  if (error) {
    return (
      <span className="src-tag warn">
        <CloudOff size={14} aria-hidden="true" />
        {t('acct.source_offline')}
      </span>
    );
  }
  return (
    <span className="src-tag" title={t(platformDirect ? 'acct.source_direct_hint' : 'acct.source_client_hint')}>
      <CheckCircle2 size={14} aria-hidden="true" />
      {t(platformDirect ? 'acct.source_direct' : 'acct.source_client')}
    </span>
  );
}

/** 输入时自动转大写并按 4 位一组加连字符。 */
function formatCode(v) {
  const s = v.toUpperCase().replace(/[^A-Z0-9]/g, '').slice(0, 12);
  return s.match(/.{1,4}/g)?.join('-') ?? '';
}

/** 会员信息变了以后，让这台电脑上缓存的账户信息（侧栏的剩余天数、到期提醒）也更新。 */
function refreshLocal(reload) {
  api
    .get('/account')
    .catch(() => {})
    .finally(reload);
}

function MemberCard({ me, onRedeemed }) {
  const { t, i18n } = useTranslation();
  const toast = useToast();
  const [code, setCode] = useState('');
  const { busy, error, run } = useAction();
  const left = daysLeft(me.membership_expires_at);
  const submit = (e) => {
    e.preventDefault();
    run(async () => {
      const r = await platform.post('/membership/redeem', { code });
      setCode('');
      toast(t('acct.redeemed', { n: r.days, date: formatDate(r.me.membership_expires_at, i18n.language) }));
      onRedeemed();
    });
  };
  return (
    <Card>
      <div className="between">
        <span className="inline">
          <CalendarClock size={20} aria-hidden="true" />
          <b>{t(me.plan === 'paid' ? 'acct.plan_paid' : 'acct.plan_free')}</b>
        </span>
        <span className={`badge ${!me.membership_valid ? 'badge-danger' : left <= 30 ? 'badge-warn' : 'badge-ok'}`}>
          {me.membership_valid ? t('account.days_left', { n: left }) : t('account.expired')}
        </span>
      </div>
      <div className="member-date">{formatDate(me.membership_expires_at, i18n.language)}</div>
      <p className="muted small">{t('acct.member_lead')}</p>
      <form onSubmit={submit} className="stack-sm">
        <Field label={t('acct.redeem_title')} hint={t('acct.redeem_hint')}>
          <div className="row">
            <input
              className="mono code-input"
              autoCapitalize="characters"
              autoComplete="off"
              spellCheck="false"
              placeholder="XXXX-XXXX-XXXX"
              value={code}
              onChange={(e) => setCode(formatCode(e.target.value))}
            />
            <Button type="submit" icon={Ticket} busy={busy} disabled={code.replace(/-/g, '').length < 12}>
              {t('acct.redeem')}
            </Button>
          </div>
        </Field>
        <ErrorNote error={error} />
      </form>
    </Card>
  );
}

function Invites() {
  const { t, i18n } = useTranslation();
  const { status } = useStatus();
  const toast = useToast();
  const confirm = useConfirm();
  const [data, error, reload] = useLoad(() => platform.get('/invites'));
  const { busy, error: actionError, run } = useAction();
  const link = (code) => webUrl(status, `/register?invite=${encodeURIComponent(code)}`);
  if (!data) {
    return (
      <Card title={t('acct.invites')}>
        <ErrorNote error={error} />
        {!error && <Skeleton height={120} />}
      </Card>
    );
  }
  const full = data.items.length >= data.max_codes;
  const create = () =>
    run(async () => {
      await platform.post('/invites');
      toast(t('acct.invite_created'));
      reload();
    });
  const revoke = async (x) => {
    if (!(await confirm({ title: t('acct.invite_revoke_title'), body: t('acct.invite_revoke_body'), confirm: t('acct.invite_revoke'), danger: true }))) return;
    run(async () => {
      await platform.del(`/invites/${x.id}`);
      toast(t('acct.invite_revoked'));
      reload();
    });
  };
  return (
    <Card
      title={t('acct.invites')}
      actions={
        <Button variant="secondary" icon={Plus} busy={busy} disabled={full || !data.registration_open} onClick={create}>
          {full ? t('acct.invite_full', { max: data.max_codes }) : t('acct.invite_create')}
        </Button>
      }
    >
      <p className="muted small">{data.reward_days > 0 ? t('acct.invite_reward', { n: data.reward_days }) : t('acct.invite_lead')}</p>
      {!data.registration_open && <Note kind="warn">{t('acct.invite_closed')}</Note>}
      <div className="stats">
        <div className="stat">
          <span className="muted small">{t('acct.invited')}</span>
          <b>{t('acct.n_people', { n: data.invited_count })}</b>
        </div>
        <div className="stat">
          <span className="muted small">{t('acct.rewarded')}</span>
          <b>{t('acct.plus_days', { n: data.rewarded_days_total })}</b>
        </div>
      </div>
      <ErrorNote error={actionError} />
      {data.items.length === 0 ? (
        <Empty icon={UserPlus} title={t('acct.invite_empty')} />
      ) : (
        <ul className="list">
          {data.items.map((x) => (
            <li key={x.id}>
              <span className="grow" style={{ minWidth: 0 }}>
                <span className="inline">
                  <b className="mono">{x.code}</b>
                  <span className={`badge ${x.used_count > 0 ? 'badge-ok' : ''}`}>{t('acct.code_used', { n: x.used_count })}</span>
                </span>
                <span className="muted small ellipsis" style={{ display: 'block', marginTop: 4 }}>
                  {link(x.code)}
                </span>
              </span>
              <span className="inline">
                <CopyButton text={link(x.code)} label={t('acct.copy_link')} />
                <Button variant="plain" busy={busy} onClick={() => revoke(x)}>
                  {t('acct.invite_revoke')}
                </Button>
              </span>
            </li>
          ))}
        </ul>
      )}
      {data.friends.length > 0 && (
        <>
          <h3 className="sub-title">{t('acct.friends')}</h3>
          <ul className="list">
            {data.friends.map((f, i) => (
              <li key={i}>
                <span className="inline">
                  <Gift size={16} className="muted" aria-hidden="true" />
                  <b>{f.nickname}</b>
                </span>
                <span className="inline">
                  {f.reward_days > 0 && <span className="badge badge-ok">{t('acct.plus_days', { n: f.reward_days })}</span>}
                  <span className="muted small">{t('acct.joined', { date: formatDate(f.joined_at, i18n.language) })}</span>
                </span>
              </li>
            ))}
          </ul>
        </>
      )}
    </Card>
  );
}

export default function Membership() {
  const { t, i18n } = useTranslation();
  const { reload: reloadStatus } = useStatus();
  const [data, error, reload] = useLoad(() => platform.get('/membership'));
  return (
    <>
      <div className="page-head">
        <h1>{t('acct.membership')}</h1>
        <SourceTag error={error} />
      </div>
      <ErrorNote error={error} />
      {!data ? (
        !error && <Skeleton height={220} />
      ) : (
        <>
          <MemberCard
            me={data.me}
            onRedeemed={() => {
              reload();
              refreshLocal(reloadStatus);
            }}
          />
          <Invites />
          <Card title={t('acct.history_member')}>
            {data.history.length === 0 ? (
              <Empty icon={History} title={t('acct.history_member_empty')} />
            ) : (
              <ul className="list">
                {data.history.map((h) => (
                  <li key={h.id}>
                    <span>
                      <b>{h.kind === 'invite' ? t('acct.history_invite', { name: h.friend, n: h.days }) : t('acct.history_renewal', { n: h.days })}</b>
                      <span className="muted small" style={{ display: 'block' }}>
                        {t('acct.history_to', { date: formatDate(h.new_expires_at, i18n.language) })}
                      </span>
                    </span>
                    <span className="muted small">
                      <Time value={h.created_at} />
                    </span>
                  </li>
                ))}
              </ul>
            )}
          </Card>
        </>
      )}
    </>
  );
}
