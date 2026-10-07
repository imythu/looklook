// 设置 → 模型服务商：给 Claude Code、Codex 各选一个第三方接口（中转站）并填 API Key，之后新建或重启的终端走这个接口。
// 列表由平台下发（含推广）；Key 只保存在这台电脑上。第一次使用必须同意免责声明。
import { Bot, Check, ExternalLink, PencilLine } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { useRouter } from '../shared/router.jsx';
import { Button, Card, ErrorNote, Field, FoldNote, Modal, Note, PasswordInput, Skeleton, useAction, useConfirm, useLoad, useToast } from '../shared/ui.jsx';

const AGENT_NAME = { claude: 'Claude Code', codex: 'Codex' };
const CUSTOM = 'custom';

function Disclaimer() {
  const { t } = useTranslation();
  return (
    <ul className="model-terms">
      {['third_party', 'data', 'key', 'cost'].map((k) => (
        <li key={k}>{t(`model.disclaimer.${k}`)}</li>
      ))}
    </ul>
  );
}

function ProviderLogo({ p }) {
  const [broken, setBroken] = useState(false);
  if (!p.logo_url || broken) return <span className="model-logo">{(p.name || '?').slice(0, 1).toUpperCase()}</span>;
  return <img className="model-logo" src={p.logo_url} alt="" onError={() => setBroken(true)} />;
}

/** 新建终端表单里的一行：这个终端会走哪个接口。`device` 只用来在换电脑时重新读取（`client` 每次渲染都是新对象）。 */
export function ModelChoiceLine({ client, device, launch }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const agent = AGENT_NAME[launch] ? launch : null;
  const [data] = useLoad(() => (agent ? client.get('/model-providers?local=1') : Promise.resolve(null)), [device, agent]);
  if (!agent) return null;
  const choice = data?.agents?.find((a) => a.agent === agent)?.choice;
  return (
    <p className="field-hint model-line">
      <Bot size={15} aria-hidden="true" />
      <span>
        {choice ? t('model.form_using', { name: choice.name }) : t('model.form_official')} ·{' '}
        <a
          href="/settings#model"
          onClick={(e) => {
            e.preventDefault();
            navigate('/settings#model');
          }}
        >
          {t('model.form_change')}
        </a>
      </span>
    </p>
  );
}

