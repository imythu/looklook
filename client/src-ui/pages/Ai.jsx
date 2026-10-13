// AI 助手：MCP 接口（令牌、一键装进 Codex / Claude Code / OpenCode / DSH）和它给 AI 助手的全部工具，
// 一处看清 AI 能做什么：本机网页的工具，以及 SSH 工具的总开关和每台主机“允许 AI 使用”的快捷开关。
// MCP 的令牌和配置只能直接在这台电脑上看、改；远程打开时只显示工具和主机开关。
import { Bot, Globe, Server, Wrench } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link } from '../shared/router.jsx';
import { Card, ErrorNote, Note, Skeleton, Switch, useLoad, useToast } from '../shared/ui.jsx';
import McpCard from './PagesMcp.jsx';

const PAGE_TOOLS = ['list_mappings', 'find_mapping', 'ensure_mapping'];
const SSH_TOOLS = ['list_ssh_hosts', 'ssh_run'];

function ToolList({ names, on }) {
  const { t } = useTranslation();
  return (
    <ul className="ai-tools">
      {names.map((n) => (
        <li key={n} className={on ? '' : 'off'}>
          <code className="mono small">{n}</code>
          <span className="small muted">{t(`ai.tool.${n}`)}</span>
        </li>
      ))}
    </ul>
  );
}

function ToolsCard({ version, local }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [data, error, reload] = useLoad(() => api.get('/ssh/hosts'), [version]);
  const [busy, setBusy] = useState(null);
  if (error) return <ErrorNote error={error} />;
  if (!data) return <Skeleton height={200} />;
  const { mcp, ssh } = data.ai;
  const run = async (key, fn) => {
    setBusy(key);
    try {
      await fn();
      reload();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(null);
    }
  };
  const allowed = data.items.filter((h) => h.allow_ai).length;
  return (
    <Card
      title={
        <span className="inline">
          <Wrench size={18} aria-hidden="true" /> {t('ai.tools_title')}
        </span>
      }
    >
      <p className="muted">{mcp ? t('ai.tools_intro') : t('ai.tools_off')}</p>
      <section className="ai-group">
        <div className="ai-group-head">
          <Globe size={16} aria-hidden="true" />
          <b className="grow">{t('ai.group_pages')}</b>
          <span className={`badge ${mcp ? 'badge-ok' : ''}`}>{t(mcp ? 'ai.on' : 'ai.off')}</span>
        </div>
        <ToolList names={PAGE_TOOLS} on={mcp} />
      </section>
      <section className="ai-group">
        <div className="ai-group-head">
          <Server size={16} aria-hidden="true" />
          <b className="grow">{t('ai.group_ssh')}</b>
          <Switch checked={ssh} disabled={!local || busy === 'ssh'} label={t('ai.ssh_switch')} onChange={(on) => run('ssh', () => api.put('/mcp', { ssh: on }))} />
        </div>
        <p className="small muted">{t(ssh ? 'ai.ssh_on_hint' : 'ai.ssh_off_hint')}</p>
        {!local && <p className="small muted">{t('ai.ssh_switch_local')}</p>}
        <ToolList names={SSH_TOOLS} on={mcp && ssh} />
        {data.items.length === 0 ? (
          <p className="small muted">
            {t('ai.no_hosts')} <Link to="/ssh">{t('nav.ssh')}</Link>
          </p>
        ) : (
          <>
            <p className="small">{t('ai.hosts_count', { n: allowed, total: data.items.length })}</p>
            <ul className="list ai-hosts">
              {data.items.map((h) => (
                <li key={h.id} className={ssh ? '' : 'off'}>
                  <span className="grow" style={{ minWidth: 0 }}>
                    <b className="ellipsis">{h.name}</b>
                    <span className="mono small muted ellipsis">
                      {h.user}@{h.host}
                      {h.port !== 22 && `:${h.port}`}
                    </span>
                  </span>
                  <Switch
                    checked={h.allow_ai}
                    disabled={busy === h.id}
                    label={`${t('ssh.allow_ai')} · ${h.name}`}
                    onChange={(allow) => run(h.id, () => api.put(`/ssh/hosts/${h.id}/ai`, { allow }))}
                  />
                </li>
              ))}
            </ul>
          </>
        )}
      </section>
    </Card>
  );
}

export default function AiPage() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const local = status.access === 'local';
  // MCP 开关一变，工具卡片跟着刷新
  const [version, setVersion] = useState(0);
  return (
    <>
      <div className="page-head">
        <h1 className="inline">
          <Bot size={24} aria-hidden="true" /> {t('ai.title')}
        </h1>
      </div>
      <p className="muted">{t('ai.intro')}</p>
      {local ? <McpCard onChange={() => setVersion((v) => v + 1)} /> : <Note kind="info">{t('ai.local_only')}</Note>}
      <ToolsCard version={version} local={local} />
    </>
  );
}
