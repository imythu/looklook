// 电脑页的安装密钥（docs/PLAN_2026-10-27.md §5）：给云服务器、镜像、开机脚本用的预先批准的电脑授权，
// 装好后 `looklook login --key LLK-…` 直接登录，不用扫码、不用到浏览器里批准。数据在平台上（shared/platform.js），
// 和平台网页“电脑”页的同一张卡片一样。生成要输入密码；密钥只在生成时显示一次（平台只存哈希）。
import { KeyRound, Plus, X } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus, webUrl } from '../App.jsx';
import { platform } from '../shared/platform.js';
import { formatTime, relativeTime } from '../shared/time.js';
import { Button, Card, CopyButton, ErrorNote, Field, Modal, Note, PasswordInput, useAction, useConfirm, useLoad, useToast } from '../shared/ui.jsx';

const HOURS = [1, 24, 168];
const MAX_ACTIVE = 20;

function CommandLine({ label, cmd }) {
  return (
    <>
      <p className="field-label" style={{ marginTop: 12 }}>
        {label}
      </p>
      <div className="row">
        <input className="mono" readOnly value={cmd} onFocus={(e) => e.target.select()} />
        <CopyButton text={cmd} label={false} />
      </div>
    </>
  );
}

function CreateModal({ onClose, onDone }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const [hours, setHours] = useState(1);
  const [uses, setUses] = useState(1);
  const [note, setNote] = useState('');
  const [password, setPassword] = useState('');
  const [created, setCreated] = useState(null);
  const { busy, error, run } = useAction();
  const submit = (e) => {
    e.preventDefault();
    run(async () => {
      setCreated(await platform.post('/install-keys', { hours, uses, note: note.trim(), password }));
      onDone();
    });
  };
  if (created) {
    const install = `curl -fsSL ${webUrl(status, '/install.sh')} | sh -s -- --key ${created.key}`;
    return (
      <Modal open onClose={onClose} title={t('ikey.created_title')} wide>
        <Note kind="warn">{t('ikey.created_once')}</Note>
        <CommandLine label={t('ikey.key')} cmd={created.key} />
        <CommandLine label={t('ikey.cmd_install')} cmd={install} />
        <CommandLine label={t('ikey.cmd_login')} cmd={`looklook login --key ${created.key}`} />
        <p className="muted small">{t('ikey.cmd_hint')}</p>
        <div className="modal-actions">
          <Button onClick={onClose}>{t('action.close')}</Button>
        </div>
      </Modal>
    );
  }
  return (
    <Modal open onClose={onClose} title={t('ikey.create_title')}>
      <form onSubmit={submit}>
        <p className="muted">{t('ikey.create_explain')}</p>
        <Field label={t('ikey.valid_for')}>
          <select value={hours} onChange={(e) => setHours(Number(e.target.value))}>
            {HOURS.map((h) => (
              <option key={h} value={h}>
                {t(`ikey.hours_${h}`)}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t('ikey.uses')} hint={t('ikey.uses_hint')}>
          <select value={uses} onChange={(e) => setUses(Number(e.target.value))}>
            {[1, 2, 3, 4, 5, 6, 7].map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </Field>
        <Field label={t('ikey.note')}>
          <input value={note} maxLength={60} onChange={(e) => setNote(e.target.value)} placeholder={t('ikey.note_placeholder')} />
        </Field>
        <Field label={t('acct.password')}>
          <PasswordInput autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={!password}>
            {t('ikey.create')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

export default function InstallKeysCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const confirm = useConfirm();
  const [data, loadError, reload] = useLoad(() => platform.get('/install-keys'));
  const [creating, setCreating] = useState(false);
  const { error, run } = useAction();
  const items = data?.items ?? [];
  const revoke = async (k) => {
    const ok = await confirm({ title: t('ikey.revoke_title', { hint: k.hint }), body: t('ikey.revoke_body'), confirm: t('ikey.revoke'), danger: true });
    if (ok && (await run(async () => (await platform.del(`/install-keys/${k.id}`), true)))) {
      toast(t('ikey.revoked'));
      reload();
    }
  };
  return (
    <Card
      title={t('ikey.title')}
      actions={
        <Button variant="secondary" icon={Plus} disabled={!data || items.length >= MAX_ACTIVE} onClick={() => setCreating(true)}>
          {t('ikey.create')}
        </Button>
      }
    >
      <p className="muted small">{t('ikey.lead')}</p>
      <ErrorNote error={loadError || error} />
      {items.length > 0 && (
        <ul className="list">
          {items.map((k) => (
            <li key={k.id}>
              <span className="inline" style={{ flexWrap: 'nowrap', minWidth: 0 }}>
                <KeyRound size={18} className="muted" style={{ flexShrink: 0 }} />
                <span style={{ minWidth: 0 }}>
                  <b className="mono">LLK-…{k.hint}</b>
                  {k.note && <span> · {k.note}</span>}
                  <span className="muted small" style={{ display: 'block' }}>
                    {t('ikey.left', { left: k.uses_left, total: k.uses_total })} · <span title={formatTime(k.expires_at)}>{t('ikey.expires', { when: relativeTime(t, k.expires_at) })}</span>
                  </span>
                </span>
              </span>
              <Button variant="ghost" icon={X} aria-label={t('ikey.revoke_title', { hint: k.hint })} title={t('ikey.revoke')} onClick={() => revoke(k)} />
            </li>
          ))}
        </ul>
      )}
      {creating && <CreateModal onClose={() => setCreating(false)} onDone={reload} />}
    </Card>
  );
}
