// 终端页的 “Git” 标签：终端当前目录所在仓库的分支、改动、提交历史；点文件或提交在右边看对比。
// 改动分成“这段对话改的”和“其他未提交”，勾上要提交的，左下角写说明提交（可以顺便推送）。
// 暂存区、拉取、合并交给终端（AI 编程助手或 git 命令）。手机上列表和对比分两屏。
import { ArrowDown, ArrowLeft, ArrowUp, Check, GitBranch, GitCommitHorizontal, History, RefreshCw } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { errorText } from '../shared/i18n.js';
import { relativeTime } from '../shared/time.js';
import { Button, Empty, useToast } from '../shared/ui.jsx';

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

function FileRow({ f, selected, onOpen, picked, onPick, stat }) {
  const { t } = useTranslation();
  const [dir, name] = splitPath(f.path);
  return (
    <li className={`git-file ${selected ? 'selected' : ''}`}>
      <input type="checkbox" className="git-pick" checked={picked} onChange={(e) => onPick(e.target.checked)} aria-label={t('git.pick_file', { file: f.path })} />
      <button type="button" className="git-file-main" onClick={onOpen} title={f.from ? `${f.from} → ${f.path}` : f.path}>
        <StatusBadge code={f.code} />
        <span className="git-file-name ellipsis">
          {name}
          {dir && <span className="git-file-dir"> {dir}</span>}
        </span>
        {stat && (stat.add > 0 || stat.del > 0) && (
          <span className="git-file-stat">
            <i className="add">+{stat.add}</i> <i className="del">−{stat.del}</i>
          </span>
        )}
      </button>
    </li>
  );
}

/** 对话里改过的文件（绝对路径）→ 相对仓库根目录的路径（和 git 状态里的对上），不在这个仓库里的去掉 */
export function relToRepo(root, abs) {
  const norm = (p) => p.replace(/\\/g, '/').replace(/\/+$/, '');
  const r = norm(root ?? '');
  const a = norm(abs ?? '');
  if (!r || !a.toLowerCase().startsWith(`${r.toLowerCase()}/`)) return null;
  return a.slice(r.length + 1);
}

/** 左下角的提交：说明、提交选中的文件、提交并推送 */
function CommitBox({ id, picked, onDone }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [msg, setMsg] = useState('');
  const [busy, setBusy] = useState(false);
  const go = async (push) => {
    if (!msg.trim() || !picked.length) return;
    setBusy(true);
    try {
      const r = await api.post(`/instances/${id}/git/commit`, { files: picked, message: msg, push });
      setMsg('');
      if (r.push_error) toast(t('git.push_failed', { hash: r.hash, detail: r.push_error }), 'error');
      else toast(t(r.pushed ? 'git.pushed' : 'git.committed', { hash: r.hash }));
      onDone();
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="git-commit"
      onSubmit={(e) => {
        e.preventDefault();
        go(false);
      }}
    >
      <label className="sec" htmlFor={`git-msg-${id}`}>{t('git.commit_msg')}</label>
      <textarea
        id={`git-msg-${id}`}
        rows={3}
        value={msg}
        onChange={(e) => setMsg(e.target.value)}
        placeholder={t('git.commit_ph')}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            go(false);
          }
        }}
      />
      <div className="git-commit-acts">
        <Button type="submit" busy={busy} disabled={!msg.trim() || !picked.length} className="grow">
          {t('git.commit_n', { n: picked.length })}
        </Button>
        <Button variant="secondary" icon={ArrowUp} busy={busy} disabled={!msg.trim() || !picked.length} onClick={() => go(true)} aria-label={t('git.commit_push')} title={t('git.commit_push')} />
      </div>
    </form>
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
      // 这段对话改过的文件（没有对话界面的终端没有，忽略）
      const [data, conv] = await Promise.all([api.get(`/instances/${id}/git`), api.get(`/instances/${id}/chat/changes`).catch(() => null)]);
      if (my === tick.current) setState({ data, conv });
    } catch (error) {
      if (my === tick.current) setState((s) => ({ ...s, data: s.data, error }));
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
  // 对话改过的（相对仓库根目录 → 增删行数），只留现在还没提交的
  const conv = useMemo(() => {
    const m = new Map();
    for (const c of state.conv?.files ?? []) {
      const rel = relToRepo(data?.root, c.path);
      if (rel && files.some((f) => f.path === rel)) m.set(rel, c);
    }
    return m;
  }, [state.conv, data, files]);
  // 勾选要提交的：没动过时默认勾上这段对话改的（没有就全部），动过之后照用户的
  const [picks, setPicks] = useState(null);
  const picked = files.map((f) => f.path).filter((p) => (picks ? picks.has(p) : conv.size ? conv.has(p) : true));
  const pick = (p, on) => {
    const next = new Set(picked);
    if (on) next.add(p);
    else next.delete(p);
    setPicks(next);
  };
  const row = (f) => <FileRow key={f.path} f={f} selected={view?.key === `file:${f.path}`} onOpen={() => openFile(f)} picked={picked.includes(f.path)} onPick={(on) => pick(f.path, on)} stat={conv.get(f.path)} />;
  const mine = files.filter((f) => conv.has(f.path));
  const others = files.filter((f) => !conv.has(f.path));

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
          </div>

          {mode === 'changes' ? (
            <div className="git-scroll">
              {nChanges === 0 && (
                <Empty icon={Check} title={t('git.clean_title')}>
                  <p className="muted small">{t('git.clean_body')}</p>
                </Empty>
              )}
              {mine.length > 0 && (
                <>
                  <div className="sec git-group">{t('git.group_conv', { n: mine.length })}</div>
                  <ul className="git-files">{mine.map(row)}</ul>
                  {others.length > 0 && <div className="sec git-group">{t('git.group_other', { n: others.length })}</div>}
                </>
              )}
              <ul className="git-files">{(mine.length ? others : files).map(row)}</ul>
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
          {mode === 'changes' && nChanges > 0 && (
            <CommitBox
              id={id}
              picked={picked}
              onDone={() => {
                setPicks(null);
                setView(null);
                load();
              }}
            />
          )}
        </aside>
      )}
      {(!narrow || view) && <DiffView view={view} narrow={narrow} onBack={() => setView(null)} />}
    </div>
  );
}
