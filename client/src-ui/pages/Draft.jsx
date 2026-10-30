// 草稿新会话：点“新会话”（侧栏、Ctrl+Shift+O、项目上的 +、标签栏的 +、手机右下角）马上出现，在当前项目里；
// 写下第一句、回车才新建并启动，不发出去不会启动任何东西。上面换项目（按电脑分组、可以搜），输入框上换类型、账号、全部权限，
// ⋯ 里是名字、Shell、开机自动启动、Docker；AI 工具旁边有常用任务模板（PromptTemplates.jsx）。终端、DSH 点了马上开，不用草稿。
// 发出去后会话马上打开：Claude Code、Codex 的第一句（连同图片）等对话起来了再发（shared/firstSend.js），
// 其他的交给电脑在启动时带上（prompt）。没写完的字按项目存在浏览器里。
import { ArrowLeft, ArrowUp, Container, FolderOpen, Folder, FolderPlus, ImagePlus, MoreHorizontal, Search, ShieldCheck, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { SafetyNote, useStatus } from '../App.jsx';
import { deviceApi, rememberTerminals, terminalDevice } from '../shared/api.js';
import { CHAT_LAUNCHES, isDshWeb } from '../shared/chat.js';
import { DeviceAvatar, deviceColor, LaunchIcon } from '../shared/brands.jsx';
import { deviceName, osLabel, useDevices } from '../shared/devices.jsx';
import { clearDraftBack, draftBack, queueFirst } from '../shared/firstSend.js';
import { errorText } from '../shared/i18n.js';
import { isTouch } from '../shared/KeyBar.jsx';
import PopMenu from '../shared/PopMenu.jsx';
import { useRouter } from '../shared/router.jsx';
import { baseName, isDraftProject, projectEntries, projectKey, projectPath, useSessions } from '../shared/sessions.jsx';
import { Button, ErrorNote, Spinner, Switch, useLoad, useToast } from '../shared/ui.jsx';
import { ClaudeProfilePick, CodexProfilePick } from './ClaudeProfiles.jsx';
import { DiscoverCard } from './AddProject.jsx';
import { Onboarding, useGuideShowsRemote } from './Onboarding.jsx';
import { DraftTip } from './Discover.jsx';
import PromptsMenu from './PromptTemplates.jsx';
import { AI_TOOLS, FULL_ACCESS, fullAccessFor, opensInPage, shortAgo, useSessionUi } from './Sessions.jsx';
import { BackendNote, FolderPicker, InstallProgress, Promotions, RemoteCard, ShellPicker } from './Terminals.jsx';
import { imeComposing } from '../shared/ime.js';

// 留在草稿里的类型（写好第一句再开始）；终端、DSH 点了马上开
const DRAFT_LAUNCHES = ['claude', 'codex', 'opencode', 'dsh'];
// 第一句话能用在哪些类型上（AI 工具作为第一条消息，普通终端当作启动后运行的命令）
const PROMPT_LAUNCHES = ['claude', 'codex', 'opencode', 'shell'];
const DRAFTS_KEY = 'looklook.new-drafts';
const RECENT_N = 3;
const MAX_IMAGES = 10;

function readDrafts() {
  try {
    return JSON.parse(localStorage.getItem(DRAFTS_KEY) ?? '{}') ?? {};
  } catch {
    return {};
  }
}
function writeDraft(key, text) {
  try {
    const all = readDrafts();
    if (text) all[key] = text;
    else delete all[key];
    localStorage.setItem(DRAFTS_KEY, JSON.stringify(all));
  } catch {
    /* 无痕模式等：只是不记住 */
  }
}

/**
 * Docker 容器：点开后列出这台电脑上运行中的容器（docker ps），可以搜；选中后探测容器里的用户、默认目录和装了的工具。
 * 容器里的会话只有终端，没有对话界面。
 */
function DockerPick({ device, onPick }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [q, setQ] = useState('');
  const [busy, setBusy] = useState(null);
  const [error, setError] = useState(null);
  const [list] = useLoad(() => (open ? deviceApi(device).get('/docker/containers').catch(() => ({ items: [], error: 'DOCKER_FAILED' })) : Promise.resolve(null)), [open, device]);
  const pick = async (c) => {
    setBusy(c.name);
    setError(null);
    try {
      const p = await deviceApi(device).get(`/docker/containers/${encodeURIComponent(c.name)}`);
      onPick({ name: c.name, image: c.image, dir: p.workdir || '/', root: Boolean(p.root), user: p.user, tools: p.tools ?? [] });
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };
  if (!open) {
    return (
      <button type="button" className="ppick-item ppick-other" onClick={() => setOpen(true)}>
        <Container size={15} aria-hidden="true" />
        <span className="grow">{t('draft.docker')}</span>
        <span className="ppick-path">{t('draft.docker_hint')}</span>
      </button>
    );
  }
  const words = q.trim().toLowerCase();
  const items = (list?.items ?? []).filter((c) => !words || `${c.name} ${c.image} ${c.id}`.toLowerCase().includes(words));
  return (
    <div className="ppick-docker">
      <label className="slist-search ppick-search">
        <Search size={14} aria-hidden="true" />
        <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('sess.docker_search')} aria-label={t('sess.docker_search')} spellCheck={false} autoFocus={!isTouch()} />
      </label>
      {!list ? (
        <div className="ppick-note"><Spinner size={14} /></div>
      ) : list.error ? (
        <p className="ppick-note">{t(`sess.docker_err.${list.error}`, { defaultValue: t('sess.docker_err.DOCKER_FAILED') })}</p>
      ) : items.length === 0 ? (
        <p className="ppick-note">{words ? t('sess.no_match') : t('sess.docker_none')}</p>
      ) : (
        items.map((c) => (
          <button key={c.id || c.name} type="button" className="ppick-item" onClick={() => pick(c)} disabled={Boolean(busy)} title={c.status}>
            {busy === c.name ? <Spinner size={15} /> : <Container size={15} aria-hidden="true" />}
            <span className="ellipsis grow">{c.name}</span>
            <span className="ppick-path mono ellipsis">{c.image}</span>
          </button>
        ))
      )}
      {error && <p className="ppick-note warn-text">{errorText(t, error)}</p>}
    </div>
  );
}

/** 文件夹选择弹窗用的“客户端”：把 `/fs/list` 换成容器里的文件夹列表（字段一样）。 */
function containerFs(device, name) {
  return { get: (url) => deviceApi(device).get(url.replace(/^\/fs\/list\?/, `/docker/containers/${encodeURIComponent(name)}/fs?`)) };
}

/** 换项目：按电脑分组列出项目（可以搜），下面“选择其他文件夹”“添加项目”。 */
function ProjectPicker({ device, workdir, container, onPick, onClose }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const ref = useRef(null);
  const [q, setQ] = useState('');
  // 正在浏览哪台电脑的文件夹：{ dev }，dev 只有一台电脑时是 null，所以不能直接用 null 表示“没打开”
  const [browse, setBrowse] = useState(null);
  useEffect(() => {
    const close = (e) => !ref.current?.contains(e.target) && !browse && onClose();
    const esc = (e) => e.key === 'Escape' && !browse && onClose();
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', esc);
    };
  }, [browse]);
  const words = q.trim().toLowerCase();
  const fits = (p) => !words || `${p.name} ${p.workdir} ${p.container?.name ?? ''}`.toLowerCase().includes(words);
  const devs = devices.multi ? devices.items.filter((d) => d.online) : [null];
  return (
    <div className="ppick draft-ppick" ref={ref} role="dialog" aria-label={t('draft.switch_project')}>
      {s.projects.length > 6 && (
        <label className="slist-search ppick-search">
          <Search size={14} aria-hidden="true" />
          <input type="search" value={q} onChange={(e) => setQ(e.target.value)} placeholder={t('draft.search_project')} aria-label={t('draft.search_project')} spellCheck={false} autoFocus={!isTouch()} />
        </label>
      )}
      {devs.map((d) => {
        const ps = s.projects.filter((p) => p.device === (d?.id ?? null) && !p.offline && fits(p)).slice(0, words ? 30 : 12);
        if (words && !ps.length) return null;
        return (
          <div key={d?.id ?? 'one'} className="ppick-group">
            {d && (
              <div className="ppick-dev" style={{ '--dc': deviceColor(devices, d.id) }}>
                <DeviceAvatar device={d} size={18} />
                <b className="ellipsis">{deviceName(t, d)}</b>
                <span className="muted">{osLabel(d.os)}</span>
              </div>
            )}
            {ps.map((p) => (
              <button
                key={p.key}
                type="button"
                className="ppick-item"
                aria-pressed={isDraftProject(p, { device, workdir, container })}
                onClick={() => onPick(p.device, p.container ? '' : p.workdir, p.container ?? null)}
              >
                {p.container ? <Container size={15} aria-hidden="true" /> : <Folder size={15} aria-hidden="true" />}
                <span className="ellipsis grow">{p.name}</span>
                <span className="ppick-path mono ellipsis">{p.container ? `${p.container.name}:${p.container.dir}` : p.workdir}</span>
              </button>
            ))}
            {!words && (
              <button type="button" className="ppick-item ppick-other" onClick={() => setBrowse({ dev: d?.id ?? null })}>
                <FolderOpen size={15} aria-hidden="true" />
                <span className="grow">{t('sess.pick_other')}</span>
              </button>
            )}
          </div>
        );
      })}
      {words && !s.projects.some((p) => !p.offline && fits(p)) && <p className="ppick-note">{t('sess.no_match')}</p>}
      <button
        type="button"
        className="ppick-item ppick-other ppick-add"
        onClick={() => {
          onClose();
          ui.addProject({ device });
        }}
      >
        <FolderPlus size={15} aria-hidden="true" />
        <span className="grow">{t('draft.add_project')}</span>
      </button>
      <FolderPicker
        open={browse !== null}
        initial={browse && browse.dev === device ? workdir : ''}
        client={deviceApi(browse?.dev ?? null)}
        onClose={() => setBrowse(null)}
        onPick={(path) => onPick(browse.dev, path, null)}
      />
    </div>
  );
}

