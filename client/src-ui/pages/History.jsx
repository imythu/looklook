// 对话记录：只读地翻看这台电脑上 Codex、DSH、Claude Code（以及登记的目录）里的历史对话（读的是各工具自己的
// 会话文件，见客户端 src/convo）。选哪个工具就只列哪个工具的对话。按 Markdown 排版，工具调用默认折叠。
// 看完可以一键在终端里接着聊（Codex：`codex resume`，Claude Code：`claude --resume`），或迁移到另一个工具。
// 不驱动 AI、不改任何会话：要接着聊就回到终端，用工具自己的界面。
import { ArrowLeft, ArrowRight, Bot, ChevronRight, Fish, History as HistoryIcon, Play, Sparkles, SquareTerminal, Wrench } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi, rememberTerminals } from '../shared/api.js';
import { useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import Markdown from '../shared/Markdown.jsx';
import { useRouter } from '../shared/router.jsx';
import { formatTime, relativeTime } from '../shared/time.js';
import { Button, Empty, ErrorNote, Skeleton, useLoad, useToast } from '../shared/ui.jsx';
import ConvoDialog, { sourceName } from './Convo.jsx';
import { useNarrow } from './TerminalView.jsx';

const KIND_ICON = { codex: Bot, dsh: Fish, claude: Sparkles, opencode: SquareTerminal };
// 能在终端里接着聊的（只有默认目录：登记的其他目录终端里没有对应的环境变量）→ 用哪种终端
const RESUME_LAUNCH = { codex: 'codex', claude: 'claude', dsh: 'dsh_tui' };

function shortPath(p) {
  if (!p) return '';
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join('/')}` : p;
}

/** 一次工具调用：默认折叠，展开看参数和结果。 */
function ToolCall({ call, result }) {
  const { t } = useTranslation();
  return (
    <details className={`hist-tool ${result?.is_error ? 'err' : ''}`}>
      <summary>
        <ChevronRight size={14} aria-hidden="true" className="hist-chev" />
        <Wrench size={13} aria-hidden="true" />
        <span className="mono">{call.name}</span>
        {result?.is_error && <span className="badge badge-danger">{t('history.tool_error')}</span>}
        {!result && <span className="muted small">{t('history.tool_no_result')}</span>}
      </summary>
      <div className="hist-tool-body">
        <p className="small muted">{t('history.arguments')}</p>
        <pre className="mono">{call.arguments || '{}'}</pre>
        {call.clipped && <p className="small muted">{t('history.clipped')}</p>}
        {result && (
          <>
            <p className="small muted">{t('history.result')}</p>
            <pre className="mono">{result.output || t('history.empty_output')}</pre>
            {result.clipped && <p className="small muted">{t('history.clipped')}</p>}
          </>
        )}
      </div>
    </details>
  );
}

function Transcript({ source, id, client, onBack, onResumed, onMigrate }) {
  const from = source.id;
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(null);
  const [data, error] = useLoad(() => client.get(`/convo/read?from=${from}&id=${encodeURIComponent(id)}`), [from, id]);
  // 结果按调用 id 放进调用里一起显示
  const { rows, results } = useMemo(() => {
    const results = new Map();
    const rows = [];
    for (const it of data?.items ?? []) {
      if (it.kind === 'tool_result') results.set(it.call_id, it);
      else rows.push(it);
    }
    return { rows, results };
  }, [data]);
  const resumable = Boolean(RESUME_LAUNCH[source.kind]) && source.id === source.kind;
  // DSH 在终端里接着聊要带 tui 的 DSH（桌面版自带的没有）
  const [tools] = useLoad(() => (source.kind === 'dsh' ? client.get('/tools').catch(() => null) : Promise.resolve(null)), [source.kind]);
  const noTui = source.kind === 'dsh' && tools && (!tools.tools?.dsh || tools.dsh_tui === false);
  const run = async (key, fn) => {
    setBusy(key);
    try {
      await fn();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(null);
    }
  };
  // 开一个接着这个会话聊的终端（`codex resume <id>` / `claude --resume <id>` / `dsh tui --resume <id>`）
  const resume = () =>
    run('resume', async () => {
      const name = `↪ ${data?.title ?? sourceName(t, source)}`.slice(0, 40);
      const r = await client.post('/instances', { name, launch: RESUME_LAUNCH[source.kind], workdir: data?.cwd ?? '', resume: id, start: true });
      onResumed(r);
    });
  return (
    <div className="hist-reader">
      <header className="hist-head">
        {onBack && (
          <button type="button" className="btn btn-plain btn-icon" onClick={onBack} aria-label={t('history.back')}>
            <ArrowLeft size={18} />
          </button>
        )}
        <span className="grow" style={{ minWidth: 0 }}>
          <b className="ellipsis" style={{ display: 'block' }}>
            {data?.title ?? id}
          </b>
          {data && (
            <span className="small muted ellipsis" style={{ display: 'block' }}>
              {data.cwd && <span className="mono">{data.cwd}</span>}
              {data.model && ` · ${data.model}`}
              {` · ${t('history.count', { n: data.total })}`}
            </span>
          )}
        </span>
        <span className="hist-actions">
          {resumable && (
            <Button icon={Play} busy={busy === 'resume'} disabled={!data || Boolean(busy) || noTui} onClick={resume} title={noTui ? t('history.dsh_no_tui') : t('history.resume_hint')}>
              <span className="hide-narrow">{t('history.resume')}</span>
            </Button>
          )}
          <Button variant="secondary" icon={ArrowRight} disabled={!data || Boolean(busy)} onClick={() => onMigrate({ id, title: data?.title, cwd: data?.cwd, updated_ms: Date.now() })} title={t('history.migrate_any_hint')}>
            <span className="hide-narrow">{t('history.migrate_any')}</span>
          </Button>
        </span>
      </header>
      <ErrorNote error={error} />
      {noTui && <p className="small muted" style={{ marginTop: 0 }}>{t('history.dsh_no_tui')}</p>}
      {!data && !error && <Skeleton height={240} />}
      {data && (
        <div className="hist-messages">
          {data.skipped > 0 && <p className="small muted hist-skipped">{t('history.skipped', { n: data.skipped })}</p>}
          {rows.map((it, i) =>
            it.kind === 'tool_call' ? (
              <ToolCall key={`c${i}`} call={it} result={results.get(it.id)} />
            ) : (
              <article key={`m${i}`} className={`hist-msg hist-${it.kind}`}>
                <span className="small muted hist-time" title={formatTime(new Date(it.time_ms).toISOString())}>
                  {t(`history.${it.kind}`)} · {relativeTime(t, new Date(it.time_ms).toISOString())}
                </span>
                {it.kind === 'assistant' ? <Markdown text={it.text} /> : <div className="hist-user-text">{it.text}</div>}
              </article>
            ),
          )}
        </div>
      )}
    </div>
  );
}

export default function History() {
  const { t } = useTranslation();
  const devices = useDevices();
  const narrow = useNarrow('(max-width: 899px)');
  const { navigate } = useRouter();
  const dev = devices.multi ? devices.selected : null;
  const client = deviceApi(dev);
  const [from, setFrom] = useState('codex');
  const [query, setQuery] = useState('');
  const [picked, setPicked] = useState(null);
  const [migrate, setMigrate] = useState(null);
  const [sources, sourcesError] = useLoad(() => client.get('/convo/harnesses'), [dev]);
  const readable = (sources?.items ?? []).filter((s) => s.can_export && s.exists);
  const source = readable.find((s) => s.id === from) ?? { id: from, kind: from };
  useEffect(() => {
    if (readable.length && !readable.some((s) => s.id === from)) setFrom(readable[0].id);
  }, [sources]);
  // 每个结果都记下是哪个来源的：切换来源时上一个来源的列表不会混进来（新列表到之前显示“正在读取”）
  const [loaded, error] = useLoad(() => client.get(`/convo/sessions?from=${encodeURIComponent(from)}`).then((r) => ({ ...r, from })), [from, dev]);
  const list = loaded?.from === from ? loaded : null;
  useEffect(() => setPicked(null), [from, dev]);
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  const items = (list?.items ?? []).filter((s) => words.every((w) => `${s.title ?? ''} ${s.cwd ?? ''} ${s.id}`.toLowerCase().includes(w)));
  const resumed = (inst) => {
    if (dev && inst?.id) rememberTerminals(dev, [inst]);
    navigate(`/t/${inst.id}`);
  };
  const showList = !narrow || !picked;
  return (
    <>
      <div className="page-head">
        <h1>{t('history.title')}</h1>
        <div className="seg" role="group" aria-label={t('history.tool')}>
          {readable.map((s) => {
            const Icon = KIND_ICON[s.kind] ?? Bot;
            return (
              <button key={s.id} type="button" aria-pressed={from === s.id} onClick={() => setFrom(s.id)} title={s.home}>
                <Icon size={15} aria-hidden="true" /> {sourceName(t, s)}
              </button>
            );
          })}
        </div>
      </div>
      <p className="muted">{t('history.intro')}</p>
      <ErrorNote error={sourcesError ?? (loaded === null || loaded?.from === from ? error : null)} />
      <div className={`hist ${picked ? 'has-pick' : ''}`}>
        {showList && (
          <aside className="hist-list" aria-label={t('history.list')}>
            <input type="search" className="convo-search" value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t('convo.search')} aria-label={t('convo.search')} spellCheck={false} />
            <div className="convo-list hist-items">
              {!list && !error ? (
                <Skeleton height={160} />
              ) : items.length === 0 ? (
                <p className="muted small" style={{ padding: 8 }}>
                  {t(query ? 'convo.no_match' : 'convo.empty', { tool: sourceName(t, source) })}
                </p>
              ) : (
                items.map((s) => (
                  <button key={s.id} type="button" className={`convo-item ${picked === s.id ? 'on' : ''}`} aria-current={picked === s.id || undefined} onClick={() => setPicked(s.id)} title={`${s.title ?? s.id}\n${s.cwd ?? ''}`}>
                    <span className="ellipsis">{s.title ?? s.id}</span>
                    <span className="small muted ellipsis">
                      {s.cwd ? <span className="mono">{shortPath(s.cwd)}</span> : null}
                      {s.cwd ? ' · ' : ''}
                      {relativeTime(t, new Date(s.updated_ms).toISOString())}
                    </span>
                  </button>
                ))
              )}
            </div>
          </aside>
        )}
        {picked ? (
          <Transcript key={`${from}:${picked}`} source={source} id={picked} client={client} onBack={narrow ? () => setPicked(null) : null} onResumed={resumed} onMigrate={(session) => setMigrate({ from, session })} />
        ) : (
          !narrow && (
            <div className="hist-reader hist-empty">
              <Empty icon={HistoryIcon} title={t('history.pick_title')}>
                <p>{t('history.pick_body')}</p>
              </Empty>
            </div>
          )
        )}
      </div>
      <ConvoDialog open={Boolean(migrate)} preset={migrate} onClose={() => setMigrate(null)} onOpened={resumed} />
    </>
  );
}

