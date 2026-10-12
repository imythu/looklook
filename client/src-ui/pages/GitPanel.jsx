// 终端页的 “Git” 标签：终端当前目录所在仓库的分支、改动、提交历史；点文件或提交在右边看对比。
// 能暂存 / 取消暂存、写说明提交、拉取（只快进）/ 推送 / 抓取；不提供丢弃改动这类不可撤销的操作。
// 需要输入密码的远程操作这里做不了（不能交互），提示到终端里运行。手机上列表和对比分两屏。
import { ArrowDown, ArrowLeft, ArrowUp, Check, CloudDownload, GitBranch, GitCommitHorizontal, History, Minus, Plus, RefreshCw, Upload } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { relativeTime } from '../shared/time.js';
import { Button, Empty, useToast } from '../shared/ui.jsx';

const POLL_MS = 5000;

/** 一个文件在列表里的分组：冲突、已暂存、未暂存、未跟踪（同一个文件可以既有暂存的改动又有未暂存的）。 */
export function groupFiles(files) {
  const g = { conflict: [], staged: [], changed: [], untracked: [] };
  for (const f of files ?? []) {
    if (f.conflict) g.conflict.push(f);
    else if (f.index === '?') g.untracked.push(f);
    else {
      if (f.index !== '.') g.staged.push(f);
      if (f.worktree !== '.') g.changed.push(f);
    }
  }
  return g;
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

function FileRow({ f, code, selected, onOpen, action }) {
  const [dir, name] = splitPath(f.path);
  return (
    <li className={`git-file ${selected ? 'selected' : ''}`}>
      <button type="button" className="git-file-main" onClick={onOpen} title={f.from ? `${f.from} → ${f.path}` : f.path}>
        <StatusBadge code={code} />
        <span className="git-file-name ellipsis">
          {name}
          {dir && <span className="git-file-dir"> {dir}</span>}
        </span>
      </button>
      {action}
    </li>
  );
}

function Section({ title, count, actions, children }) {
  if (!count) return null;
  return (
    <section className="git-sec">
      <header className="git-sec-head">
        <span>{title}</span>
        <span className="git-count">{count}</span>
        <span className="grow" />
        {actions}
      </header>
      <ul className="git-files">{children}</ul>
    </section>
  );
}

function DiffView({ view, onBack, narrow }) {
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

/** `id` 终端；`active` 标签可见（不可见时不刷新）。 */
export default function GitPanel({ id, active }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [state, setState] = useState({ loading: true });
  const [mode, setMode] = useState('changes');
  const [view, setView] = useState(null); // { key, title, tag, data?, error? }
  const [message, setMessage] = useState('');
  const [busy, setBusy] = useState(null); // 正在进行的操作
  const [remoteOut, setRemoteOut] = useState(null);
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
  const groups = useMemo(() => groupFiles(data?.files), [data]);

  // 打开对比：改动的文件按当前内容重新取，列表刷新时右边跟着更新
  const openFile = async (f, kind) => {
    const key = `${kind}:${f.path}`;
    const tag = t(`git.sec.${kind}`);
    setView({ key, title: f.path, tag });
    const q = new URLSearchParams({ file: f.path, staged: kind === 'staged' ? '1' : '0', untracked: kind === 'untracked' ? '1' : '0' });
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

  const act = async (name, fn, done) => {
    setBusy(name);
    try {
      const r = await fn();
      if (done) toast(typeof done === 'function' ? done(r) : done);
      return r;
    } catch (e) {
      if (name === 'fetch' || name === 'pull' || name === 'push') setRemoteOut({ error: e });
      else toast(errorText(t, e), 'error');
      return null;
    } finally {
      setBusy(null);
      load();
    }
  };
  const stage = (files, on) => act(on ? 'stage' : 'unstage', () => api.post(`/instances/${id}/git/stage`, { files, on }));
  const commit = async () => {
    const r = await act('commit', () => api.post(`/instances/${id}/git/commit`, { message }), (x) => t('git.committed', { commit: x?.commit ?? '' }));
    if (r) {
      setMessage('');
      setView(null);
    }
  };
  const remote = (action) =>
    act(action, async () => {
      const r = await api.post(`/instances/${id}/git/remote`, { action });
      setRemoteOut({ action, output: r?.output ?? '' });
      return r;
    }, t(`git.done.${action}`));

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

  const nChanges = (data.files ?? []).length;
  const stagedN = groups.staged.length;
  const showList = !narrow || !view;
  const unstagedPaths = [...groups.changed, ...groups.untracked].map((f) => f.path);
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
            <div className="git-remote">
              <button type="button" className="btn btn-plain git-rbtn" disabled={Boolean(busy)} onClick={() => remote('fetch')} title={t('git.fetch_hint')}>
                <RefreshCw size={15} className={busy === 'fetch' ? 'spin' : ''} />
                {t('git.fetch')}
              </button>
              <button type="button" className="btn btn-plain git-rbtn" disabled={Boolean(busy) || !data.upstream} onClick={() => remote('pull')} title={t('git.pull_hint')}>
                <CloudDownload size={15} className={busy === 'pull' ? 'spin' : ''} />
                {t('git.pull')}
                {data.behind > 0 && <span className="git-count">{data.behind}</span>}
              </button>
              <button type="button" className="btn btn-plain git-rbtn" disabled={Boolean(busy)} onClick={() => remote('push')} title={t('git.push_hint')}>
                <Upload size={15} className={busy === 'push' ? 'spin' : ''} />
                {t('git.push')}
                {data.ahead > 0 && <span className="git-count">{data.ahead}</span>}
              </button>
            </div>
            {remoteOut && (
              <div className={`git-remote-out ${remoteOut.error ? 'bad' : ''}`}>
                <pre>{remoteOut.error ? errorText(t, remoteOut.error) : remoteOut.output || t(`git.done.${remoteOut.action}`)}</pre>
                <button type="button" className="link-btn small" onClick={() => setRemoteOut(null)}>{t('action.close')}</button>
              </div>
            )}
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
          </div>

          {mode === 'changes' ? (
            <div className="git-scroll">
              <form
                className="git-commit"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (stagedN && message.trim()) commit();
                }}
              >
                <textarea
                  value={message}
                  onChange={(e) => setMessage(e.target.value)}
                  placeholder={t('git.message_ph', { branch: data.branch ?? 'HEAD' })}
                  rows={2}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
                      e.preventDefault();
                      if (stagedN && message.trim()) commit();
                    }
                  }}
                />
                <div className="git-commit-row">
                  <Button type="submit" icon={Check} busy={busy === 'commit'} disabled={!stagedN || !message.trim() || Boolean(busy)}>
                    {stagedN ? t('git.commit_n', { n: stagedN }) : t('git.commit')}
                  </Button>
                  {unstagedPaths.length > 0 && (
                    <Button variant="ghost" icon={Plus} disabled={Boolean(busy)} onClick={() => stage([], true)}>
                      {t('git.stage_all')}
                    </Button>
                  )}
                </div>
                {!stagedN && nChanges > 0 && <p className="muted small git-tip">{t('git.stage_tip')}</p>}
              </form>

              {nChanges === 0 && (
                <Empty icon={Check} title={t('git.clean_title')}>
                  <p className="muted small">{t('git.clean_body')}</p>
                </Empty>
              )}
              <Section title={t('git.sec.conflict')} count={groups.conflict.length}>
                {groups.conflict.map((f) => (
                  <FileRow key={f.path} f={f} code="U" selected={view?.key === `changed:${f.path}`} onOpen={() => openFile(f, 'changed')} />
                ))}
              </Section>
              <Section
                title={t('git.sec.staged')}
                count={groups.staged.length}
                actions={<button type="button" className="link-btn small" disabled={Boolean(busy)} onClick={() => stage(groups.staged.map((f) => f.path), false)}>{t('git.unstage_all')}</button>}
              >
                {groups.staged.map((f) => (
                  <FileRow
                    key={f.path}
                    f={f}
                    code={f.index}
                    selected={view?.key === `staged:${f.path}`}
                    onOpen={() => openFile(f, 'staged')}
                    action={
                      <button type="button" className="btn btn-plain btn-icon git-fact" disabled={Boolean(busy)} onClick={() => stage([f.path], false)} aria-label={`${t('git.unstage')} ${f.path}`} title={t('git.unstage')}>
                        <Minus size={15} />
                      </button>
                    }
                  />
                ))}
              </Section>
              <Section title={t('git.sec.changed')} count={groups.changed.length}>
                {groups.changed.map((f) => (
                  <FileRow
                    key={f.path}
                    f={f}
                    code={f.worktree}
                    selected={view?.key === `changed:${f.path}`}
                    onOpen={() => openFile(f, 'changed')}
                    action={
                      <button type="button" className="btn btn-plain btn-icon git-fact" disabled={Boolean(busy)} onClick={() => stage([f.path], true)} aria-label={`${t('git.stage')} ${f.path}`} title={t('git.stage')}>
                        <Plus size={15} />
                      </button>
                    }
                  />
                ))}
              </Section>
              <Section title={t('git.sec.untracked')} count={groups.untracked.length}>
                {groups.untracked.map((f) => (
                  <FileRow
                    key={f.path}
                    f={f}
                    code="?"
                    selected={view?.key === `untracked:${f.path}`}
                    onOpen={() => openFile(f, 'untracked')}
                    action={
                      <button type="button" className="btn btn-plain btn-icon git-fact" disabled={Boolean(busy)} onClick={() => stage([f.path], true)} aria-label={`${t('git.stage')} ${f.path}`} title={t('git.stage')}>
                        <Plus size={15} />
                      </button>
                    }
                  />
                ))}
              </Section>
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
      {(!narrow || view) && <DiffView view={view} narrow={narrow} onBack={() => setView(null)} />}
    </div>
  );
}