/** 草稿页：等项目列表到了再初始化（默认在当前项目、最近用过的项目里）。 */
export function DraftPane() {
  const s = useSessions();
  const ui = useSessionUi();
  if (s.loading) return <div className="draft draft-loading"><Spinner size={20} /></div>;
  return <DraftForm key={ui.start.nonce} />;
}

function DraftForm() {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const { status: selectedStatus } = useStatus();
  const devices = useDevices();
  const s = useSessions();
  const ui = useSessionUi();
  const toast = useToast();
  // 没起来的会话“回到草稿”时带回来的（字、图片、项目、类型）优先
  const [want] = useState(() => draftBack() ?? ui.start);
  useEffect(() => clearDraftBack(), []);
  // 默认：要求的 > 最近用过的在线项目 > 默认文件夹
  const firstProject = s.projects.find((p) => !p.offline);
  const [device, setDevice] = useState(want.device !== undefined ? want.device : firstProject ? firstProject.device : devices.multi ? devices.selected : null);
  const [workdir, setWorkdir] = useState(want.workdir ?? (want.device === undefined && firstProject && !firstProject.container ? firstProject.workdir : ''));
  // Docker 容器（{ name, dir, root, tools }）：选了时会话在容器里运行，只有终端
  const [container, setContainer] = useState(want.container ?? (want.device === undefined && firstProject?.container ? firstProject.container : null));
  const at = { device, workdir, container };
  const project = s.projects.find((p) => isDraftProject(p, at));
  const draftKey = projectKey(device, container ? `docker://${container.name}${container.dir}` : workdir);
  const initLaunch = (p) => (DRAFT_LAUNCHES.includes(p?.launch) || p?.launch === 'custom' ? p.launch : 'claude');
  const [launch, setLaunchState] = useState(DRAFT_LAUNCHES.includes(want.launch) || want.launch === 'custom' ? want.launch : initLaunch(project));
  // 用户自己选过类型、全部权限：换项目、那台电脑的设置到了都不再改它
  const chosen = useRef(Boolean(want.launch));
  const fullChosen = useRef(false);
  const setLaunch = (l) => {
    chosen.current = true;
    setLaunchState(l);
  };
  const [text, setText] = useState(() => want.text || readDrafts()[draftKey] || '');
  const [files, setFiles] = useState(() => (want.files ?? []).map((file, k) => ({ key: k, file, url: URL.createObjectURL(file) })));
  // Claude Code 用哪个账号 / 接口：null 用那台电脑上的默认
  // Claude Code 账号 / Codex 配置（null 用默认）；换了助手就回到默认
  const [profile, setProfile] = useState(null);
  useEffect(() => setProfile(null), [launch]);
  const [picking, setPicking] = useState(false);
  const [dirPick, setDirPick] = useState(false);
  const [adv, setAdv] = useState(false);
  const [name, setName] = useState('');
  const [shell, setShell] = useState(project?.shell ?? '');
  const [fullAccess, setFullAccessState] = useState(() => fullAccessFor(project, selectedStatus.settings));
  const setFullAccess = (v) => {
    fullChosen.current = true;
    setFullAccessState(v);
  };
  const [autoStart, setAutoStart] = useState(Boolean(project?.auto_start));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(null);
  const input = useRef(null);
  const picker = useRef(null);
  const client = deviceApi(device);
  // 不是顶栏选中的那台电脑时，用那台电脑自己的状态（默认文件夹、能力）
  const [devStatus] = useLoad(() => (devices.multi && device && device !== devices.selected ? client.get('/status').catch(() => null) : Promise.resolve(null)), [device]);
  const status = devStatus?.settings && devStatus?.capabilities ? devStatus : selectedStatus;
  const [tools, , reloadTools] = useLoad(() => client.get('/tools').catch(() => null), [device]);
  const [shells] = useLoad(() => (adv ? client.get('/shells').catch(() => null) : Promise.resolve(null)), [device, adv]);
  useEffect(() => {
    if (!workdir && !container && status?.settings?.default_workdir) setWorkdir(status.settings.default_workdir);
  }, [status?.settings?.default_workdir]);
  // 侧栏、标签栏显示“新会话 · 草稿”
  useEffect(() => {
    ui.setDraftAt(at);
    return () => ui.setDraftAt(null);
  }, [device, workdir, container?.name, container?.dir]);
  // 写一半的字：停手 0.3 秒按项目存一次；离开草稿页时马上存（发出去了就不存）
  const latest = useRef({ key: draftKey, text, sent: false });
  latest.current = { ...latest.current, key: draftKey, text };
  useEffect(() => {
    const timer = setTimeout(() => writeDraft(draftKey, text), 300);
    return () => clearTimeout(timer);
  }, [draftKey, text]);
  useEffect(() => () => !latest.current.sent && writeDraft(latest.current.key, latest.current.text), []);
  // 手机上不主动聚焦（会弹出输入法）；点侧栏里的“新会话 · 草稿”时也回到输入框
  useEffect(() => {
    if (!isTouch()) input.current?.focus();
    const focus = () => input.current?.focus();
    window.addEventListener('ll-draft-focus', focus);
    return () => window.removeEventListener('ll-draft-focus', focus);
  }, []);
  useEffect(() => {
    const el = input.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.35))}px`;
  }, [text]);
  // 换了项目：写了一半的字留着（那个项目存着的草稿只在这边没写时拿出来）；类型、全部权限等跟着那个项目上次的（自己改过的不动）
  const pickProject = (dev, dir, c = null) => {
    setPicking(false);
    const nextKey = projectKey(dev, c ? `docker://${c.name}${c.dir}` : dir);
    if (nextKey !== draftKey && text) writeDraft(draftKey, '');
    if (!text) setText(readDrafts()[nextKey] || '');
    setDevice(dev);
    setContainer(c);
    setWorkdir(c ? '' : dir);
    const p = s.projects.find((x) => isDraftProject(x, { device: dev, workdir: c ? '' : dir, container: c }));
    if (p) {
      if (!chosen.current) setLaunchState(initLaunch(p));
      setShell(p.shell ?? '');
      if (!fullChosen.current) setFullAccessState(fullAccessFor(p, status?.settings));
      setAutoStart(Boolean(p.auto_start));
    }
    setTimeout(() => !isTouch() && input.current?.focus(), 0);
  };
  // 别的电脑的设置（默认全部权限）晚一点才到：还没勾过时跟着它
  useEffect(() => {
    if (!fullChosen.current) setFullAccessState(fullAccessFor(project, status?.settings));
  }, [device, status?.settings?.default_full_access]);
  const missing = (l) => (container ? AI_TOOLS.includes(l) && Array.isArray(container.tools) && !container.tools.includes(l) : AI_TOOLS.includes(l) && tools && !tools.tools?.[l]);
  const toolName = t(`launch.${launch}.title`);
  const canFull = FULL_ACCESS.includes(launch);
  const allowed = selectedStatus.account.allowed;
  const devObj = devices.multi ? devices.byId(device) : null;
  const offline = devices.multi && !devObj?.online;
  // 有对话界面的（Claude Code、Codex）：第一句连同图片等对话起来了再发；其他的交给电脑在启动时带上，没法带图片
  const willChat = CHAT_LAUNCHES.includes(launch) && !container && Boolean(status.capabilities?.persistent);
  // 没装的工具：点开始就先在后台装（npm），装好后自动接着开始
  const [job, setJob] = useState(null);
  const [mirror, setMirror] = useState('npmmirror');
  const [afterInstall, setAfterInstall] = useState(false);
  useEffect(() => {
    setJob(null);
    setAfterInstall(false);
  }, [launch, device]);
  useEffect(() => {
    if (job?.state !== 'running') return undefined;
    const timer = setTimeout(async () => {
      const j = await client.get(`/tools/install/${launch}`).catch(() => null);
      // 一次没取到（网络抖了）：接着问
      setJob(j ?? { ...job });
      if (j?.state === 'ok') reloadTools();
    }, 1000);
    return () => clearTimeout(timer);
  }, [job]);
  const canInstall = !container && AI_TOOLS.includes(launch) && Boolean(tools?.tools?.npm);
  const install = async () => {
    setError(null);
    try {
      setJob(await client.post('/tools/install', { tool: launch, mirror }));
      setAfterInstall(true);
    } catch (e) {
      setError(e);
    }
  };
  // 装好了、工具列表也刷新了：接着开始
  const installed = afterInstall && job?.state === 'ok' && !missing(launch);
  useEffect(() => {
    if (!installed) return;
    setAfterInstall(false);
    go();
  }, [installed]);
  const where = container ? container.dir.trim() : workdir.trim();
  // 不能新建的原因：写在输入框上面，草稿留着
  const blocked = !allowed ? t('sess.why_locked') : offline ? t('draft.offline', { device: devObj ? deviceName(t, devObj) : '' }) : !where ? t('sess.why_no_folder') : null;
  const target = () =>
    project ?? (container ? { device, workdir: '', container: { name: container.name, dir: container.dir.trim() || '/' }, items: [] } : { device, workdir, container: null, items: [], shell, auto_start: autoStart });
  // 终端、DSH：点了就开，不用草稿
  const openNow = async (l) => {
    if (blocked) return toast(blocked, 'error');
    if (l === 'dsh-web') return dshWeb && ui.openSession(dshWeb.id);
    await ui.quickCreate(target(), l);
  };
  const go = async () => {
    if (busy || blocked) return;
    setError(null);
    if (missing(launch)) {
      // 能直接装的就装，装好后自动开始；没有 npm（要先装 Node.js）时打开说明怎么装的窗口
      if (canInstall) {
        if (job?.state !== 'running') install();
      } else if (!container) ui.install(launch, device, workdir);
      return;
    }
    const body = text.trim();
    if (launch === 'custom' && !body) {
      input.current?.focus();
      return;
    }
    const prompt = !willChat && PROMPT_LAUNCHES.includes(launch) ? body : '';
    setBusy(true);
    try {
      const r = await client.post('/instances', {
        // 没填名称时用第一句话的开头，否则交给电脑取（文件夹 · 工具）
        name: (name.trim() || (body && launch !== 'custom' ? body.replace(/\s+/g, ' ').slice(0, 20) : '')).slice(0, 40),
        name_custom: Boolean(name.trim()),
        workdir: container ? '' : workdir,
        container: container ? { name: container.name, dir: container.dir.trim() || '/' } : undefined,
        launch,
        command: launch === 'custom' ? body : '',
        prompt: prompt || undefined,
        shell: shell.trim(),
        auto_start: autoStart,
        full_access: canFull && fullAccess,
        claude_profile: launch === 'claude' && !container && profile ? profile : undefined,
        codex_profile: launch === 'codex' && !container && profile ? profile : undefined,
        start: true,
      });
      if (device && r.id) rememberTerminals(device, [r]);
      if (willChat && r.chat_ui && (body || files.length)) queueFirst(r.id, body, files.map((f) => f.file));
      // 全部权限只在能选的类型（AI 会话）上记
      if (!container) s.touchProject(device, r.workdir ?? workdir, { launch, shell: shell.trim(), auto_start: autoStart, ...(canFull ? { full_access: fullAccess } : {}) });
      latest.current.sent = true;
      writeDraft(draftKey, '');
      files.forEach((f) => URL.revokeObjectURL(f.url));
      s.reload();
      s.reloadFolders();
      ui.openSession(r.id);
    } catch (e) {
      setError(e);
      setBusy(false);
    }
  };
  // 图片：等会话起来了再传（上传要会话号），这里先留着
  const addFiles = (list) => {
    const imgs = [...list].filter((f) => f.type.startsWith('image/'));
    if (!imgs.length) return false;
    setFiles((cur) => [...cur, ...imgs.map((file, k) => ({ key: `${Date.now()}-${k}`, file, url: URL.createObjectURL(file) }))].slice(0, MAX_IMAGES));
    return true;
  };
  const removeFile = (key) =>
    setFiles((cur) => {
      const x = cur.find((f) => f.key === key);
      if (x) URL.revokeObjectURL(x.url);
      return cur.filter((f) => f.key !== key);
    });
  const projName = container ? container.name : project?.name || baseName(workdir) || workdir || '…';
  const recent = project && !container ? projectEntries(project).rest.filter((x) => x.kind === 'conv').slice(0, RECENT_N) : [];
  const empty = !text.trim() && !files.length;
  const guideRemote = useGuideShowsRemote();
  // 以前开的网页版 DSH（每台电脑一个）：还能从这里打开它的网页；新开的 DSH 都是对话模式
  const dshWeb = s.items.find((i) => isDshWeb(i) && (!devices.multi || terminalDevice(i.id) === device));
  const typeItems = [
    { head: t('draft.group_chat') },
    // DSH 的对话走这台电脑上的 dsh web，容器里用不了
    ...DRAFT_LAUNCHES.filter((l) => !(container && l === 'dsh')).map((l) => ({
      icon: (p) => <LaunchIcon launch={l} {...p} />,
      label: t(`launch.${l}.title`),
      hint: missing(l) ? t(container ? 'sess.container_missing' : 'draft.not_installed_hint', { tool: t(`launch.${l}.title`) }) : project?.launch === l ? t('draft.last_used') : undefined,
      checked: launch === l,
      onClick: () => setLaunch(l),
    })),
    { head: t('draft.group_now') },
    { icon: (p) => <LaunchIcon launch="shell" {...p} />, label: t('launch.shell.title'), hint: t('draft.shell_hint'), checked: false, onClick: () => openNow('shell') },
    { icon: (p) => <LaunchIcon launch="custom" {...p} />, label: t('draft.custom'), hint: t('draft.custom_hint'), checked: launch === 'custom', onClick: () => setLaunch('custom') },
    !container && dshWeb && { icon: (p) => <LaunchIcon launch="dsh" {...p} />, label: t('draft.dsh_web'), hint: t('draft.dsh_open'), checked: false, onClick: () => openNow('dsh-web') },
  ];
  const ph = launch === 'custom' ? t('sess.ph_custom') : t('draft.ph', { tool: toolName });
  const installing = job?.tool === launch && job.state === 'running';
  const sendLabel = missing(launch) && canInstall ? t('draft.install_start') : t('sess.start');
  const where2 = container ? `${container.name}:${container.dir}` : workdir;
  return (
    <div className="draft">
      <header className="draft-head">
        {!opensInPage() && (
          <button type="button" className="btn btn-plain btn-icon draft-back" onClick={() => navigate('/')} aria-label={t('sess.back')}>
            <ArrowLeft size={20} />
          </button>
        )}
        <span className="draft-loc" ref={picker}>
          <button type="button" className="draft-proj" onClick={() => setPicking(!picking)} aria-expanded={picking} title={where2}>
            {container ? <Container size={15} aria-hidden="true" /> : <Folder size={15} aria-hidden="true" />}
            <b className="ellipsis">{projName}</b>
            {devObj && <span className="muted ellipsis">· {deviceName(t, devObj)}</span>}
            <span className="draft-caret" aria-hidden="true">▾</span>
          </button>
          {picking && <ProjectPicker device={device} workdir={workdir} container={container} onPick={pickProject} onClose={() => setPicking(false)} />}
        </span>
        {where2 && <span className="draft-path mono ellipsis">{where2}</span>}
      </header>
      <div className="draft-body">
        <div className="draft-hero">
          <span className={`draft-mark draft-mark-${launch}`} aria-hidden="true">
            <LaunchIcon launch={launch} size={30} />
          </span>
          <h1>{launch === 'custom' ? t('draft.title_custom', { name: projName }) : t('draft.title', { name: projName, tool: toolName })}</h1>
          <p className="muted">{t('draft.lead')}</p>
        </div>
        {recent.length > 0 && <RecentConvs device={device} items={recent} />}
        {empty && (
          <Onboarding
            onTry={(x) => {
              setText(x);
              input.current?.focus();
            }}
          />
        )}
        {s.projects.length === 0 && <DiscoverCard device={device} />}
        {empty && (
          <div className="draft-cards">
            <BackendNote />
            {!guideRemote && <RemoteCard />}
            <SafetyNote />
            <Promotions />
          </div>
        )}
      </div>
      <div className="draft-dock">
        {blocked ? (
          <div className="draft-strip draft-strip-off">
            <span className="grow">{blocked}</span>
            {(offline || !where) && (
              <button type="button" className="btn btn-plain btn-sm" onClick={() => setPicking(true)}>
                {t('draft.switch_project')}
              </button>
            )}
          </div>
        ) : installing || (job?.tool === launch && job.state !== 'ok') ? (
          <div className="draft-strip draft-strip-info">
            <div className="grow">
              <InstallProgress job={job} tool={toolName} onRetry={install} busy={false} />
              {installing && <p className="small">{t('sess.install_auto_start')}</p>}
            </div>
          </div>
        ) : (
          missing(launch) && (
            <div className="draft-strip draft-strip-warn">
              <span className="grow">{t(container ? 'sess.container_missing' : canInstall ? 'draft.install_strip' : 'sess.install_first', { tool: toolName })}</span>
              {canInstall && (
                <select value={mirror} onChange={(e) => setMirror(e.target.value)} aria-label={t('tools.mirror')}>
                  {['npmmirror', 'tencent', 'official'].map((m) => (
                    <option key={m} value={m}>
                      {t(`tools.mirrors.${m}`)}
                    </option>
                  ))}
                </select>
              )}
              {!container && (
                <button type="button" className="btn btn-plain btn-sm" onClick={reloadTools}>
                  {t('tools.recheck')}
                </button>
              )}
            </div>
          )
        )}
        <form
          className={`draft-box ${blocked ? 'off' : ''}`}
          onSubmit={(e) => {
            e.preventDefault();
            go();
          }}
          onDragOver={(e) => willChat && e.preventDefault()}
          onDrop={(e) => {
            if (!willChat) return;
            e.preventDefault();
            addFiles(e.dataTransfer.files);
          }}
        >
          {files.length > 0 && (
            <div className="chat-files">
              {files.map((f) => (
                <span key={f.key} className="chat-file">
                  <img src={f.url} alt={f.file.name} />
                  <button type="button" onClick={() => removeFile(f.key)} aria-label={t('chat.remove_image')}>
                    <X size={12} />
                  </button>
                </span>
              ))}
            </div>
          )}
          <textarea
            ref={input}
            className="draft-input"
            rows={2}
            autoCapitalize="none"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onPaste={(e) => willChat && addFiles(e.clipboardData.files) && e.preventDefault()}
            // 手机上没有 Shift+回车：回车换行，点“开始”发出去（同对话页的输入框）
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey && !imeComposing(e) && !isTouch()) {
                e.preventDefault();
                go();
              }
            }}
            enterKeyHint={isTouch() ? 'enter' : 'send'}
            placeholder={ph}
            aria-label={t('draft.first_line')}
            spellCheck={false}
            maxLength={4000}
          />
          <div className="draft-tools">
            {willChat && (
              <label className="btn btn-plain btn-icon draft-attach" title={t('draft.attach')}>
                <ImagePlus size={17} aria-hidden="true" />
                <span className="sr-only">{t('draft.attach')}</span>
                <input
                  type="file"
                  accept="image/*"
                  multiple
                  hidden
                  onChange={(e) => {
                    addFiles(e.target.files);
                    e.target.value = '';
                  }}
                />
              </label>
            )}
            {DRAFT_LAUNCHES.includes(launch) && (
              <PromptsMenu
                client={client}
                text={text}
                className="btn btn-plain btn-icon draft-attach"
                onPick={(p) => {
                  // 填进输入框，不发送；已经写了字就接在后面
                  setText((cur) => (cur.trim() ? `${cur.trimEnd()}\n\n${p}` : p));
                  input.current?.focus();
                }}
              />
            )}
            <PopMenu
              sheet
              className={`draft-chip draft-type draft-type-${launch} ${missing(launch) ? 'warn' : ''}`}
              icon={(p) => <LaunchIcon launch={launch} {...p} />}
              text={missing(launch) ? `${toolName} · ${t('sess.not_installed')}` : toolName}
              label={t('draft.pick_type')}
              items={typeItems}
            />
            {launch === 'claude' && !container && <ClaudeProfilePick client={client} device={device} value={profile} onChange={setProfile} className="draft-chip" />}
            {launch === 'codex' && !container && <CodexProfilePick client={client} device={device} value={profile} onChange={setProfile} className="draft-chip" />}
            {canFull && (
              <button type="button" className="draft-chip draft-full" aria-pressed={fullAccess} onClick={() => setFullAccess(!fullAccess)} title={t(`form.full_access_hint_${launch}`)}>
                <ShieldCheck size={14} aria-hidden="true" />
                {t('sess.full_access_chip')}
              </button>
            )}
            <button type="button" className="draft-chip" onClick={() => setAdv(!adv)} aria-expanded={adv} aria-label={t('draft.more')} title={t('draft.more')}>
              <MoreHorizontal size={16} aria-hidden="true" />
            </button>
            <span className="grow" />
            <button type="submit" className={`draft-go ${empty || sendLabel !== t('sess.start') ? 'wide' : ''}`} disabled={busy || installing || Boolean(blocked)} aria-label={sendLabel} title={blocked ?? t('sess.start_hint')}>
              {busy || installing ? <Spinner size={16} /> : empty || missing(launch) ? sendLabel : <ArrowUp size={18} />}
            </button>
          </div>
          {container && (
            <div className="start-container">
              <label className="start-container-dir">
                <span className="muted">{t('sess.container_dir')}</span>
                <input className="mono" value={container.dir} onChange={(e) => setContainer({ ...container, dir: e.target.value })} spellCheck={false} aria-label={t('sess.container_dir')} />
                <Button variant="secondary" icon={FolderOpen} onClick={() => setDirPick(true)} aria-label={t('fs.browse')} title={t('fs.browse')} />
              </label>
              <FolderPicker
                open={dirPick}
                initial={container.dir}
                client={containerFs(device, container.name)}
                onClose={() => setDirPick(false)}
                onPick={(dir) => setContainer((c) => c && { ...c, dir })}
              />
              <p className="small muted">
                {t('sess.container_note')}
                {container.root && canFull && fullAccess && launch === 'claude' ? ` ${t('sess.container_root')}` : ''}
              </p>
            </div>
          )}
          {adv && (
            <div className="start-advanced">
              <label className="field">
                <span className="field-label">{t('form.name')}</span>
                <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={t('sess.name_ph')} />
              </label>
              <ShellPicker value={shell} onChange={setShell} data={shells} inherit={status.settings?.default_shell || null} />
              {status.capabilities?.persistent && (
                <div className="between">
                  <span className="grow">
                    <b style={{ fontSize: 14 }}>{t('form.auto_start')}</b>
                    <span className="field-hint" style={{ display: 'block' }}>
                      {t('sess.adv_remember')}
                    </span>
                  </span>
                  <Switch checked={autoStart} onChange={setAutoStart} label={t('form.auto_start')} />
                </div>
              )}
              {!container && <DockerPick device={device} onPick={(c) => pickProject(device, '', c)} />}
            </div>
          )}
        </form>
        <ErrorNote error={error} />
        <DraftTip />
      </div>
    </div>
  );
}

