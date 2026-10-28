// 添加项目：选一台电脑，从那台电脑上有过对话的文件夹里勾（GET /history/folders，Claude Code、Codex、OpenCode 的记录里的工作目录），
// 也可以粘贴路径、浏览文件夹。加进来后项目下面自动列出这个文件夹里以前的对话。
// 新用户的开始页上也摆几个找到的文件夹（DiscoverCard），点一下就加。
import { FolderOpen, FolderPlus, Search } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { deviceApi } from '../shared/api.js';
import { DeviceAvatar } from '../shared/brands.jsx';
import { deviceName, useDevices } from '../shared/devices.jsx';
import { errorText } from '../shared/i18n.js';
import { useRouter } from '../shared/router.jsx';
import { baseName, dirKey, projectPath, useSessions } from '../shared/sessions.jsx';
import { relativeTime } from '../shared/time.js';
import { Modal, Spinner, useToast } from '../shared/ui.jsx';
import { useSessionUi } from './Sessions.jsx';
import { FolderPicker } from './Terminals.jsx';
import { imeComposing } from '../shared/ime.js';

// 看起来是路径（不是在搜名字）：~、/ 开头，或者 C:\ 这样的盘符
const PATH_RE = /^(~|\/|[a-z]:[\\/])/i;
// 开始页上摆几个
const CARD_SHOW = 3;

/** 那台电脑上有过对话的文件夹（从新到旧）和用户主目录。旧版客户端没有这个接口：当作一个也没有。 */
function useDiscover(device) {
  const [data, setData] = useState(null);
  useEffect(() => {
    let alive = true;
    setData(null);
    const client = deviceApi(device);
    Promise.all([client.get('/history/folders?per=0').catch(() => null), client.get('/fs/list?path=').catch(() => null)]).then(([f, home]) => {
      if (alive) setData({ items: Array.isArray(f?.items) ? f.items : [], home: home?.home ?? '' });
    });
    return () => {
      alive = false;
    };
  }, [device]);
  return data;
}

/** 主目录下的路径写成 ~/…；`~` 开头的换回完整路径。 */
const shortPath = (p, home) => (home && (p === home || p.startsWith(`${home}/`) || p.startsWith(`${home}\\`)) ? `~${p.slice(home.length)}` : p);
const fullPath = (p, home) => (home && /^~([\\/]|$)/.test(p) ? `${home}${p.slice(1)}` : p);

/** 已经是这台电脑上的项目了。 */
function useAdded(device) {
  const s = useSessions();
  return (workdir) => s.projects.some((p) => (p.device ?? null) === (device ?? null) && !p.container && dirKey(p.workdir) === dirKey(workdir));
}

function counts(t, f) {
  return Object.entries(f.counts ?? {})
    .sort((a, b) => b[1] - a[1])
    .map(([a, n]) => `${t(`launch.${a}.title`)} ${n}`)
    .join(' · ');
}

