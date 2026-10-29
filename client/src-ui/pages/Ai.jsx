// AI 助手：MCP 接口（令牌、一键装进 Codex / Claude Code / OpenCode / DSH）和它给 AI 助手的全部工具，
// 一处看清 AI 能做什么：本机网页的工具，SSH 工具的总开关和每台主机“允许 AI 使用”的快捷开关，
// 以及飞书工具（以你本人的身份读、发飞书消息）的开关和这台电脑上的授权。
// 开关远程也能改；MCP 的令牌和装进 AI 助手只能直接在这台电脑上看、做（见 PagesMcp.jsx）。
import { Bot, ExternalLink, Globe, MessageSquare, Server, Wrench } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link } from '../shared/router.jsx';
import { isTouch } from '../shared/KeyBar.jsx';
import { Button, Card, CopyButton, ErrorNote, Note, QrCode, Skeleton, Switch, useAction, useLoad, useToast } from '../shared/ui.jsx';
import { HelpLink } from '../shared/help.jsx';
import McpCard from './PagesMcp.jsx';

const PAGE_TOOLS = ['list_mappings', 'find_mapping', 'ensure_mapping'];
const SSH_TOOLS = ['list_ssh_hosts', 'ssh_run'];
const FEISHU_TOOLS = [
  'feishu_list_chats',
  'feishu_read_messages',
  'feishu_chat_members',
  'feishu_download_file',
  'feishu_search_users',
  'feishu_send_message',
  'feishu_search_docs',
  'feishu_read_doc',
];

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

// 飞书工具：开关 + 这台电脑上的授权（令牌只在这台电脑上，不同步）
function FeishuGroup({ mcp, on, busy, onToggle }) {
  const { t } = useTranslation();
  const [data, error, reload] = useLoad(() => api.get('/notify'), []);
  const { busy: acting, error: actError, run } = useAction();
  const [now, setNow] = useState(Date.now());
  const u = data?.user;
  const auth = u?.auth;
  const pending = auth?.state === 'pending' && auth.expires_at > now;
  useEffect(() => {
    if (!pending) return undefined;
    const iv = setInterval(() => {
      setNow(Date.now());
      reload();
    }, 2000);
    return () => clearInterval(iv);
  }, [pending]);
  const ready = Boolean(data?.feishu?.bound);
  const start = async () => {
    if (await run(() => api.post('/notify/feishu/user'))) reload();
  };
  const revoke = async () => {
    if (await run(() => api.delete('/notify/feishu/user'))) reload();
  };
  const failed = auth && !pending && auth.state !== 'done' && !u?.authorized ? auth.state : '';
  let body = null;
  if (error) body = <ErrorNote error={error} />;
  else if (!data) body = <Skeleton height={40} />;
  else if (!ready)
    body = (
      <p className="small muted">
        {t('ai.feishu_setup')} <Link to="/settings#notify">{t('ai.feishu_setup_link')}</Link>
      </p>
    );
  else if (u.authorized)
    body = (
      <>
        <div className="between" style={{ gap: 12 }}>
          <span className="small">{t('ai.feishu_authorized', { name: u.name || '' })}</span>
          <Button variant="ghost" className="btn-sm" busy={acting} onClick={revoke}>
            {t('ai.feishu_revoke')}
          </Button>
        </div>
        {u.missing?.length > 0 && <Note kind="warn">{t('ai.feishu_missing', { list: u.missing.join(', ') })}</Note>}
      </>
    );
  else if (pending) {
    const touch = isTouch();
    body = (
      <div className="nt-scan">
        {!touch && <QrCode text={auth.url} size={148} label={t('ai.feishu_auth_qr')} />}
        <div className="stack" style={{ gap: 8 }}>
          <span className="small muted">{t('ai.feishu_auth_waiting')}</span>
          <div className="nt-actions">
            <a className={`btn btn-sm${touch ? ' btn-primary' : ' btn-ghost'}`} href={auth.url} target="_blank" rel="noopener noreferrer">
              {t('notify.link_open')} <ExternalLink size={12} aria-hidden="true" />
            </a>
            <CopyButton text={auth.url} label={t('notify.link_copy')} />
          </div>
        </div>
      </div>
    );
  } else
    body = (
      <>
        {failed && <Note kind="warn">{t(['access_denied', 'expired_token', 'wrong_user'].includes(failed) ? `ai.feishu_auth_${failed}` : 'ai.feishu_auth_failed', { code: failed })}</Note>}
        <p className="small muted">{t('ai.feishu_auth_hint')}</p>
        <div className="nt-actions">
          <Button className="btn-sm" busy={acting} onClick={start}>
            {t('ai.feishu_authorize')}
          </Button>
        </div>
      </>
    );
  return (
    <section className="ai-group">
      <div className="ai-group-head">
        <MessageSquare size={16} aria-hidden="true" />
        <b className="grow">{t('ai.group_feishu')}</b>
        <Switch checked={on} disabled={busy} label={t('ai.feishu_switch')} onChange={onToggle} />
      </div>
      <p className="small muted">{t(on ? 'ai.feishu_on_hint' : 'ai.feishu_off_hint')}</p>
      <ToolList names={FEISHU_TOOLS} on={mcp && on && ready} />
      {on && body}
      <ErrorNote error={actError} />
    </section>
  );
}

function ToolsCard({ version }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [data, error, reload] = useLoad(() => api.get('/ssh/hosts'), [version]);
  const [busy, setBusy] = useState(null);
  if (error) return <ErrorNote error={error} />;
  if (!data) return <Skeleton height={200} />;
  const { mcp, ssh, feishu } = data.ai;
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
          <Switch checked={ssh} disabled={busy === 'ssh'} label={t('ai.ssh_switch')} onChange={(on) => run('ssh', () => api.put('/mcp', { ssh: on }))} />
        </div>
        <p className="small muted">{t(ssh ? 'ai.ssh_on_hint' : 'ai.ssh_off_hint')}</p>
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
      <FeishuGroup mcp={mcp} on={feishu} busy={busy === 'feishu'} onToggle={(on) => run('feishu', () => api.put('/mcp', { feishu: on }))} />
    </Card>
  );
}

export default function AiPage() {
  const { t } = useTranslation();
  // MCP 开关一变，工具卡片跟着刷新
  const [version, setVersion] = useState(0);
  return (
    <>
      <div className="page-head">
        <h1 className="inline">
          <Bot size={24} aria-hidden="true" /> {t('ai.title')} <HelpLink page="ai" />
        </h1>
      </div>
      <p className="muted">{t('ai.intro')}</p>
      <McpCard onChange={() => setVersion((v) => v + 1)} />
      <ToolsCard version={version} />
    </>
  );
}