/** 这个项目里最近的几个对话：点“接着聊”新建一个会话接着它。 */
function RecentConvs({ device, items }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const ui = useSessionUi();
  const toast = useToast();
  const { navigate } = useRouter();
  const [busy, setBusy] = useState(null);
  const go = async (c) => {
    if (busy) return;
    // 最近还在写的（可能正开在别处）：先去项目页看一眼，跟侧栏一样，不直接接着
    if (c.live && !c.instance_id) return navigate(`${projectPath({ device, workdir: c.workdir })}&c=${encodeURIComponent(`${c.agent}:${c.id}`)}`);
    setBusy(c.id);
    try {
      await ui.continueConv(device, c);
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="draft-recent">
      <span className="draft-recent-label">{t('draft.recent')}</span>
      {items.map(({ conv: c }) => {
        const when = Date.parse(c.updated_at) || 0;
        return (
          <button key={`${c.agent}:${c.id}`} type="button" className="draft-conv" onClick={() => go(c)} disabled={Boolean(busy) || !status.account.allowed} title={[c.title, c.last].filter(Boolean).join('\n')}>
            <LaunchIcon launch={c.agent} size={16} />
            <span className="ellipsis grow">{c.title}</span>
            {when > 0 && <span className="draft-conv-when">{shortAgo(t, when)}</span>}
            {busy === c.id ? <Spinner size={13} /> : <span className="draft-conv-go">{t('draft.continue')}</span>}
          </button>
        );
      })}
    </div>
  );
}