export function AddProjectDialog({ want, onClose }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  const firstOnline = devices.items.find((d) => d.online)?.id ?? null;
  const [device, setDevice] = useState(want.device !== undefined ? want.device : devices.multi ? (devices.byId(devices.selected)?.online ? devices.selected : firstOnline) : null);
  const data = useDiscover(device);
  const added = useAdded(device);
  const [q, setQ] = useState('');
  const [picked, setPicked] = useState(() => new Map());
  const [browse, setBrowse] = useState(false);
  const [busy, setBusy] = useState(false);
  const home = data?.home ?? '';
  const text = q.trim();
  const isPath = PATH_RE.test(text);
  const typed = isPath ? fullPath(text, home) : '';
  const words = text.toLowerCase();
  const list = (data?.items ?? []).filter((f) => !words || (isPath ? dirKey(f.workdir).toLowerCase().startsWith(dirKey(typed).toLowerCase()) : `${baseName(f.workdir)}\n${f.workdir}`.toLowerCase().includes(words)));
  const typedListed = isPath && list.some((f) => dirKey(f.workdir) === dirKey(typed));
  const pickDevice = (id) => {
    setDevice(id);
    setPicked(new Map());
  };
  const toggle = (f) =>
    setPicked((m) => {
      const next = new Map(m);
      if (next.has(dirKey(f.workdir))) next.delete(dirKey(f.workdir));
      else next.set(dirKey(f.workdir), f);
      return next;
    });
  /** 加进去；只加了一个时打开它的项目页。 */
  const add = async (items) => {
    if (!items.length || busy) return;
    setBusy(true);
    try {
      await s.addProjects(device, items.map((f) => ({ workdir: f.workdir, used_at: f.updated_at })));
      toast(t('addp.done', { count: items.length }), 'ok');
      onClose();
      if (items.length === 1) navigate(projectPath({ device, workdir: items[0].workdir }));
    } catch (e) {
      toast(errorText(t, e), 'error');
      setBusy(false);
    }
  };
  const chosen = [...picked.values()];
  const withTyped = typed && !typedListed && !added(typed) ? [...chosen, { workdir: typed }] : chosen;
  return (
    <Modal open onClose={onClose} title={t('addp.title')} className="addp">
      {devices.multi && (
        <div className="addp-devs" role="group" aria-label={t('addp.device')}>
          {devices.items.map((d) => (
            <button key={d.id} type="button" aria-pressed={device === d.id} disabled={!d.online} onClick={() => pickDevice(d.id)}>
              <DeviceAvatar device={d} size={18} />
              <span className="ellipsis">{deviceName(t, d)}</span>
              {!d.online && <span className="muted">{t('devices.offline')}</span>}
            </button>
          ))}
        </div>
      )}
      <div className="addp-bar">
        <label className="addp-search">
          <Search size={15} aria-hidden="true" />
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !imeComposing(e) && withTyped.length) {
                e.preventDefault();
                add(withTyped);
              }
            }}
            placeholder={t('addp.search')}
            aria-label={t('addp.search')}
            spellCheck={false}
            autoFocus
          />
        </label>
        <button type="button" className="btn btn-ghost" onClick={() => setBrowse(true)}>
          <FolderOpen size={16} aria-hidden="true" />
          {t('addp.browse')}
        </button>
      </div>
      {typed && !typedListed && (
        <button type="button" className="addp-path" onClick={() => add([{ workdir: typed }])} disabled={busy || added(typed)}>
          <FolderPlus size={18} aria-hidden="true" />
          <span className="grow">
            <b className="mono ellipsis">{shortPath(typed, home)}</b>
            <span className="small muted">{added(typed) ? t('addp.added') : t('addp.path_hint')}</span>
          </span>
          {!added(typed) && <span className="small addp-enter">{t('addp.enter')}</span>}
        </button>
      )}
      <h3 className="addp-head">{t(words ? 'addp.found_match' : 'addp.found', { count: list.length })}</h3>
      <div className="addp-list">
        {!data ? (
          <div className="addp-loading">
            <Spinner size={16} />
          </div>
        ) : list.length === 0 ? (
          <p className="addp-empty">{t(words ? 'addp.none_match' : 'addp.none')}</p>
        ) : (
          list.map((f) => {
            const has = added(f.workdir);
            return (
              <label key={f.workdir} className={`addp-row ${has ? 'added' : ''} ${picked.has(dirKey(f.workdir)) ? 'on' : ''}`}>
                <input type="checkbox" checked={has || picked.has(dirKey(f.workdir))} disabled={has} onChange={() => toggle(f)} />
                <span className="addp-text">
                  <span className="addp-name">
                    <b className="ellipsis">{baseName(f.workdir)}</b>
                    {has && <span className="badge badge-ok">{t('addp.added')}</span>}
                  </span>
                  <span className="mono ellipsis small muted">{shortPath(f.workdir, home)}</span>
                </span>
                <span className="small muted addp-counts">{counts(t, f)}</span>
                <span className="small muted addp-when">{relativeTime(t, f.updated_at)}</span>
              </label>
            );
          })
        )}
      </div>
      <div className="addp-foot">
        <button
          type="button"
          className="btn btn-plain btn-sm"
          onClick={() => {
            onClose();
            ui.newSession({ device });
          }}
        >
          {t('addp.docker')}
        </button>
        <span className="grow" />
        <button type="button" className="btn btn-ghost" onClick={onClose}>
          {t('action.cancel')}
        </button>
        <button type="button" className="btn btn-primary" onClick={() => add(withTyped)} disabled={busy || withTyped.length === 0}>
          {busy && <Spinner size={15} />}
          {withTyped.length > 1 ? t('addp.add_n', { count: withTyped.length }) : t('addp.add')}
        </button>
      </div>
      <FolderPicker open={browse} initial={home} client={deviceApi(device)} onClose={() => setBrowse(false)} onPick={(dir) => add([{ workdir: dir }])} pickLabel={t('addp.add')} />
    </Modal>
  );
}

/** 还没有项目时开始页上的卡片：这台电脑上找到的几个有过对话的文件夹，点“添加”就加。 */
export function DiscoverCard({ device }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  const data = useDiscover(device);
  const added = useAdded(device);
  const list = (data?.items ?? []).filter((f) => !added(f.workdir)).slice(0, CARD_SHOW);
  if (!data || list.length === 0) return null;
  const add = (f) =>
    s.addProjects(device, [{ workdir: f.workdir, used_at: f.updated_at }]).then(
      () => navigate(projectPath({ device, workdir: f.workdir })),
      (e) => toast(errorText(t, e), 'error'),
    );
  return (
    <section className="discover" aria-label={t('addp.card_title')}>
      <h2>{t('addp.card_title')}</h2>
      <p className="small muted">{t('addp.card_lead', { count: data.items.length })}</p>
      {list.map((f) => (
        <div key={f.workdir} className="discover-row">
          <span className="grow">
            <b className="ellipsis">{baseName(f.workdir)}</b>
            <span className="small muted ellipsis">
              {shortPath(f.workdir, data.home)} · {t('addp.conv_count', { count: f.total })}
            </span>
          </span>
          <button type="button" className="btn btn-ghost btn-sm" onClick={() => add(f)}>
            {t('addp.add')}
          </button>
        </div>
      ))}
      <button type="button" className="btn btn-ghost btn-sm" onClick={() => ui.addProject({ device })}>
        {t('addp.other')}
      </button>
    </section>
  );
}
