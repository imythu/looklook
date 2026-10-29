// 设置 → 网络与访问 → 直连：远程打开这台电脑时 WebRTC 用的传输端口范围、公网 IPv4 / IPv6（服务端 docs/07）。
// 公网地址也可以填域名（DDNS），客户端定时解析，保存后在输入框下显示当前解析结果。
// 端口范围和地址一起点保存；IPv6 开关点一下就保存。改了只对新建的连接生效；远程打开时也能改（改错了还有中转）。
// “检测”由这台电脑去问中转的 STUN 和 cip.cc、ip.me 等服务，结果只填进输入框，不自动保存。
import { Search } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { Button, Card, ErrorNote, Field, Note, Skeleton, Switch, useAction, useToast } from '../shared/ui.jsx';

/** 填的是域名（有字母、没有冒号）：检测不覆盖它。 */
const isName = (s) => /[a-z]/i.test(s) && !s.includes(':');

const form = (d) => ({ port_begin: String(d.port_begin), port_end: String(d.port_end), public_v4: d.public_v4, public_v6: d.public_v6 });

/** 检测结果：每个来源一行（地址或失败原因），各来源不一致时提示。 */
function Probes({ items }) {
  const { t } = useTranslation();
  if (!items) return null;
  const ips = items.filter((p) => p.ip).map((p) => p.ip);
  return (
    <span className="field-hint direct-probes">
      {items.map((p) => (
        <span key={p.source} className="direct-probe">
          <span>{p.source === 'relay' ? t('direct.source_relay') : p.source}</span>
          <span className="mono">{p.ip || t('direct.probe_failed', { why: p.error })}</span>
        </span>
      ))}
      {ips.some((ip) => ip !== ips[0]) && <span className="direct-probe-warn">{t('direct.probe_disagree')}</span>}
    </span>
  );
}

export default function DirectCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const [data, setData] = useState(null);
  const [f, setF] = useState(null);
  const [loadError, setLoadError] = useState(null);
  const [detected, setDetected] = useState(null);
  const { busy, error, run } = useAction();
  const detect = useAction();
  useEffect(() => {
    api.get('/rtc/net').then((d) => {
      setData(d);
      setF(form(d));
    }, setLoadError);
  }, []);
  if (loadError) return <ErrorNote error={loadError} />;
  if (!data || !f) return <Skeleton height={240} />;

  const save = async (body) => {
    const r = await run(() => api.put('/rtc/net', body));
    if (r) {
      setData(r);
      setF(form(r));
      toast(t('common.saved'));
    }
  };
  const set = (k) => (e) => setF({ ...f, [k]: e.target.value });
  const changed = Object.keys(f).some((k) => f[k] !== form(data)[k]);
  const [defBegin, defEnd] = data.default_ports;
  const isDefault = f.port_begin === String(defBegin) && f.port_end === String(defEnd);
  // 保存的是域名时显示它现在解析出的地址（输入框改动后不显示）
  const addrHint = (k, fallback) => {
    const r = data[`resolved_${k}`];
    if (r === undefined || f[`public_${k}`] !== data[`public_${k}`]) return fallback;
    return r ? t('direct.resolved', { ip: r }) : t('direct.unresolved');
  };
  const fieldError = (name) => (error?.params?.field === name ? t(`error:rule.${error.params.rule}`) : undefined);

  const runDetect = async () => {
    const r = await detect.run(() => api.post('/rtc/net/detect', {}));
    if (!r) return;
    setDetected(r);
    const pick = (cur, ip) => (isName(cur) ? cur : ip || cur);
    setF((cur) => ({ ...cur, public_v4: pick(cur.public_v4, r.suggest_v4), public_v6: pick(cur.public_v6, r.suggest_v6) }));
  };

  return (
    <Card title={t('direct.title')}>
      <p className="muted small" style={{ marginTop: -6 }}>
        {t('direct.intro')}
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          save({ port_begin: Number(f.port_begin), port_end: Number(f.port_end), public_v4: f.public_v4.trim(), public_v6: f.public_v6.trim() });
        }}
      >
        <Field label={t('direct.ports')} hint={t('direct.ports_hint')} error={fieldError('rtc_ports')}>
          <span className="row direct-ports">
            <input value={f.port_begin} onChange={set('port_begin')} inputMode="numeric" className="mono" aria-label={t('direct.port_begin')} />
            <span aria-hidden="true">–</span>
            <input value={f.port_end} onChange={set('port_end')} inputMode="numeric" className="mono" aria-label={t('direct.port_end')} />
            {!isDefault && (
              <Button variant="plain" onClick={() => setF({ ...f, port_begin: String(defBegin), port_end: String(defEnd) })}>
                {t('direct.ports_default')}
              </Button>
            )}
          </span>
        </Field>
        <Field label={t('direct.public_v4')} hint={addrHint('v4', t('direct.public_v4_hint'))} error={fieldError('public_v4')}>
          <input value={f.public_v4} onChange={set('public_v4')} className="mono" placeholder={t('direct.not_set')} autoComplete="off" autoCapitalize="off" spellCheck={false} />
        </Field>
        {detected && <Probes items={detected.v4} />}
        <Field
          label={t('direct.public_v6')}
          hint={addrHint('v6', data.nic_v6.length ? t('direct.public_v6_nic', { ips: data.nic_v6.join(', ') }) : t('direct.public_v6_hint'))}
          error={fieldError('public_v6')}
        >
          <input value={f.public_v6} onChange={set('public_v6')} className="mono" placeholder={t('direct.not_set')} autoComplete="off" autoCapitalize="off" spellCheck={false} />
        </Field>
        {detected && <Probes items={detected.v6} />}
        <div className="row" style={{ margin: '12px 0' }}>
          <Button type="submit" busy={busy} disabled={!changed}>
            {t('action.save')}
          </Button>
          <Button variant="secondary" icon={Search} busy={detect.busy} onClick={runDetect}>
            {t('direct.detect')}
          </Button>
        </div>
        <span className="field-hint">{t('direct.detect_hint')}</span>
        <ErrorNote error={detect.error} />
      </form>
      <div className="toggle-row">
        <span className="grow">
          <b>{t('direct.ipv6')}</b>
          <span className="field-hint" style={{ display: 'block' }}>
            {t('direct.ipv6_hint')}
          </span>
        </span>
        <Switch checked={data.ipv6} onChange={(v) => save({ ipv6: v })} disabled={busy} label={t('direct.ipv6')} />
      </div>
      <Note>{t('direct.firewall', { begin: data.port_begin, end: data.port_end })}</Note>
      <ErrorNote error={error?.params?.field ? null : error} />
    </Card>
  );
}
