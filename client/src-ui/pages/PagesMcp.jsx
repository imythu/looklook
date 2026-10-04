// 本机网页 → AI 助手：打开 MCP 接口后，Codex / Claude Code / OpenCode / DSH 在开发时可以自己查看、添加本机网页。
// 令牌和配置都只在这台机器上；只有直接在本机（或允许的局域网）打开管理台时显示。
import { Bot, RefreshCw, Trash2 } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Button, Card, CopyButton, ErrorNote, FoldNote, Note, Skeleton, Switch, useConfirm, useLoad, useToast } from '../shared/ui.jsx';

const AGENT_NAME = { claude: 'Claude Code', codex: 'Codex', opencode: 'OpenCode', dsh: 'DSH (DeepSeek Harness)' };

function AgentRow({ a, skill, enabled, busy, onInstall, onRemove }) {
  const { t } = useTranslation();
  const installed = a.mcp !== 'none' || a.skill;
  let badge;
  if (!a.available) badge = <span className="badge">{t('pages.mcp.not_found')}</span>;
  else if (a.mcp === 'ok') badge = <span className="badge badge-ok">{a.skill ? t(`pages.mcp.installed_${a.skill}`) : t('pages.mcp.installed')}</span>;
  else if (a.mcp === 'stale') badge = <span className="badge badge-warn">{t('pages.mcp.stale')}</span>;
  else badge = <span className="badge">{t('pages.mcp.not_installed')}</span>;
  // 已装好且技能就是现在选的：不用再装
  const current = a.mcp === 'ok' && (a.skill ?? 'none') === skill;
  return (
    <li>
      <span className="grow inline">
        <b>{AGENT_NAME[a.agent]}</b>
        {badge}
      </span>
      <span className="inline" style={{ flexWrap: 'nowrap' }}>
        {a.available && (
          <Button variant={current ? 'ghost' : 'primary'} disabled={!enabled || busy || current} onClick={() => onInstall(a.agent)}>
            {a.mcp === 'none' ? t('pages.mcp.install') : current ? t('pages.mcp.up_to_date') : t('pages.mcp.reinstall')}
          </Button>
        )}
        {installed && <Button variant="ghost" icon={Trash2} disabled={busy} aria-label={t('pages.mcp.remove')} onClick={() => onRemove(a.agent)} />}
      </span>
    </li>
  );
}

export default function McpCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const confirm = useConfirm();
  const [loaded, error] = useLoad(() => api.get('/mcp'));
  const [view, setView] = useState(null);
  const [skill, setSkill] = useState(null);
  const [busy, setBusy] = useState(false);
  const v = view ?? loaded;

  // 默认选中已经装着的技能；都没装选“先问”
  useEffect(() => {
    if (v && skill === null) setSkill(v.agents.find((a) => a.skill)?.skill ?? 'ask');
  }, [v, skill]);

  const act = async (fn, done) => {
    setBusy(true);
    try {
      setView(await fn());
      if (done) toast(done);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };

  const rotate = async () => {
    if (await confirm({ title: t('pages.mcp.rotate_title'), body: t('pages.mcp.rotate_body'), confirm: t('pages.mcp.rotate') })) {
      act(() => api.put('/mcp', { rotate: true }), t('pages.mcp.rotated'));
    }
  };
  const remove = async (agent) => {
    if (await confirm({ title: t('pages.mcp.remove_title', { name: AGENT_NAME[agent] }), body: t('pages.mcp.remove_body', { name: AGENT_NAME[agent] }), confirm: t('pages.mcp.remove'), danger: true })) {
      act(() => api.post('/mcp/uninstall', { agent }), t('pages.mcp.removed'));
    }
  };

  if (error) return <ErrorNote error={error} />;
  if (!v) return <Skeleton height={120} />;
  const skills = [
    ['auto', t('pages.mcp.skill_auto'), t('pages.mcp.skill_auto_hint')],
    ['ask', t('pages.mcp.skill_ask'), t('pages.mcp.skill_ask_hint')],
    ['none', t('pages.mcp.skill_none'), t('pages.mcp.skill_none_hint')],
  ];
  const hint = skills.find(([k]) => k === skill)?.[2];
  return (
    <Card
      title={
        <span className="inline">
          <Bot size={18} aria-hidden="true" /> {t('pages.mcp.title')}
        </span>
      }
      actions={<Switch checked={v.enabled} disabled={busy} label={t('pages.mcp.enable')} onChange={(on) => act(() => api.put('/mcp', { enabled: on }))} />}
    >
      <p className="muted">{t('pages.mcp.intro')}</p>
      {v.enabled && (
        <>
          <div className="field">
            <span className="field-label">{t('pages.mcp.skill')}</span>
            <div className="seg" role="group" aria-label={t('pages.mcp.skill')}>
              {skills.map(([k, text]) => (
                <button key={k} type="button" aria-pressed={skill === k} onClick={() => setSkill(k)}>
                  {text}
                </button>
              ))}
            </div>
            <span className="field-hint">{hint}</span>
          </div>
          <ul className="list">
            {v.agents.map((a) => (
              <AgentRow
                key={a.agent}
                a={a}
                skill={skill}
                enabled={v.enabled}
                busy={busy}
                onInstall={(agent) => act(() => api.post('/mcp/install', { agent, skill }), t('pages.mcp.done', { name: AGENT_NAME[agent] }))}
                onRemove={remove}
              />
            ))}
          </ul>
          {v.agents.some((a) => a.mcp === 'stale') && <Note kind="warn">{t('pages.mcp.stale_note')}</Note>}
          <p className="muted small">{t('pages.mcp.restart_note')}</p>
          <FoldNote summary={t('pages.mcp.manual')}>
            <p>{t('pages.mcp.manual_body')}</p>
            <p className="inline">
              <span className="muted small">{t('pages.mcp.url')}</span> <code className="mono small">{v.url}</code> <CopyButton text={v.url} label={false} />
            </p>
            <p className="inline">
              <span className="muted small">{t('pages.mcp.header')}</span> <code className="mono small">Authorization: Bearer {v.token.slice(0, 10)}…</code>{' '}
              <CopyButton text={`Authorization: Bearer ${v.token}`} label={false} />
            </p>
            <Button variant="ghost" icon={RefreshCw} disabled={busy} onClick={rotate}>
              {t('pages.mcp.rotate')}
            </Button>
          </FoldNote>
        </>
      )}
    </Card>
  );
}
