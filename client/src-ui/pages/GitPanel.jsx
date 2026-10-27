// 终端页的 “Git” 标签（只读）：终端当前目录所在仓库的分支、改动、提交历史；点文件或提交在右边看对比。
// 暂存、提交、拉取、推送交给终端（AI 编程助手或 git 命令）。手机上列表和对比分两屏。
// 唯一会改文件的是“检查点”：每轮开始自动拍的快照，可以看之后改了什么、回到那一轮之前（docs/PLAN_2026-10-27.md §3.3）。
import { ArrowDown, ArrowLeft, ArrowUp, Check, GitBranch, GitCommitHorizontal, History, RefreshCw, RotateCcw } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { relativeTime } from '../shared/time.js';
import { Button, Empty, useConfirm, useToast } from '../shared/ui.jsx';

const POLL_MS = 5000;

/** 改动的文件（相对上次提交，暂存与否都算）：有冲突的排前面。code 是列表里显示的状态字母。 */
export function changedFiles(files) {
  const code = (f) => (f.conflict ? 'U' : f.index === '?' ? '?' : f.index !== '.' ? f.index : f.worktree);
  const all = (files ?? []).map((f) => ({ ...f, code: code(f) }));
  return [...all.filter((f) => f.conflict), ...all.filter((f) => !f.conflict)];
}

/** 统一格式的对比 → 行：{kind: add|del|ctx|hunk|meta, text, old, new}。 */
export function parseDiff(text) {
  const out = [];
  let o = 0;
  let n = 0;
  let inHunk = false;
  for (const line of (text ?? '').split('\n')) {
    const m = line.match(/^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/);
    if (m) {
      o = Number(m[1]);
      n = Number(m[2]);
      inHunk = true;
      out.push({ kind: 'hunk', text: line });
    } else if (inHunk && line.startsWith('+')) out.push({ kind: 'add', text: line.slice(1), new: n++ });
    else if (inHunk && line.startsWith('-')) out.push({ kind: 'del', text: line.slice(1), old: o++ });
    else if (inHunk && line.startsWith(' ')) out.push({ kind: 'ctx', text: line.slice(1), old: o++, new: n++ });
    else if (inHunk && line.startsWith('\\')) out.push({ kind: 'meta', text: line });
    else {
      if (line.startsWith('diff --git') || line.startsWith('commit ')) inHunk = false;
      if (line !== '' || out.length) out.push({ kind: 'meta', text: line });
    }
  }
  while (out.length && out[out.length - 1].kind === 'meta' && !out[out.length - 1].text) out.pop();
  return out;
}

const STATUS_LABEL = { M: 'modified', A: 'added', D: 'deleted', R: 'renamed', C: 'copied', T: 'typechange', U: 'conflict', '?': 'untracked' };

function StatusBadge({ code }) {
  const { t } = useTranslation();
  const c = code === '.' ? 'M' : code;
  return (
    <span className={`git-st git-st-${STATUS_LABEL[c] ?? 'modified'}`} title={t(`git.st.${STATUS_LABEL[c] ?? 'modified'}`)}>
      {c === '?' ? 'U' : c}
    </span>
  );
}

function splitPath(p) {
  const i = p.lastIndexOf('/');
  return i < 0 ? ['', p] : [p.slice(0, i + 1), p.slice(i + 1)];
}

function FileRow({ f, selected, onOpen }) {
  const [dir, name] = splitPath(f.path);
  return (
    <li className={`git-file ${selected ? 'selected' : ''}`}>
      <button type="button" className="git-file-main" onClick={onOpen} title={f.from ? `${f.from} → ${f.path}` : f.path}>
        <StatusBadge code={f.code} />
        <span className="git-file-name ellipsis">
          {name}
          {dir && <span className="git-file-dir"> {dir}</span>}
        </span>
      </button>
    </li>
  );
}