function Picker({ agent, data, onClose, onSaved }) {
  const { t } = useTranslation();
  const toast = useToast();
  const a = data.agents.find((x) => x.agent === agent);
  const providers = data.providers.filter((p) => p.clients.some((c) => c.client === a.client));
  const current = a.choice;
  const [pick, setPick] = useState(current ? (current.provider_id ?? CUSTOM) : (providers[0]?.id ?? CUSTOM));
  const [name, setName] = useState(current && !current.provider_id ? current.name : '');
  const [baseUrl, setBaseUrl] = useState(current && !current.provider_id ? current.base_url : '');
  const [key, setKey] = useState('');
  const [agree, setAgree] = useState(false);
  const { busy, error, run } = useAction();
  const accepted = Boolean(data.accepted_at);
  // 已保存的 Key 跟着这个助手；换了服务商通常要换 Key，但不强制
  const keepKey = a.key_set;
  const chosen = providers.find((p) => p.id === pick);
  const endpoint = chosen?.clients.find((c) => c.client === a.client)?.base_url;
  const ready = (accepted || agree) && (pick !== CUSTOM || (name.trim() && baseUrl.trim())) && (keepKey || key.trim());
  const save = (e) => {
    e.preventDefault();
    run(async () => {
      const body = pick === CUSTOM ? { name, base_url: baseUrl } : { provider_id: pick };
      await api.put(`/model-providers/${agent}`, { ...body, api_key: key.trim() || undefined, accept: !accepted && agree });
      toast(t('model.saved', { agent: AGENT_NAME[agent] }));
      onSaved();
    });
  };
  return (
    <Modal open onClose={onClose} title={t('model.pick_title', { agent: AGENT_NAME[agent] })} wide>
      <form onSubmit={save}>
        {accepted ? (
          <FoldNote kind="warn" summary={t('model.disclaimer.title')}>
            <Disclaimer />
          </FoldNote>
        ) : (
          <Note kind="warn">
            <b>{t('model.disclaimer.title')}</b>
            <Disclaimer />
            <label className="check-row" style={{ margin: 0 }}>
              <input type="checkbox" checked={agree} onChange={(e) => setAgree(e.target.checked)} />
              <span>{t('model.disclaimer.agree')}</span>
            </label>
          </Note>
        )}
        {data.error && <Note kind="warn">{t('model.list_failed')}</Note>}
        <div className="model-grid" role="radiogroup" aria-label={t('model.provider')}>
          {providers.map((p) => (
            <button key={p.id} type="button" role="radio" aria-checked={pick === p.id} className="launch model-option" onClick={() => setPick(p.id)}>
              <span className="model-option-head">
                <ProviderLogo p={p} />
                <b className="grow ellipsis">{p.name}</b>
                <span className="badge">{t('promotion.label')}</span>
              </span>
              {p.tagline && <span className="small muted">{p.tagline}</span>}
            </button>
          ))}
          <button type="button" role="radio" aria-checked={pick === CUSTOM} className="launch model-option" onClick={() => setPick(CUSTOM)}>
            <span className="model-option-head">
              <span className="model-logo">
                <PencilLine size={16} />
              </span>
              <b className="grow">{t('model.custom')}</b>
            </span>
            <span className="small muted">{t('model.custom_desc')}</span>
          </button>
        </div>
        {!providers.length && !data.error && <p className="muted small">{t('model.none')}</p>}
        {chosen && (
          <div className="model-detail">
            {chosen.description && <p className="small" style={{ whiteSpace: 'pre-wrap' }}>{chosen.description}</p>}
            <p className="small muted" style={{ margin: 0 }}>
              {t('model.endpoint')} <span className="mono">{endpoint}</span> ·{' '}
              <a href={chosen.homepage_url} target="_blank" rel="noopener noreferrer sponsored">
                {t('model.homepage')} <ExternalLink size={13} style={{ verticalAlign: '-2px' }} />
              </a>
            </p>
          </div>
        )}
        {pick === CUSTOM && (
          <>
            <Field label={t('model.custom_name')}>
              <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} required />
            </Field>
            <Field label={t('model.custom_url')} hint={t(`model.custom_url_hint_${agent}`)}>
              <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="https://api.example.com" inputMode="url" spellCheck={false} autoCapitalize="none" required />
            </Field>
          </>
        )}
        <Field label="API Key" hint={keepKey ? t('model.key_keep') : t('model.key_hint')}>
          <PasswordInput value={key} onChange={(e) => setKey(e.target.value)} autoComplete="off" spellCheck={false} placeholder={keepKey ? '••••••••' : 'sk-…'} />
        </Field>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={!ready}>
            {t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

export default function ModelProvidersCard() {
  const { t, i18n } = useTranslation();
  const confirm = useConfirm();
  const toast = useToast();
  const [data, error, reload] = useLoad(() => api.get(`/model-providers?locale=${i18n.language}`), [i18n.language]);
  const [editing, setEditing] = useState(null);
  const { busy, error: actErr, run } = useAction();
  // 从新建终端表单的“更改”链接过来时滚到这里
  useEffect(() => {
    if (data && window.location.hash === '#model') document.getElementById('model')?.scrollIntoView({ block: 'start' });
  }, [Boolean(data)]);
  const reset = async (a) => {
    const ok = await confirm({ title: t('model.reset_title', { agent: AGENT_NAME[a.agent] }), body: t('model.reset_body'), confirm: t('model.reset') });
    if (ok && (await run(() => api.del(`/model-providers/${a.agent}`)))) {
      toast(t('model.reset_done'));
      reload();
    }
  };
  return (
    <div id="model" className="anchor">
      <Card title={t('model.title')}>
        <p className="muted small">{t('model.intro')}</p>
        <ErrorNote error={error ?? actErr} />
        {!data && !error && <Skeleton height={96} />}
        {data && (
          <ul className="list">
            {data.agents.map((a) => (
              <li key={a.agent}>
                <span className="grow">
                  <b>{AGENT_NAME[a.agent]}</b>
                  {a.choice ? (
                    <span className="small" style={{ display: 'block' }}>
                      <Check size={14} style={{ verticalAlign: '-2px', color: 'var(--ok)' }} /> {a.choice.name}
                      {!a.choice.provider_id && <span className="muted"> · {t('model.custom')}</span>}
                      <span className="mono muted ellipsis">{a.choice.base_url}</span>
                    </span>
                  ) : (
                    <span className="small muted" style={{ display: 'block' }}>
                      {t('model.official')}
                    </span>
                  )}
                </span>
                <span className="inline">
                  {a.choice && (
                    <Button variant="ghost" busy={busy} onClick={() => reset(a)}>
                      {t('model.reset')}
                    </Button>
                  )}
                  <Button variant={a.choice ? 'ghost' : 'secondary'} onClick={() => setEditing(a.agent)}>
                    {a.choice ? t('model.change') : t('model.choose')}
                  </Button>
                </span>
              </li>
            ))}
          </ul>
        )}
        <p className="muted small" style={{ margin: '12px 0 0' }}>
          {t('model.restart_note')}
        </p>
      </Card>
      {editing && data && (
        <Picker
          agent={editing}
          data={data}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            reload();
          }}
        />
      )}
    </div>
  );
}
