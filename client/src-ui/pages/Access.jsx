// 设置 → 访问控制：其他设备能不能直接打开这台电脑上的管理台。
// 这台电脑自己总是可以打开；局域网要勾选允许；白名单是额外允许的 IP；访问码可选，只存在本机。
// 修改立即生效（开关点一下就保存），不需要重启看看。
import { KeyRound, Plus, RefreshCw, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { SafetyNote, useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { checkOpenHost, urlHost } from '../shared/host.js';
import { Button, Card, CopyButton, ErrorNote, Note, Skeleton, Switch, useAction, useToast } from '../shared/ui.jsx';

/** 一行“开关 + 标题 + 说明”。 */
function Toggle({ checked, onChange, disabled, title, children }) {
  return (
    <div className="toggle-row">
      <span className="grow">
        <b>{title}</b>
        <span className="field-hint" style={{ display: 'block' }}>
          {children}
        </span>
      </span>
      <Switch checked={checked} onChange={onChange} disabled={disabled} label={title} />
    </div>
  );
}

function CodeBox({ data, save, busy }) {
  const { t } = useTranslation();
  const [custom, setCustom] = useState(null);
  return (
    <div className="access-code">
      <div className="access-code-row">
        <span className="access-code-value mono">{data.code}</span>
        <CopyButton text={data.code} label={false} variant="ghost" />
        <Button variant="ghost" icon={RefreshCw} busy={busy} onClick={() => save({ new_code: true })}>
          {t('access.code_new')}
        </Button>
        {custom === null && (
          <Button variant="plain" onClick={() => setCustom('')}>
            {t('access.code_custom')}
          </Button>
        )}
      </div>
      {custom !== null && (
        <form
          className="row"
          onSubmit={async (e) => {
            e.preventDefault();
            if (await save({ code: custom })) setCustom(null);
          }}
        >
          <input value={custom} onChange={(e) => setCustom(e.target.value)} className="mono" autoComplete="off" spellCheck={false} placeholder={t('access.code_custom_placeholder')} aria-label={t('access.code_custom')} />
          <Button type="submit" busy={busy} disabled={custom.replace(/[\s-]/g, '').length < 8}>
            {t('action.save')}
          </Button>
        </form>
      )}
      <span className="field-hint">{t('access.code_forgot')}</span>
    </div>
  );
}

function Whitelist({ data, save, busy }) {
  const { t } = useTranslation();
  const [ip, setIp] = useState('');
  const add = async (e) => {
    e.preventDefault();
    if (await save({ allowed_ips: [...data.allowed_ips, ip.trim()] })) setIp('');
  };
  return (
    <div className="field">
      <b>{t('access.list_title')}</b>
      <span className="field-hint">{t('access.list_hint')}</span>
      {data.allowed_ips.length > 0 && (
        <div className="chips">
          {data.allowed_ips.map((x) => (
            <span key={x} className="chip chip-x mono">
              {x}
              <button type="button" aria-label={t('access.list_remove', { ip: x })} disabled={busy} onClick={() => save({ allowed_ips: data.allowed_ips.filter((y) => y !== x) })}>
                <X size={14} />
              </button>
            </span>
          ))}
        </div>
      )}
      <form className="row" onSubmit={add}>
        <input value={ip} onChange={(e) => setIp(e.target.value)} className="mono" inputMode="decimal" autoComplete="off" spellCheck={false} placeholder="203.0.113.7" aria-label={t('access.list_title')} />
        <Button type="submit" variant="secondary" icon={Plus} busy={busy} disabled={!ip.trim()}>
          {t('access.list_add')}
        </Button>
      </form>
    </div>
  );
}

function OpenHost({ data, save, busy }) {
  const { t } = useTranslation();
  const [value, setValue] = useState(data.open_host);
  useEffect(() => setValue(data.open_host), [data.open_host]);
  const rule = checkOpenHost(value);
  const examples = t('access.open_host_example_list').split('|');
  return (
    <form
      className="field"
      onSubmit={(e) => {
        e.preventDefault();
        save({ open_host: value });
      }}
    >
      <label className="field-label" htmlFor="open-host">
        {t('access.open_host')}
      </label>
      <span className="field-hint">{t('access.open_host_hint')}</span>
      <div className="row">
        <input id="open-host" value={value} onChange={(e) => setValue(e.target.value)} className="mono" placeholder={t('access.open_host_placeholder')} inputMode="url" autoComplete="off" autoCapitalize="off" spellCheck={false} aria-invalid={rule ? 'true' : undefined} />
        <Button type="submit" busy={busy} disabled={Boolean(rule) || value.trim() === data.open_host}>
          {t('action.save')}
        </Button>
      </div>
      {rule ? (
        <span className="field-error">{t(`error:rule.${rule}`)}</span>
      ) : (
        <span className="field-hint">{t('access.open_host_url', { url: `http://${value.trim() ? urlHost(value) : '127.0.0.1'}:${data.port}/` })}</span>
      )}
      <div className="chips" aria-label={t('access.open_host_examples')}>
        <span className="small muted">{t('access.open_host_examples')}</span>
        {examples.map((x) => (
          <button key={x} type="button" className="chip mono" onClick={() => setValue(x)}>
            {x}
          </button>
        ))}
      </div>
    </form>
  );
}

/** Windows 第一次运行时防火墙会弹窗（管理台监听所有网卡）：说明怎么选、选错了怎么改。 */
function WindowsFirewall({ exe }) {
  const { t } = useTranslation();
  const program = exe || '$env:LOCALAPPDATA\\Looklook\\looklook.exe';
  const ps = `New-NetFirewallRule -DisplayName "Looklook LAN" -Direction Inbound -Action Allow -Profile Private -Program "${program}"`;
  return (
    <details className="fw">
      <summary>{t('access.fw_title')}</summary>
      <p className="small muted">{t('access.fw_why')}</p>
      <ol className="fw-steps small">
        <li>{t('access.fw_local')}</li>
        <li>{t('access.fw_lan')}</li>
        <li>{t('access.fw_profile')}</li>
        <li>
          {t('access.fw_fix')}
          <span className="fw-cmd">
            <code className="mono">{ps}</code>
            <CopyButton text={ps} label={false} variant="plain" />
          </span>
        </li>
        <li>{t('access.fw_never')}</li>
      </ol>
    </details>
  );
}

export default function AccessCard() {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const [data, setData] = useState(null);
  const [loadError, setLoadError] = useState(null);
  const { busy, error, run } = useAction();
  useEffect(() => {
    api.get('/access').then(setData, setLoadError);
  }, []);
  const save = async (body) => {
    const r = await run(() => api.put('/access', body));
    if (r) {
      setData(r);
      toast(t('common.saved'));
      reload();
    }
    return Boolean(r);
  };
  if (loadError?.code === 'REMOTE_FORBIDDEN') {
    return (
      <Card title={t('access.title')}>
        <Note>{t('access.remote_only')}</Note>
      </Card>
    );
  }
  if (loadError) return <ErrorNote error={loadError} />;
  if (!data) return <Skeleton height={240} />;
  const lanUrls = data.lan_ips.map((ip) => `http://${urlHost(ip)}:${data.port}/`);
  return (
    <Card title={t('access.title')}>
      <p className="muted small" style={{ marginTop: -6 }}>
        {t('access.intro')}
      </p>
      <Toggle checked={data.allow_lan} onChange={(v) => save({ allow_lan: v })} disabled={busy} title={t('access.lan')}>
        {t('access.lan_hint')}
      </Toggle>
      {data.allow_lan && (
        <div className="access-sub">
          {lanUrls.length > 0 ? (
            <>
              <span className="field-hint">{t('access.lan_urls')}</span>
              {lanUrls.map((u) => (
                <span key={u} className="one-line">
                  <a className="mono ellipsis" href={u} target="_blank" rel="noopener noreferrer">
                    {u}
                  </a>
                  <CopyButton text={u} label={false} variant="plain" />
                </span>
              ))}
            </>
          ) : (
            <span className="field-hint">{t('access.lan_unknown', { port: data.port })}</span>
          )}
          {!data.code_enabled && <Note kind="warn">{t('access.lan_no_code')}</Note>}
        </div>
      )}

      {status.capabilities.os === 'windows' && <WindowsFirewall exe={data.exe} />}

      <Toggle checked={data.code_enabled} onChange={(v) => save({ code_enabled: v })} disabled={busy} title={t('access.code')}>
        {t('access.code_hint')}
      </Toggle>
      {data.code_enabled && <CodeBox data={data} save={save} busy={busy} />}

      <Whitelist data={data} save={save} busy={busy} />
      {data.allowed_ips.length > 0 && !data.code_enabled && (
        <Note kind="warn" icon={KeyRound}>
          {t('access.list_no_code')}{' '}
          <button type="button" className="btn btn-plain" style={{ minHeight: 28, padding: '0 4px' }} onClick={() => save({ code_enabled: true })}>
            {t('access.code_enable')}
          </button>
        </Note>
      )}

      <OpenHost data={data} save={save} busy={busy} />
      <ErrorNote error={error} />
      <SafetyNote full />
    </Card>
  );
}
