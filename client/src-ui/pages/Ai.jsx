// AI 助手：MCP 接口（令牌、一键装进 Codex / Claude Code / OpenCode / DSH）和它给 AI 助手的全部工具，
// 一处看清 AI 能做什么：本机网页的工具，SSH 工具的总开关和每台主机“允许 AI 使用”的快捷开关，
// 以及飞书：引导装飞书官方的 lark-cli，AI 助手用它以你本人的身份读、发飞书。
// 开关远程也能改；MCP 的令牌和装进 AI 助手只能直接在这台电脑上看、做（见 PagesMcp.jsx）。
import { Bot, Globe, MessageSquare, Server, Wrench } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { Link } from '../shared/router.jsx';
import { Button, Card, CopyButton, ErrorNote, Skeleton, Switch, useAction, useLoad, useToast } from '../shared/ui.jsx';
import { HelpLink } from '../shared/help.jsx';
import McpCard from './PagesMcp.jsx';

const PAGE_TOOLS = ['list_mappings', 'find_mapping', 'ensure_mapping'];
const SSH_TOOLS = ['list_ssh_hosts', 'ssh_run'];
// 飞书 CLI：装（和 src/tools.rs 里一键安装的一样）、第一次登录
const LARK_INSTALL = 'npx @larksuite/cli@latest install';
const LARK_LOGIN = ['lark-cli config init --new', 'lark-cli auth login --recommend'];

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

// 飞书：不给看看的应用申请用户权限，引导装飞书官方的 lark-cli（带技能，Claude Code、Codex 装好就会用），
// 以你本人的身份读群聊和私聊、找人、读写云文档、发消息都交给它
function LarkGroup() {
  const { t } = useTranslation();
  const [tools, error, reload] = useLoad(() => api.get('/tools'), []);
  const { busy, error: actError, run } = useAction();
  const [job, setJob] = useState(null);
  const running = job?.state === 'running';
  useEffect(() => {
    if (!running) return undefined;
    const iv = setInterval(async () => {
      const j = await api.get('/tools/install/lark-cli').catch(() => null);
      if (j) setJob(j);
      if (j?.state !== 'running') reload();
    }, 1500);
    return () => clearInterval(iv);
  }, [running]);
  const install = async () => {
    const j = await run(() => api.post('/tools/install', { tool: 'lark-cli', mirror: 'npmmirror' }));
    if (j) setJob(j);
  };
  const path = tools?.tools?.['lark-cli'];
  let body;
  if (error) body = <ErrorNote error={error} />;
  else if (!tools) body = <Skeleton height={40} />;
  else if (path)
    body = (
      <>
        <p className="small">{t('ai.lark_installed')}</p>
        <p className="small muted">{t('ai.lark_login_hint')}</p>
        {LARK_LOGIN.map((c) => (
          <div key={c} className="between" style={{ gap: 8 }}>
            <code className="mono small ellipsis">{c}</code>
            <CopyButton text={c} />
          </div>
        ))}
      </>
    );
  else
    body = (
      <>
        <div className="nt-actions">
          <Button className="btn-sm" busy={busy || running} disabled={!tools.tools?.npm} onClick={install}>
            {t('ai.lark_install')}
          </Button>
          <CopyButton text={LARK_INSTALL} label={t('ai.lark_copy')} />
        </div>
        {!tools.tools?.npm && <p className="small muted">{t('ai.lark_need_node')}</p>}
      </>
    );
  return (
    <section className="ai-group">
      <div className="ai-group-head">
        <MessageSquare size={16} aria-hidden="true" />
        <b className="grow">{t('ai.group_feishu')}</b>
        {tools && <span className={`badge ${path ? 'badge-ok' : ''}`}>{t(path ? 'ai.lark_badge_ok' : 'ai.lark_badge_none')}</span>}
      </div>
      <p className="small muted">{t('ai.lark_intro')}</p>
      {body}
      {job && job.state !== 'ok' && <pre className="mono small ai-job">{job.output.slice(-2000)}</pre>}
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
      <LarkGroup />
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