function DiffView({ view, onBack, narrow, onRestore, restoring }) {
  const { t } = useTranslation();
  const lines = useMemo(() => parseDiff(view?.data?.text), [view?.data?.text]);
  if (!view) {
    return (
      <div className="git-diff git-diff-empty">
        <Empty icon={GitCommitHorizontal} title={t('git.pick_title')}>
          <p className="muted small">{t('git.pick_body')}</p>
        </Empty>
      </div>
    );
  }
  const { data, error } = view;
  return (
    <div className="git-diff">
      <div className="git-diff-head">
        {narrow && (
          <button type="button" className="btn btn-plain btn-icon" onClick={onBack} aria-label={t('git.back')} title={t('git.back')}>
            <ArrowLeft size={18} />
          </button>
        )}
        <b className="ellipsis grow mono" title={view.title}>{view.title}</b>
        {view.tag && <span className="badge">{view.tag}</span>}
        {view.cp && data && (
          <Button variant="secondary" icon={RotateCcw} busy={restoring} disabled={!data.files?.length} onClick={() => onRestore(view.cp, data)}>
            {t('git.cp_restore')}
          </Button>
        )}
      </div>
      <div className="git-diff-body">
        {error ? (
          <pre className="git-error">{errorText(t, error)}</pre>
        ) : !data ? (
          <p className="muted small fb-pad">{t('fb.loading')}</p>
        ) : data.binary && lines.every((l) => l.kind === 'meta') ? (
          <p className="muted small fb-pad">{t('git.binary')}</p>
        ) : lines.length === 0 ? (
          <p className="muted small fb-pad">{t('git.no_diff')}</p>
        ) : (
          <>
            <table className="git-lines">
              <tbody>
                {lines.map((l, i) => (
                  <tr key={i} className={`gl-${l.kind}`}>
                    <td className="gl-no">{l.old ?? ''}</td>
                    <td className="gl-no">{l.new ?? ''}</td>
                    <td className="gl-sign">{l.kind === 'add' ? '+' : l.kind === 'del' ? '−' : ''}</td>
                    <td className="gl-text">{l.text || ' '}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            {data.truncated && <p className="muted small fb-pad">{t('git.truncated')}</p>}
          </>
        )}
      </div>
    </div>
  );
}

/** 检查点列表：新的在前，每个写着之后改了几个文件 */
function Checkpoints({ id, active, view, onOpen, reloadKey }) {
  const { t } = useTranslation();
  const [state, setState] = useState({});
  useEffect(() => {
    if (!active) return undefined;
    let live = true;
    const load = () =>
      api.get(`/instances/${id}/checkpoints`).then(
        (data) => live && setState({ data }),
        (error) => live && setState((s) => ({ data: s.data, error })),
      );
    load();
    const iv = setInterval(() => !document.hidden && load(), POLL_MS * 2);
    return () => {
      live = false;
      clearInterval(iv);
    };
  }, [id, active, reloadKey]);
  const items = state.data?.items ?? [];
  const now = Date.now();
  return (
    <div className="git-scroll">
      <p className="muted small fb-pad">{t('git.cp_lead')}</p>
      {state.error && <pre className="git-error">{errorText(t, state.error)}</pre>}
      {state.data && items.length === 0 && <p className="muted small fb-pad">{t('git.cp_none')}</p>}
      <ol className="git-log" style={{ margin: 0, padding: 0 }}>
        {items.map((c) => (
          <li key={c.id}>
            <button type="button" className={`git-commit-item ${view?.key === `cp:${c.id}` ? 'selected' : ''}`} onClick={() => onOpen(c)} disabled={c.missing}>
              <span className={`git-node ${c.before_restore ? '' : 'head'}`} aria-hidden="true" />
              <span className="git-commit-text">
                <span className="ellipsis git-subject">{cpTitle(t, c)}</span>
                <span className="small muted ellipsis">
                  {relativeTime(t, new Date(c.id).toISOString(), now)} · {c.missing ? t('git.cp_missing') : c.changed ? t('git.cp_changed', { count: c.changed }) : t('git.cp_same')}
                </span>
              </span>
            </button>
          </li>
        ))}
      </ol>
    </div>
  );
}

function cpTitle(t, c) {
  if (c.before_restore) return t('git.cp_before_restore', { label: c.label || t('git.cp_unnamed') });
  return c.label || t('git.cp_unnamed');
}

/** `id` 终端；`active` 标签可见（不可见时不刷新）。 */
export default function GitPanel({ id, active }) {
  const { t } = useTranslation();
  const confirm = useConfirm();
  const toast = useToast();
  const [restoring, setRestoring] = useState(false);
  const [cpReload, setCpReload] = useState(0);
  const [state, setState] = useState({ loading: true });
  const [mode, setMode] = useState('changes');
  const [view, setView] = useState(null); // { key, title, tag, data?, error? }
  const [narrow, setNarrow] = useState(() => window.matchMedia('(max-width: 760px)').matches);
  const tick = useRef(0);

  useEffect(() => {
    const mq = window.matchMedia('(max-width: 760px)');
    const on = () => setNarrow(mq.matches);
    mq.addEventListener('change', on);
    return () => mq.removeEventListener('change', on);
  }, []);

  const load = async () => {
    const my = ++tick.current;
    try {
      const data = await api.get(`/instances/${id}/git`);
      if (my === tick.current) setState({ data });
    } catch (error) {
      if (my === tick.current) setState((s) => ({ data: s.data, error }));
    }
  };
  useEffect(() => {
    if (!active) return undefined;
    load();
    const iv = setInterval(() => !document.hidden && load(), POLL_MS);
    return () => clearInterval(iv);
  }, [id, active]);

  const data = state.data;
  const files = useMemo(() => changedFiles(data?.files), [data]);

  const openFile = async (f) => {
    const key = `file:${f.path}`;
    setView({ key, title: f.path });
    const q = new URLSearchParams({ file: f.path, untracked: f.code === '?' ? '1' : '0' });
    try {
      const d = await api.get(`/instances/${id}/git/diff?${q}`);
      setView((v) => (v?.key === key ? { ...v, data: d } : v));
    } catch (error) {
      setView((v) => (v?.key === key ? { ...v, error } : v));
    }
  };
  const openCommit = async (c) => {
    const key = `commit:${c.hash}`;
    setView({ key, title: c.subject, tag: c.short });
    try {
      const d = await api.get(`/instances/${id}/git/show?commit=${c.hash}`);
      setView((v) => (v?.key === key ? { ...v, data: d } : v));
    } catch (error) {
      setView((v) => (v?.key === key ? { ...v, error } : v));
    }
  };

  const openCheckpoint = async (c) => {
    const key = `cp:${c.id}`;
    setView({ key, title: cpTitle(t, c), tag: relativeTime(t, new Date(c.id).toISOString(), Date.now()), cp: c });
    try {
      const d = await api.get(`/instances/${id}/checkpoints/${c.id}/diff`);
      setView((v) => (v?.key === key ? { ...v, data: d } : v));
    } catch (error) {
      setView((v) => (v?.key === key ? { ...v, error } : v));
    }
  };
  const restore = async (c, d) => {
    const added = d.files.filter((f) => f.status === 'A').length;
    const body = t('git.cp_restore_body', { count: d.files.length }) + (added ? ` ${t('git.cp_restore_added', { count: added })}` : '');
    if (!(await confirm({ title: t('git.cp_restore_title'), body, confirm: t('git.cp_restore'), danger: true }))) return;
    setRestoring(true);
    try {
      await api.post(`/instances/${id}/checkpoints/${c.id}/restore`, {});
      toast(t('git.cp_restored'));
      setView(null);
      setCpReload((n) => n + 1);
      load();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setRestoring(false);
    }
  };

  if (state.error && !data) {
    return (
      <div className="git git-center">
        <pre className="git-error">{errorText(t, state.error)}</pre>
        <Button variant="ghost" icon={RefreshCw} onClick={load}>{t('git.retry')}</Button>
      </div>
    );
  }
  if (!data) return <div className="git git-center"><p className="muted small">{t('fb.loading')}</p></div>;
  if (!data.repo) {
    return (
      <div className="git git-center">
        <Empty icon={GitBranch} title={t('git.not_repo_title')}>
          <p className="muted small">{t('git.not_repo_body')}</p>
          <p className="mono small"><code>git init</code></p>
        </Empty>
      </div>
    );
  }

  const nChanges = files.length;
  const showList = !narrow || !view;
  const now = Date.now();
  return (
    <div className={`git ${narrow ? 'git-narrow' : ''}`}>
      {showList && (
        <aside className="git-side">
          <div className="git-branch">
            <div className="git-branch-line">
              <GitBranch size={16} aria-hidden="true" className="git-branch-ico" />
              <b className="ellipsis" title={data.root}>{data.branch ?? t('git.detached', { head: data.head ?? '' })}</b>
              {data.upstream && <span className="muted small ellipsis">→ {data.upstream}</span>}
              <span className="grow" />
              {data.ahead > 0 && <span className="git-ab" title={t('git.ahead', { n: data.ahead })}><ArrowUp size={12} />{data.ahead}</span>}
              {data.behind > 0 && <span className="git-ab git-ab-behind" title={t('git.behind', { n: data.behind })}><ArrowDown size={12} />{data.behind}</span>}
            </div>
          </div>

          <div className="seg seg-sm git-modes" role="tablist">
            <button type="button" role="tab" aria-pressed={mode === 'changes'} onClick={() => setMode('changes')}>
              {t('git.changes')}
              {nChanges > 0 && <span className="git-count">{nChanges}</span>}
            </button>
            <button type="button" role="tab" aria-pressed={mode === 'history'} onClick={() => setMode('history')}>
              <History size={14} aria-hidden="true" />
              {t('git.history')}
            </button>
            <button type="button" role="tab" aria-pressed={mode === 'checkpoints'} onClick={() => setMode('checkpoints')}>
              <RotateCcw size={14} aria-hidden="true" />
              {t('git.checkpoints')}
            </button>
          </div>

          {mode === 'checkpoints' ? (
            <Checkpoints id={id} active={active} view={view} onOpen={openCheckpoint} reloadKey={cpReload} />
          ) : mode === 'changes' ? (
            <div className="git-scroll">
              {nChanges === 0 && (
                <Empty icon={Check} title={t('git.clean_title')}>
                  <p className="muted small">{t('git.clean_body')}</p>
                </Empty>
              )}
              <ul className="git-files">
                {files.map((f) => (
                  <FileRow key={f.path} f={f} selected={view?.key === `file:${f.path}`} onOpen={() => openFile(f)} />
                ))}
              </ul>
              {data.truncated && <p className="muted small fb-pad">{t('git.too_many')}</p>}
            </div>
          ) : (
            <ol className="git-scroll git-log">
              {(data.commits ?? []).length === 0 && <p className="muted small fb-pad">{t('git.no_commits')}</p>}
              {(data.commits ?? []).map((c, i) => (
                <li key={c.hash}>
                  <button type="button" className={`git-commit-item ${view?.key === `commit:${c.hash}` ? 'selected' : ''}`} onClick={() => openCommit(c)}>
                    <span className={`git-node ${i === 0 ? 'head' : ''}`} aria-hidden="true" />
                    <span className="git-commit-text">
                      <span className="ellipsis git-subject">{c.subject}</span>
                      <span className="small muted ellipsis">
                        <span className="mono">{c.short}</span> · {c.author} · {relativeTime(t, new Date(c.time * 1000).toISOString(), now)}
                      </span>
                      {c.refs && (
                        <span className="git-refs">
                          {c.refs.split(', ').map((r) => (
                            <span key={r} className={`git-ref ${r.startsWith('HEAD') ? 'head' : r.startsWith('tag:') ? 'tag' : ''}`}>{r.replace('HEAD -> ', '')}</span>
                          ))}
                        </span>
                      )}
                    </span>
                  </button>
                </li>
              ))}
            </ol>
          )}
        </aside>
      )}
      {(!narrow || view) && <DiffView view={view} narrow={narrow} onBack={() => setView(null)} onRestore={restore} restoring={restoring} />}
    </div>
  );
}
