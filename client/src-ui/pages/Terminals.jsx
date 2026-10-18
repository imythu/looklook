// 首页：我的终端。每个终端是一个可以长期运行的工作区（例如让 Claude Code 在里面干活），关掉网页也不会停。
import {
  ArrowUp,
  Bot,
  Braces,
  Check,
  ChevronDown,
  ChevronRight,
  Command,
  CopyPlus,
  ExternalLink,
  FileText,
  Fish,
  Folder,
  FolderOpen,
  FolderPlus,
  Globe,
  HardDrive,
  Home,
  LayoutGrid,
  List,
  Monitor,
  MonitorSmartphone,
  PanelRightClose,
  PanelRightOpen,
  PanelsTopLeft,
  PanelTop,
  Pencil,
  Play,
  Plus,
  QrCode as QrCodeIcon,
  RotateCcw,
  Search,
  Sparkles,
  Square,
  SquareTerminal,
  Trash2,
  Wifi,
  X,
} from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { SafetyNote, useStatus } from "../App.jsx";
import { api, ApiError, deviceApi, rememberTerminals, terminalDevice } from "../shared/api.js";
import { CHAT_LAUNCHES } from "../shared/chat.js";
import PopMenu from "../shared/PopMenu.jsx";
import { termTheme } from "../shared/termTheme.js";
import { deviceName, osLabel, RenameDeviceDialog, useDevices } from "../shared/devices.jsx";
import { urlHost } from "../shared/host.js";
import { errorText } from "../shared/i18n.js";
import { Link, useRouter } from "../shared/router.jsx";
import { relativeTime } from "../shared/time.js";
import { TermPane, useTermChrome } from "./TerminalView.jsx";
import { ModelChoiceLine } from "./ModelProviders.jsx";
import { isTouch } from "../shared/KeyBar.jsx";
import RelayStatus from "../shared/RelayStatus.jsx";
import {
  Button,
  Card,
  CopyButton,
  Empty,
  ErrorNote,
  Field,
  FoldNote,
  Modal,
  Note,
  QrCode,
  Skeleton,
  Spinner,
  Switch,
  useAction,
  useConfirm,
  useLoad,
  useToast,
} from "../shared/ui.jsx";

export const LAUNCH_ICON = {
  shell: SquareTerminal,
  codex: Bot,
  claude: Sparkles,
  opencode: Braces,
  dsh: Fish,
  custom: Command,
};
const LAUNCHES = ["shell", "codex", "claude", "opencode", "dsh", "custom"];
// 能一键安装的 AI 工具（没装时提示安装）；能以全部权限启动的
const AI_TOOLS = ["codex", "claude", "opencode", "dsh"];
const FULL_ACCESS = ["codex", "claude", "opencode"];

function RemoteCard() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const [qr, setQr] = useState(false);
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState(() => {
    try {
      return localStorage.getItem("remote_mode") === "lan" ? "lan" : "public";
    } catch {
      return "public";
    }
  });
  const [lanIdx, setLanIdx] = useState(0);
  const pick = (m) => {
    setMode(m);
    try {
      localStorage.setItem("remote_mode", m);
    } catch {
      /* 无痕模式等：只是不记住选择 */
    }
  };
  const c = status.console ?? {};
  const ips = c.lan_ips ?? [];
  const lan = mode === "lan";
  const lanUrl = ips.length
    ? `http://${urlHost(ips[Math.min(lanIdx, ips.length - 1)])}:${c.port}/`
    : null;
  // 局域网：没开放或没找到地址时没有可扫的地址，卡片改为说明怎么开启
  const lanReady = !lan || (c.allow_lan && lanUrl);
  const url = lan ? lanUrl : status.account.session.user_host_url;
  const host = url?.replace(/^https?:\/\//, "").replace(/\/$/, "");
  const body = lan
    ? !c.allow_lan
      ? "terminals.lan_off"
      : !lanUrl
        ? "terminals.lan_unknown"
        : c.code_enabled
          ? "terminals.lan_body_code"
          : "terminals.lan_body"
    : status.account.allowed
      ? "terminals.remote_body"
      : "terminals.remote_paused";
  return (
    <Card className={`remote-card ${open ? "" : "remote-collapsed"}`}>
      <button
        type="button"
        className="remote-toggle"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      >
        <MonitorSmartphone size={16} aria-hidden="true" />
        <b className="grow">{t("terminals.remote_title")}</b>
        <ChevronDown className="remote-chev" size={16} aria-hidden="true" />
      </button>
      {open && (
        <div className="remote-main">
          <div
            className="seg"
            role="group"
            aria-label={t("terminals.route_label")}
          >
            <button
              type="button"
              aria-pressed={!lan}
              onClick={() => pick("public")}
            >
              <Globe size={15} aria-hidden="true" />{" "}
              {t("terminals.route_public")}
            </button>
            <button
              type="button"
              aria-pressed={lan}
              onClick={() => pick("lan")}
            >
              <Wifi size={15} aria-hidden="true" /> {t("terminals.route_lan")}
            </button>
          </div>
          <p className="muted small remote-body">{t(body)}</p>
          {!lan && <RelayStatus />}
          {lan && ips.length > 1 && lanReady && (
            <select
              className="remote-ips"
              value={lanIdx}
              onChange={(e) => setLanIdx(Number(e.target.value))}
              aria-label={t("terminals.lan_pick")}
            >
              {ips.map((ip, i) => (
                <option key={ip} value={i}>
                  {ip}
                </option>
              ))}
            </select>
          )}
          {lanReady ? (
            <div className="remote-address">
              <span className="address-pill">
                <span>{host}</span>
                <CopyButton text={url} label={false} variant="plain" />
              </span>
              <a
                className="btn btn-plain remote-open"
                href={url}
                target="_blank"
                rel="noopener noreferrer"
              >
                <ExternalLink size={16} aria-hidden="true" />
                {t("terminals.remote_open")}
              </a>
              <button
                type="button"
                className="btn btn-plain remote-qr-toggle"
                aria-expanded={qr}
                onClick={() => setQr(!qr)}
              >
                <QrCodeIcon size={16} aria-hidden="true" />
                {t(
                  qr ? "terminals.remote_qr_hide" : "terminals.remote_qr_show",
                )}
              </button>
            </div>
          ) : (
            <div className="remote-address">
              <Link to="/settings" className="btn btn-plain remote-open">
                {t("terminals.lan_go_settings")}
              </Link>
            </div>
          )}
        </div>
      )}
      {open && lanReady && (
        <div className={`remote-qr ${qr ? "show" : ""}`}>
          <QrCode
            text={url}
            label={t(
              lan
                ? "terminals.remote_qr_label_lan"
                : "terminals.remote_qr_label",
            )}
          />
        </div>
      )}
    </Card>
  );
}

function BackendNote() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const c = status.capabilities;
  if (!c.ttyd) return <Note kind="error">{t("terminals.no_ttyd")}</Note>;
  if (c.persistent) return null;
  return (
    <FoldNote kind="warn" summary={t("terminals.not_persistent.short")}>
      {t(
        `terminals.not_persistent.${c.os === "darwin" ? "darwin" : c.os === "windows" ? "windows" : "linux"}`,
      )}
    </FoldNote>
  );
}

function stateOf(i) {
  if (i.running) return i.task_alive === false ? "ready" : "running";
  return i.task_alive ? "paused" : "stopped";
}

const BADGE = {
  running: "badge-ok",
  ready: "",
  paused: "badge-warn",
  stopped: "",
};

/** 对话界面的状态（docs/CLAUDE_UI.md）：等待确认、等你回答、要到终端里操作。没有需要人处理的事时不显示。 */
function ChatBadge({ inst }) {
  const { t } = useTranslation();
  const c = inst.chat;
  if (!inst.running || !c) return null;
  const key = c.pending ? `chat.badge_${c.pending}` : c.phase === "tui" ? "chat.badge_tui" : null;
  if (!key) return null;
  return <span className="badge badge-warn chat-badge">{t(key)}</span>;
}

// 手机/平板打开整页终端 /t/{id}（带按键条）；电脑上新标签页直接打开 /i/{id}/。
// 新窗口打开管理台的终端页（带文件面板、粘贴、拖放），不再直接打开裸 /i/{id}/。
const tabUrl = (inst) => `/t/${inst.id}`;

// DSH 是网页界面：打开它就是打开它的本机网页映射（本机、外面都是同一个地址）。
// 先在点击里同步开一个空白窗口（不会被当作弹窗拦截），后台确保 DSH 在运行、映射可用后再跳过去。
async function openDsh(inst, t, toast) {
  const w = window.open("about:blank", "_blank");
  try {
    const r = await api.post(`/instances/${inst.id}/page`);
    if (w) w.location.href = r.url;
    else window.open(r.url, "_blank", "noopener");
    if (!r.ready) toast(t("dsh.starting"));
  } catch (e) {
    w?.close();
    toast(errorText(t, e), "error");
  }
}

/**
 * 一个终端的各种操作（卡片、列表行、工作台侧栏共用）：打开、新窗口、停止、重启、删除……
 * onOpenHere(id)：在本页打开（电脑上进工作台，手机上整页打开）。
 */
function useInstanceActions(inst, { onChanged, onEdit, onLogs, onOpenHere }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const confirm = useConfirm();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const state = stateOf(inst);
  // 终端服务或后台任务还在就算“在运行”，这时不能删除，要先停止。
  const live = inst.running || Boolean(inst.task_alive);
  const persistent = status.capabilities.persistent;
  const allowed = status.account.allowed;
  const dsh = inst.launch === "dsh";
  const act = async (fn, okText) => {
    setBusy(true);
    try {
      await fn();
      if (okText) toast(okText);
    } catch (e) {
      toast(errorText(t, e), "error");
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  // 需要时先启动；成功返回 true。
  const ensureRunning = async () => {
    if (inst.running) return true;
    setBusy(true);
    try {
      await api.post(`/instances/${inst.id}/start`);
      return true;
    } catch (e) {
      toast(errorText(t, e), "error");
      return false;
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  // 在本页打开（不改变标签页地址）。
  const openHere = async () => {
    if (await ensureRunning()) onOpenHere(inst.id);
  };
  const openPage = async () => {
    setBusy(true);
    await openDsh(inst, t, toast);
    setBusy(false);
    onChanged();
  };
  // 新标签页打开：未运行时先同步开一个空白窗口（避免被拦截），启动后再导航过去。
  const openTab = async () => {
    if (inst.running) {
      window.open(tabUrl(inst), "_blank", "noopener");
      return;
    }
    const w = window.open("about:blank", "_blank");
    if (await ensureRunning()) {
      if (w) w.location.href = tabUrl(inst);
    } else {
      w?.close();
    }
  };
  const stop = async () => {
    const ok = await confirm({
      title: t("instance.stop_title", { name: inst.name }),
      body: t(persistent ? "instance.stop_body" : "instance.stop_body_direct"),
      confirm: t("instance.stop_confirm"),
      danger: true,
    });
    if (ok) act(() => api.post(`/instances/${inst.id}/stop`), t("instance.stopped"));
  };
  const restart = async () => {
    const ok = await confirm({
      title: t("instance.restart_title", { name: inst.name }),
      body: t("instance.restart_body"),
      confirm: t("instance.restart_confirm"),
      danger: true,
    });
    if (ok) act(() => api.post(`/instances/${inst.id}/restart`), t("instance.restarted"));
  };
  const remove = async () => {
    const ok = await confirm({
      title: t("instance.delete_title", { name: inst.name }),
      body: t(live ? "instance.delete_body_running" : "instance.delete_body"),
      confirm: t("action.delete"),
      danger: true,
    });
    if (ok) act(() => api.del(`/instances/${inst.id}`), t("instance.deleted"));
  };
  // 复制终端：同一台电脑上照原样（启动方式、文件夹、shell、权限）新建一个并启动，然后在本页打开。
  // 不复制开机自动运行，免得重启后一下子多跑一份；DSH 一台电脑只有一个，不能复制。
  const duplicate = async () => {
    const dev = terminalDevice(inst.id);
    const client = deviceApi(dev);
    setBusy(true);
    try {
      const r = await client.post("/instances", {
        name: t("instance.duplicate_name", { name: inst.name }).slice(0, 40),
        workdir: inst.workdir,
        launch: inst.launch,
        command: inst.launch === "custom" ? inst.command : "",
        shell: inst.shell ?? "",
        auto_start: false,
        full_access: inst.full_access,
        root_confirmed: inst.root_confirmed,
        chat_ui: inst.chat_ui,
        start: true,
      });
      if (dev && r.id) rememberTerminals(dev, [r]);
      toast(t("instance.duplicated", { name: r.name }));
      onOpenHere(r.id);
    } catch (e) {
      toast(errorText(t, e), "error");
    } finally {
      setBusy(false);
      onChanged();
    }
  };
  // “更多”菜单。full：列表行和侧栏没有单独的按钮，把打开方式、停止也放进来
  const menu = (full) => [
    full && dsh && { icon: SquareTerminal, label: t("dsh.output"), onClick: openHere, disabled: busy || !allowed },
    full && !dsh && { icon: ExternalLink, label: t("instance.new_window"), onClick: openTab, disabled: busy || !allowed },
    { icon: Pencil, label: t("instance.edit"), onClick: () => onEdit(inst) },
    !dsh && { icon: CopyPlus, label: t("instance.duplicate"), onClick: duplicate, disabled: busy || !allowed },
    inst.running && { icon: RotateCcw, label: t("instance.restart"), onClick: restart },
    full && live && { icon: Square, label: t("instance.stop"), onClick: stop, disabled: busy },
    { icon: FileText, label: t("instance.logs"), onClick: () => onLogs(inst) },
    {
      icon: Trash2,
      label: t("action.delete"),
      onClick: remove,
      danger: true,
    },
  ];
  // 点名称 / 整行：DSH 打开它的网页，其他终端在本页打开
  const primary = dsh ? openPage : openHere;
  return { busy, state, live, dsh, allowed, openHere, openPage, openTab, stop, duplicate, menu, primary };
}

/** 终端的一行说明：启动方式 · 自定义命令 · shell。 */
function LaunchLine({ inst }) {
  const { t } = useTranslation();
  return (
    <span className="one-line">
      <span className="nowrap">{t(`launch.${inst.launch}.title`)}</span>
      {inst.launch === "custom" && <span className="mono ellipsis">· {inst.command}</span>}
      {inst.shell && (
        <span className="mono ellipsis" title={t("shell.label")}>
          · {inst.shell}
        </span>
      )}
    </span>
  );
}

function InstanceError({ inst }) {
  const { t } = useTranslation();
  if (!inst.error) return null;
  return (
    <span style={{ color: "var(--danger)" }}>
      {t(`instance.error.${inst.error}`, {
        defaultValue: t("instance.error.other"),
      })}
    </span>
  );
}

function InstanceCard({ inst, ...handlers }) {
  const { t } = useTranslation();
  const a = useInstanceActions(inst, handlers);
  const { busy, state, live, dsh, allowed } = a;
  const Icon = LAUNCH_ICON[inst.launch] ?? SquareTerminal;
  return (
    <article className="instance">
      <div className="instance-head">
        <span className={`instance-icon ${state === "running" ? "live" : ""}`}>
          <Icon size={22} aria-hidden="true" />
        </span>
        <span className="grow">
          <span className="instance-name ellipsis">{inst.name}</span>
          <span className={`badge ${BADGE[state]}`}>
            {t(`instance.state.${state}`)}
          </span>
          <ChatBadge inst={inst} />
        </span>
      </div>
      <div className="instance-meta">
        <LaunchLine inst={inst} />
        <span className="mono ellipsis" title={inst.workdir}>
          {inst.workdir}
        </span>
        {inst.last_activity_at && state === "running" && (
          <span>
            {t("instance.last_activity", {
              when: relativeTime(t, inst.last_activity_at),
            })}
          </span>
        )}
        <InstanceError inst={inst} />
      </div>
      <div className="instance-actions">
        {dsh ? (
          <>
            <Button className="grow-btn" icon={ExternalLink} busy={busy} disabled={!allowed} onClick={a.openPage} title={t("dsh.open_hint")}>
              {t("dsh.open")}
            </Button>
            {/* DSH 自己的输出（启动报错、日志）还在终端里 */}
            <Button variant="secondary" icon={SquareTerminal} disabled={busy || !allowed} onClick={a.openHere} aria-label={t("dsh.output")} title={t("dsh.output")} />
          </>
        ) : (
          <>
            <Button
              className="grow-btn"
              icon={inst.running ? PanelTop : Play}
              busy={busy}
              disabled={!allowed}
              onClick={a.openHere}
              title={t(isTouch() ? "instance.open_page_hint" : "instance.open_here_hint")}
            >
              {inst.running ? t("instance.open_here") : t("instance.start_open")}
            </Button>
            <Button
              variant="secondary"
              icon={ExternalLink}
              disabled={busy || !allowed}
              onClick={a.openTab}
              aria-label={t("instance.new_window")}
              title={t("instance.new_window")}
            />
          </>
        )}
        {live && (
          <Button variant="ghost" icon={Square} disabled={busy} onClick={a.stop}>
            {t("instance.stop")}
          </Button>
        )}
        <span className="menu-pop">
          <PopMenu items={a.menu(false)} />
        </span>
      </div>
    </article>
  );
}

/**
 * 列表视图的一行：终端多的时候一屏能看到很多。点整行打开，右边是“更多”。
 * compact：工作台侧栏里的样子（更窄，只有名称和一行说明）；current：工作台里正在看的那个。
 */
function InstanceRow({ inst, compact, current, opened, ...handlers }) {
  const { t } = useTranslation();
  const a = useInstanceActions(inst, handlers);
  const Icon = LAUNCH_ICON[inst.launch] ?? SquareTerminal;
  const sub = inst.workdir;
  return (
    <div className={`irow ${compact ? "irow-compact" : ""} ${current ? "current" : ""} ${opened ? "opened" : ""}`} data-state={a.state}>
      <button
        type="button"
        className="irow-main"
        disabled={a.busy || !a.allowed}
        onClick={a.primary}
        aria-current={current ? "true" : undefined}
        title={`${inst.name} · ${t(`instance.state.${a.state}`)}${inst.workdir ? `\n${inst.workdir}` : ""}`}
      >
        <span className={`irow-icon ${a.state === "running" ? "live" : ""}`}>
          <Icon size={compact ? 16 : 18} aria-hidden="true" />
          <span className={`dot ${a.state === "running" ? "on" : a.state === "paused" ? "warn" : ""}`} aria-hidden="true" />
        </span>
        <span className="irow-text">
          <span className="irow-name ellipsis">
            {inst.name}
          </span>
          <span className="irow-sub ellipsis">
            {compact ? (
              <span className="mono">{shortPath(sub)}</span>
            ) : (
              <>
                {t(`launch.${inst.launch}.title`)}
                {sub && <span className="mono"> · {sub}</span>}
              </>
            )}
          </span>
          {!compact && inst.error && (
            <span className="irow-sub">
              <InstanceError inst={inst} />
            </span>
          )}
        </span>
        {!compact && (
          <span className="irow-meta">
            <span className={`badge ${BADGE[a.state]}`}>{t(`instance.state.${a.state}`)}</span>
            <ChatBadge inst={inst} />
            {inst.last_activity_at && a.state === "running" && <span className="small muted nowrap">{relativeTime(t, inst.last_activity_at)}</span>}
          </span>
        )}
      </button>
      {a.busy && <span className="irow-busy"><Spinner size={15} /></span>}
      <PopMenu items={a.menu(true)} className="btn btn-plain btn-icon irow-more" />
    </div>
  );
}

/** 侧栏里的路径太长：只留最后两级。 */
function shortPath(p) {
  if (!p) return "";
  const parts = p.split(/[\\/]/).filter(Boolean);
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : p;
}

const sepOf = (p) =>
  /^[A-Za-z]:[\\/]/.test(p) || p.includes("\\") ? "\\" : "/";
const joinPath = (dir, name) =>
  dir.endsWith("/") || dir.endsWith("\\")
    ? dir + name
    : dir + sepOf(dir) + name;

/** 面包屑：把路径拆成可点击的各级目录。 */
function crumbsOf(path) {
  const win = sepOf(path) === "\\";
  const out = win ? [] : [{ label: "/", path: "/" }];
  let acc = "";
  path
    .split(/[\\/]/)
    .filter(Boolean)
    .forEach((name, i) => {
      acc = win && i === 0 ? `${name}\\` : joinPath(acc, name);
      out.push({ label: name, path: acc });
    });
  return out;
}

/** 没有权限读取文件夹：按本机系统说明怎么授权（macOS 隐私保护 / Windows 安全设置 / Unix 文件权限）。 */
function DeniedNote({ data, onRetry }) {
  const { t } = useTranslation();
  const os = ["macos", "windows"].includes(data.os) ? data.os : "unix";
  return (
    <div style={{ marginBottom: 6 }}>
      <Note kind="warn">
        <strong>{t("fs.denied_title", { path: data.path })}</strong>
        <div className="small" style={{ marginTop: 4 }}>
          {t(`fs.denied_${os}`)}
        </div>
        {data.app_path && (
          <div className="small" style={{ marginTop: 4 }}>
            {t("fs.denied_app_path")}{" "}
            <code className="mono">{data.app_path}</code>
          </div>
        )}
        <div className="row" style={{ gap: 8, marginTop: 6 }}>
          <Button variant="secondary" icon={RotateCcw} onClick={onRetry}>
            {t("fs.retry")}
          </Button>
        </div>
      </Note>
    </div>
  );
}

/**
 * 文件夹选择弹窗：点文件夹进入，“选择此文件夹”确定当前位置。Windows 上最顶层是“此电脑”（各个盘符）。
 * files：改成选文件（如 SSH 私钥）：文件也列出来，点一下选中、双击直接确定；initial 是文件路径时从它所在的文件夹开始。
 * title / pickLabel：替换默认的标题和确定按钮文字。
 */
export function FolderPicker({ open, initial, onClose, onPick, client = api, files = false, title, pickLabel }) {
  const { t } = useTranslation();
  const [cur, setCur] = useState("");
  const [typed, setTyped] = useState("");
  const [drives, setDrives] = useState(false);
  const [data, setData] = useState(null);
  const [err, setErr] = useState(null);
  const [hidden, setHidden] = useState(false);
  const [newName, setNewName] = useState("");
  const [loading, setLoading] = useState(false);
  const [sel, setSel] = useState("");
  const seq = useRef(0);
  const load = async (path) => {
    const n = ++seq.current;
    setLoading(true);
    try {
      const r = await client.get(
        `/fs/list?path=${encodeURIComponent(path)}&hidden=${hidden ? 1 : 0}${files ? "&files=1" : ""}`,
      );
      if (n !== seq.current) return;
      setData(r);
      setErr(null);
    } catch (e) {
      if (n === seq.current) setErr(e);
    } finally {
      if (n === seq.current) setLoading(false);
    }
  };
  useEffect(() => {
    if (!open) return;
    // 选文件时从文件所在的文件夹开始，并预先选中它
    const start = files && initial ? initial.replace(/[\\/][^\\/]*$/, "") || initial : initial ?? "";
    setCur(start);
    setTyped(start);
    setSel(files ? initial ?? "" : "");
    setHidden(files);
    setDrives(false);
    setNewName("");
    setData(null);
    setErr(null);
  }, [open]);
  useEffect(() => {
    if (open && !drives) load(cur);
  }, [open, cur, hidden, drives]);
  const win = data?.os === "windows";
  const go = (path) => {
    setDrives(false);
    if (files) setSel("");
    setCur(path);
    setTyped(path);
  };
  const showDrives = () => {
    setDrives(true);
    setTyped("");
  };
  // 不存在的路径：显示的是最近的已有上级，“选择”仍然选用户要的那个（启动时自动创建）。
  const target = files ? sel || null : drives ? null : data && !data.exists ? cur : data?.path;
  const up = () => {
    if (drives) return;
    if (data?.parent) go(data.parent);
    else if (win) showDrives();
  };
  const addFolder = () => {
    const name = newName.trim();
    if (!name || !data || /[\\/:*?"<>|]/.test(name)) return;
    go(joinPath(data.path, name));
    setNewName("");
  };
  const entries = drives
    ? (data?.roots ?? []).map((r) => ({ name: r, path: r, drive: true }))
    : (data?.entries ?? []).map((e) => ({ ...e, path: joinPath(data.path, e.name) }));
  const pick = (path) => {
    // 选文件时主目录下的写成 ~/…，和手填的一样短
    const home = data?.home;
    onPick(files && home && path.startsWith(home + sepOf(path)) ? `~${path.slice(home.length)}` : path);
    onClose();
  };
  return (
    <Modal open={open} onClose={onClose} title={title ?? t("fs.title")} wide className="picker">
      <div className="picker-bar">
        <Button
          variant="ghost"
          icon={ArrowUp}
          disabled={drives || !data || (!data.parent && !win)}
          onClick={up}
          aria-label={t("fs.up")}
          title={t("fs.up")}
        />
        <Button
          variant="ghost"
          icon={Home}
          disabled={!data?.home}
          onClick={() => go(data.home)}
          aria-label={t("fs.home")}
          title={t("fs.home")}
        />
        <form
          className="grow"
          onSubmit={(e) => {
            e.preventDefault();
            e.stopPropagation();
            if (typed.trim()) go(typed.trim());
          }}
        >
          <input
            value={typed}
            onChange={(e) => setTyped(e.target.value)}
            className="mono picker-path"
            spellCheck={false}
            autoComplete="off"
            placeholder={drives ? t("fs.computer") : ""}
            aria-label={t("fs.path")}
            enterKeyHint="go"
          />
        </form>
      </div>
      <nav className="picker-crumbs" aria-label={t("fs.path")}>
        {win && (
          <button
            type="button"
            className={`btn btn-plain ${drives ? "current" : ""}`}
            onClick={showDrives}
          >
            <Monitor size={15} aria-hidden="true" />
            {t("fs.computer")}
          </button>
        )}
        {!drives &&
          data &&
          crumbsOf(data.path).map((c, i, a) => (
            <button
              key={c.path}
              type="button"
              className={`btn btn-plain mono ${i === a.length - 1 ? "current" : ""}`}
              onClick={() => go(c.path)}
            >
              {c.label}
            </button>
          ))}
      </nav>
      {err ? (
        <ErrorNote error={err} />
      ) : !data ? (
        <Skeleton height={240} />
      ) : (
        <>
          {!drives &&
            (data.denied ? (
              <DeniedNote data={data} onRetry={() => load(cur)} />
            ) : (
              !data.exists && !files && (
                <Note>
                  <span className="mono">{cur}</span> · {t("fs.will_create")}
                </Note>
              )
            ))}
          <ul className={`picker-list ${loading ? "loading" : ""}`}>
            {entries.length === 0 && !data.denied && (
              <li className="muted small picker-empty">{t(files ? "fs.empty_files" : "fs.empty")}</li>
            )}
            {entries.map((e) => {
              const file = files && !e.drive && !e.is_dir;
              const Icon = e.drive ? HardDrive : file ? FileText : Folder;
              return (
                <li key={e.path}>
                  {file ? (
                    <button
                      type="button"
                      aria-pressed={sel === e.path}
                      onClick={() => setSel(e.path)}
                      onDoubleClick={() => pick(e.path)}
                    >
                      <Icon size={18} aria-hidden="true" />
                      <span className="mono ellipsis grow">{e.name}</span>
                    </button>
                  ) : (
                    <button type="button" onClick={() => go(e.path)}>
                      <Icon size={18} aria-hidden="true" />
                      <span className="mono ellipsis grow">
                        {e.drive ? e.name.replace(/\\$/, "") : e.name}
                      </span>
                      <ChevronRight size={16} className="muted" aria-hidden="true" />
                    </button>
                  )}
                </li>
              );
            })}
          </ul>
          {!drives && !data.denied && !files && (
            <div className="row" style={{ gap: 8, marginTop: 10 }}>
              <input
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    e.preventDefault();
                    addFolder();
                  }
                }}
                className="mono grow"
                placeholder={t("fs.new_placeholder")}
                aria-label={t("fs.new_placeholder")}
                spellCheck={false}
              />
              <Button
                variant="secondary"
                icon={FolderPlus}
                disabled={!newName.trim()}
                onClick={addFolder}
              >
                {t("fs.new")}
              </Button>
            </div>
          )}
          <label className="small picker-hidden">
            <input
              type="checkbox"
              checked={hidden}
              onChange={(e) => setHidden(e.target.checked)}
            />
            {t(files ? "fs.show_hidden_files" : "fs.show_hidden")}
          </label>
        </>
      )}
      <div className="picker-selected small">
        <span className="muted">{t("fs.selected")}</span>{" "}
        <span className="mono">{target ?? "—"}</span>
      </div>
      <div className="modal-actions">
        <Button variant="ghost" onClick={onClose}>
          {t("action.cancel")}
        </Button>
        <Button
          icon={Check}
          disabled={!target}
          onClick={() => pick(target)}
        >
          {pickLabel ?? t("fs.pick")}
        </Button>
      </div>
    </Modal>
  );
}

/** 工作目录：可直接输入，也可以在文件夹选择弹窗里点选、新建；目录不存在时启动会自动创建。 */
export function FolderField({ value, onChange, label, hint, client }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  return (
    <div className="field">
      <span className="field-label">{label ?? t("form.workdir")}</span>
      <div className="row" style={{ gap: 8 }}>
        <input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="mono grow"
          spellCheck={false}
          autoComplete="off"
          aria-label={label ?? t("form.workdir")}
        />
        <Button
          variant="secondary"
          icon={FolderOpen}
          aria-haspopup="dialog"
          onClick={() => setOpen(true)}
        >
          {t("fs.browse")}
        </Button>
      </div>
      <span className="field-hint">{hint ?? t("form.workdir_autocreate")}</span>
      <FolderPicker
        open={open}
        initial={value}
        onClose={() => setOpen(false)}
        onPick={onChange}
        client={client}
      />
    </div>
  );
}

const shellLabel = (data, value) =>
  data?.items.find((o) => o.value === value)?.label ?? value;

/**
 * 选择 shell：本机找到的 shell 一行排开，第一项“自动”（新建终端时跟随设置里的默认，设置里则是系统自动选择），
 * 最后“自定义”可以输入任意命令（如 bash、fish、C:\Program Files\Git\bin\bash.exe -l）。
 * value 为空表示自动/默认。inherit：设置里的默认 shell（只在新建终端时传入）。
 */
// 不填名字时电脑上取的名字（src/instances.rs 的 default_name；重名时那边再加序号）：`my-app · Claude Code`，普通终端就是文件夹名。
const TOOL_NAMES = { codex: "Codex", claude: "Claude Code", opencode: "OpenCode" };
function guessName(launch, workdir) {
  if (launch === "dsh") return "DSH";
  const dir = (workdir || "").trim().replace(/[\\/]+$/, "").split(/[\\/]/).pop()?.trim();
  if (!dir || dir === "~" || dir.endsWith(":")) return "";
  return TOOL_NAMES[launch] ? `${dir} · ${TOOL_NAMES[launch]}` : dir;
}

export function ShellPicker({ value, onChange, data, inherit }) {
  const { t } = useTranslation();
  const known = data?.items.some((o) => o.value === value);
  const [custom, setCustom] = useState(false);
  useEffect(() => {
    if (data && value && !known) setCustom(true);
  }, [data]);
  const customOn = custom || (value && data && !known);
  const autoName = inherit
    ? shellLabel(data, inherit)
    : (data?.auto?.label ?? "");
  const placeholder =
    data?.os === "windows"
      ? "bash   ·   C:\\Program Files\\Git\\bin\\bash.exe"
      : "fish   ·   /usr/local/bin/zsh -l";
  return (
    <div className="field">
      <span className="field-label">{t("shell.label")}</span>
      {!data ? (
        <Skeleton height={36} />
      ) : (
        <div className="chips" role="group" aria-label={t("shell.label")}>
          <button
            type="button"
            className="chip"
            aria-pressed={!value && !customOn}
            onClick={() => {
              setCustom(false);
              onChange("");
            }}
          >
            {t(inherit ? "shell.default" : "shell.auto")}
            {autoName && <span className="chip-sub">{autoName}</span>}
          </button>
          {data.items.map((o) => (
            <button
              key={o.value}
              type="button"
              className="chip"
              aria-pressed={value === o.value && !customOn}
              title={o.path}
              onClick={() => {
                setCustom(false);
                onChange(o.value);
              }}
            >
              {o.label}
            </button>
          ))}
          <button
            type="button"
            className="chip"
            aria-pressed={Boolean(customOn)}
            onClick={() => {
              setCustom(true);
              if (known) onChange("");
            }}
          >
            <Pencil size={14} aria-hidden="true" />
            {t("shell.custom")}
          </button>
        </div>
      )}
      {customOn && (
        <input
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="mono"
          style={{ marginTop: 8 }}
          spellCheck={false}
          autoComplete="off"
          autoFocus
          placeholder={placeholder}
          aria-label={t("shell.custom_label")}
        />
      )}
      <span className="field-hint">
        {t(customOn ? "shell.custom_hint" : "shell.hint")}
      </span>
    </div>
  );
}

/** 新建 / 编辑终端。 */
/** 后台安装的进度：最后几十行输出，自动滚到底；结束后给出结果。 */
function InstallProgress({ job, tool, onRetry, busy }) {
  const { t } = useTranslation();
  const ref = useRef(null);
  const lines = job.output.split("\n");
  const tail = lines.slice(-200).join("\n");
  useEffect(() => {
    const el = ref.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [tail]);
  const kind = job.state === "failed" ? "error" : job.state === "ok" ? "ok" : "info";
  return (
    <Note kind={kind}>
      <b>{t(`tools.job_${job.state}`, { tool })}</b>
      <pre ref={ref} className="install-log mono" aria-live="polite">
        {tail}
      </pre>
      {job.state === "running" && <p className="small muted" style={{ margin: "6px 0 0" }}>{t("tools.job_running_hint")}</p>}
      {job.state === "failed" && (
        <div className="row" style={{ marginTop: 8 }}>
          <span className="small">{t("tools.job_failed_hint")}</span>
          <Button variant="secondary" busy={busy} onClick={onRetry}>
            {t("tools.retry")}
          </Button>
        </div>
      )}
    </Note>
  );
}

export function InstanceDialog({ open, onClose, initial, preset, workdir: wantWorkdir, device: wantDevice, onSaved }) {
  const { t } = useTranslation();
  const { status: selectedStatus } = useStatus();
  const devices = useDevices();
  const editing = Boolean(initial);
  // 多台电脑：新建时先选电脑（默认当前选中的那台）；编辑时就是终端所在的电脑。
  const [device, setDevice] = useState(null);
  useEffect(() => {
    if (!open || !devices.multi) return;
    setDevice(editing ? terminalDevice(initial.id) : (wantDevice ?? devices.selected));
  }, [open]);
  const dev = devices.multi ? device : null;
  const client = deviceApi(dev);
  // 不是当前选中的电脑时，用那台电脑自己的设置（默认目录、shell）与能力；字段缺失时退回选中电脑的
  const [devStatus] = useLoad(
    () => (open && dev && dev !== devices.selected ? client.get("/status").catch(() => null) : Promise.resolve(null)),
    [open, dev],
  );
  const status = devStatus?.settings && devStatus?.capabilities ? devStatus : selectedStatus;
  const workdirTouched = useRef(false);
  const [launch, setLaunch] = useState("shell");
  const [command, setCommand] = useState("");
  const [shell, setShell] = useState("");
  const [name, setName] = useState("");
  const [workdir, setWorkdir] = useState("");
  const [autoStart, setAutoStart] = useState(false);
  const [fullAccess, setFullAccess] = useState(false);
  const [rootOk, setRootOk] = useState(false);
  const [chatUi, setChatUi] = useState(true);
  const [mirror, setMirror] = useState("npmmirror");
  const { busy, error, setError, run } = useAction();
  const [tools, , reloadTools] = useLoad(
    () => (open ? client.get("/tools") : Promise.resolve(null)),
    [open, dev],
  );
  const [shells] = useLoad(
    () => (open ? client.get("/shells") : Promise.resolve(null)),
    [open, dev],
  );
  useEffect(() => {
    if (!open) return;
    setLaunch(initial?.launch ?? preset ?? "shell");
    setCommand(initial?.command ?? "");
    setShell(initial?.shell ?? "");
    setName(initial?.name ?? "");
    setWorkdir(initial?.workdir ?? (wantWorkdir || status.settings.default_workdir));
    workdirTouched.current = Boolean(wantWorkdir);
    setAutoStart(initial?.auto_start ?? false);
    setFullAccess(initial?.full_access ?? false);
    setRootOk(initial?.root_confirmed ?? false);
    setChatUi(initial?.chat_ui ?? true);
    setError(null);
  }, [open]);
  // 换了电脑：默认目录跟着换（用户自己改过就不动）
  useEffect(() => {
    if (open && !editing && !workdirTouched.current) setWorkdir(status.settings?.default_workdir ?? "");
  }, [status]);
  const editWorkdir = (v) => {
    workdirTouched.current = true;
    setWorkdir(v);
  };
  const isRoot = Boolean(status.capabilities?.root);
  const canFull = FULL_ACCESS.includes(launch);
  // 对话界面要靠会话保持（tmux）往同一个 Claude Code 里送输入
  const canChat = CHAT_LAUNCHES.includes(launch) && Boolean(status.capabilities?.persistent);
  // DSH 带着自己的端口和网页映射，编辑时不能和别的启动方式互换
  const launches = editing
    ? LAUNCHES.filter((l) => (l === "dsh") === (initial.launch === "dsh"))
    : LAUNCHES;
  const needRoot = canFull && fullAccess && isRoot && !rootOk;
  const missing =
    tools && AI_TOOLS.includes(launch) && !tools.tools[launch];
  const submit = async (e) => {
    e.preventDefault();
    if (needRoot) return;
    const body = {
      name,
      workdir,
      launch,
      command: launch === "custom" ? command : "",
      shell: shell.trim(),
      auto_start: autoStart,
      full_access: canFull && fullAccess,
      root_confirmed: canFull && fullAccess && isRoot && rootOk,
      chat_ui: canChat && chatUi,
    };
    const r = await run(() =>
      editing
        ? api.patch(`/instances/${initial.id}`, body)
        : client.post("/instances", { ...body, start: true }),
    );
    if (!r) return;
    if (dev && r.id) rememberTerminals(dev, [r]);
    onClose();
    // 新建成功后不直接跳转，由列表页询问怎么打开（当前页小窗或新标签页）。
    onSaved?.(editing ? null : r);
  };
  // 安装在后台进行（不建终端），进度就显示在这里；关掉对话框也不影响，再打开还能看到。
  const [job, setJob] = useState(null);
  useEffect(() => setJob(null), [open, launch, dev]);
  useEffect(() => {
    if (!open || !AI_TOOLS.includes(launch)) return undefined;
    let alive = true;
    let timer = null;
    const poll = async () => {
      const j = await client.get(`/tools/install/${launch}`).catch(() => null);
      if (!alive) return;
      setJob(j);
      if (j?.state === "running") timer = setTimeout(poll, 1000);
      else if (j?.state === "ok") reloadTools();
    };
    poll();
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [open, launch, dev, job?.started_at]);
  const install = async () => {
    const r = await run(() => client.post("/tools/install", { tool: launch, mirror }));
    if (r) setJob(r);
  };
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={t(editing ? "form.edit_title" : "form.new_title")}
    >
      <form onSubmit={submit}>
        {devices.multi && !editing && (
          <Field label={t("devices.create_on")}>
            <select value={device ?? ""} onChange={(e) => setDevice(e.target.value)}>
              {devices.items.map((d) => (
                <option key={d.id} value={d.id} disabled={!d.online}>
                  {deviceName(t, d)}
                  {d.os ? ` · ${osLabel(d.os)}` : ""}
                  {d.online ? "" : ` · ${t("devices.offline")}`}
                </option>
              ))}
            </select>
          </Field>
        )}
        <p className="field-label" style={{ margin: "0 0 8px" }}>
          {t("form.launch")}
        </p>
        <div className="launch-grid" role="group" aria-label={t("form.launch")}>
          {launches.map((l) => {
            const Icon = LAUNCH_ICON[l];
            return (
              <button
                key={l}
                type="button"
                className="launch"
                aria-pressed={launch === l}
                onClick={() => setLaunch(l)}
              >
                <Icon size={20} aria-hidden="true" />
                <b>{t(`launch.${l}.title`)}</b>
                <span className="small muted">{t(`launch.${l}.desc`)}</span>
              </button>
            );
          })}
        </div>
        {job?.tool === launch && (missing || job.state === "ok") ? (
          <InstallProgress job={job} tool={t(`launch.${launch}.title`)} onRetry={install} busy={busy} />
        ) : missing && (
          <Note kind="warn">
            <b>{t("tools.missing", { tool: t(`launch.${launch}.title`) })}</b>
            {tools.tools.npm ? (
              <>
                <p style={{ margin: "4px 0 8px" }}>{t("tools.install_hint")}</p>
                <div className="row">
                  <select
                    value={mirror}
                    onChange={(e) => setMirror(e.target.value)}
                    aria-label={t("tools.mirror")}
                  >
                    {["npmmirror", "tencent", "official"].map((m) => (
                      <option key={m} value={m}>
                        {t(`tools.mirrors.${m}`)}
                      </option>
                    ))}
                  </select>
                  <Button variant="secondary" busy={busy} onClick={install}>
                    {t("tools.install")}
                  </Button>
                </div>
              </>
            ) : (
              <p style={{ margin: "4px 0 0" }}>
                {t("tools.need_node")}{" "}
                <a
                  href="https://nodejs.org/zh-cn/download"
                  target="_blank"
                  rel="noopener noreferrer"
                >
                  {t("tools.node_link")}
                </a>{" "}
                <button
                  type="button"
                  className="btn btn-plain"
                  style={{ minHeight: 28, padding: "0 4px" }}
                  onClick={reloadTools}
                >
                  {t("tools.recheck")}
                </button>
              </p>
            )}
          </Note>
        )}
        {launch === "custom" && (
          <Field label={t("form.command")} hint={t("form.command_hint")}>
            <input
              value={command}
              onChange={(e) => setCommand(e.target.value)}
              className="mono"
              spellCheck={false}
              autoComplete="off"
              required
              placeholder="npm run dev"
            />
          </Field>
        )}
        <Field label={t('form.name')} hint={t('form.name_hint')}>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={guessName(launch, workdir) || t(`launch.${launch}.title`)} />
        </Field>
        {launch === "dsh" && <Note>{t("dsh.form_note")}</Note>}
        <FolderField
          value={workdir}
          onChange={editWorkdir}
          client={client}
          hint={launch === "dsh" ? t("dsh.workdir_hint") : undefined}
        />
        <ShellPicker
          value={shell}
          onChange={setShell}
          data={shells}
          inherit={status.settings?.default_shell || null}
        />
        <ModelChoiceLine client={client} device={dev} launch={launch} />
        {canFull && (
          <div style={{ marginBottom: 12 }}>
            <label
              className="row"
              style={{ alignItems: "flex-start", gap: 8, cursor: "pointer" }}
            >
              <input
                type="checkbox"
                checked={fullAccess}
                onChange={(e) => setFullAccess(e.target.checked)}
                style={{ marginTop: 3 }}
              />
              <span>
                <b style={{ fontSize: 14 }}>{t("form.full_access")}</b>
                <span className="field-hint" style={{ display: "block" }}>
                  {t(`form.full_access_hint_${launch}`)}
                </span>
              </span>
            </label>
            {fullAccess && isRoot && (
              <Note kind="warn">
                <label
                  className="row"
                  style={{
                    alignItems: "flex-start",
                    gap: 8,
                    cursor: "pointer",
                  }}
                >
                  <input
                    type="checkbox"
                    checked={rootOk}
                    onChange={(e) => setRootOk(e.target.checked)}
                    style={{ marginTop: 3 }}
                  />
                  <span>
                    <b>{t("form.root_confirm")}</b>
                    <span style={{ display: "block" }}>
                      {t("form.root_confirm_hint")}
                    </span>
                  </span>
                </label>
              </Note>
            )}
          </div>
        )}
        {canChat && (
          <label
            className="row"
            style={{ alignItems: "flex-start", gap: 8, cursor: "pointer", marginBottom: 12 }}
          >
            <input
              type="checkbox"
              checked={chatUi}
              onChange={(e) => setChatUi(e.target.checked)}
              style={{ marginTop: 3 }}
            />
            <span>
              <b style={{ fontSize: 14 }}>{t("form.chat_ui")}</b>
              <span className="field-hint" style={{ display: "block" }}>
                {t(editing && !initial?.chat_ui ? "form.chat_ui_hint_restart" : "form.chat_ui_hint")}
              </span>
            </span>
          </label>
        )}
        {status.capabilities?.persistent && (
          <div className="between" style={{ marginBottom: 12 }}>
            <span className="grow">
              <b style={{ fontSize: 14 }}>{t("form.auto_start")}</b>
              <span className="field-hint" style={{ display: "block" }}>
                {t("form.auto_start_hint")}
              </span>
            </span>
            <Switch
              checked={autoStart}
              onChange={setAutoStart}
              label={t("form.auto_start")}
            />
          </div>
        )}
        {editing && initial.running && <Note>{t("form.restart_note")}</Note>}
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t("action.cancel")}
          </Button>
          <Button type="submit" busy={busy} disabled={needRoot}>
            {t(editing ? "action.save" : "form.create")}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

/** 新建成功后询问是否马上打开：当前页面小窗（与卡片上的“在此打开”相同）或新标签页。 */
function CreatedDialog({ inst, onClose, onOpenHere }) {
  const { t } = useTranslation();
  const here = () => {
    onOpenHere(inst.id);
    onClose();
  };
  const toast = useToast();
  const tab = () => {
    // 在点击里同步打开，不会被浏览器当作弹窗拦截。
    if (inst.launch === "dsh") openDsh(inst, t, toast);
    else window.open(tabUrl(inst), "_blank", "noopener");
    onClose();
  };
  const dsh = inst?.launch === "dsh";
  return (
    <Modal open={Boolean(inst)} onClose={onClose} title={t("created.title", { name: inst?.name ?? "" })}>
      <p className="muted" style={{ marginTop: 0 }}>{t("created.body")}</p>
      <div className="modal-actions">
        <Button variant="ghost" onClick={onClose}>
          {t("created.later")}
        </Button>
        {dsh ? (
          <Button icon={ExternalLink} onClick={tab}>
            {t("dsh.open")}
          </Button>
        ) : (
          <>
            <Button variant="secondary" icon={ExternalLink} onClick={tab}>
              {t("created.new_tab")}
            </Button>
            <Button icon={PanelTop} onClick={here}>
              {t("created.here")}
            </Button>
          </>
        )}
      </div>
    </Modal>
  );
}

function LogsDialog({ inst, onClose }) {
  const { t } = useTranslation();
  const [data, error] = useLoad(
    () =>
      inst ? api.get(`/instances/${inst.id}/logs`) : Promise.resolve(null),
    [inst?.id],
  );
  return (
    <Modal
      open={Boolean(inst)}
      onClose={onClose}
      title={t("instance.logs_title", { name: inst?.name ?? "" })}
    >
      <p className="muted small">{t("instance.logs_hint")}</p>
      <ErrorNote error={error} />
      {data ? (
        <>
          <p className="small" style={{ margin: "8px 0 4px" }}>
            <b>{t("instance.history_state")}</b>{" "}
            {t(`instance.state.${stateOf(data)}`)}
            {data.started_at && (
              <>
                {" "}
                ·{" "}
                {t("instance.history_started", {
                  when: relativeTime(t, data.started_at),
                })}
              </>
            )}
            {data.last_activity_at && (
              <>
                {" "}
                ·{" "}
                {t("instance.last_activity", {
                  when: relativeTime(t, data.last_activity_at),
                })}
              </>
            )}
          </p>
          <p className="small" style={{ margin: "8px 0 4px" }}>
            <b>{t("instance.history_output")}</b>
          </p>
          <div className="log-box">
            {data.output || t("instance.history_output_empty")}
          </div>
          <p className="small" style={{ margin: "12px 0 4px" }}>
            <b>{t("instance.history_service")}</b>
          </p>
          <div className="log-box">{data.text || t("instance.logs_empty")}</div>
        </>
      ) : (
        <Skeleton height={120} />
      )}
      <div className="modal-actions">
        <Button variant="ghost" onClick={onClose}>
          {t("action.close")}
        </Button>
      </div>
    </Modal>
  );
}

export function Promotions() {
  const { t, i18n } = useTranslation();
  const [data] = useLoad(
    () => api.get(`/promotions?locale=${i18n.language}`),
    [i18n.language],
  );
  if (!data?.items?.length) return null;
  return data.items.map((p) => (
    <a
      key={p.id}
      href={p.url}
      target="_blank"
      rel="noopener noreferrer sponsored"
      className="card promo between"
      style={{ color: "inherit" }}
    >
      <span className="grow">
        <span className="badge">{t("promotion.label")}</span> <b>{p.title}</b>
        {p.body && (
          <span
            className="muted small"
            style={{ display: "block", marginTop: 4 }}
          >
            {p.body}
          </span>
        )}
      </span>
      <ExternalLink size={18} className="muted" />
    </a>
  ));
}

const GROUP_TIMEOUT_MS = 10000;

/**
 * 多台电脑：分别向每台在线的电脑要终端列表，各自独立。一台卡住（超时）或出错只影响它自己那一组，
 * 其他电脑的终端先显示出来；同一台电脑上一次还没返回时不重复请求。
 */
function useDeviceGroups(online, key) {
  const [groups, setGroups] = useState({});
  const inflight = useRef(new Set());
  const alive = useRef(true);
  const ids = useRef([]);
  ids.current = online ? online.map((d) => d.id) : [];
  const loadOne = (id) => {
    if (inflight.current.has(id)) return;
    inflight.current.add(id);
    const ctl = new AbortController();
    const timer = setTimeout(() => ctl.abort(), GROUP_TIMEOUT_MS);
    deviceApi(id)
      .get("/instances", { signal: ctl.signal })
      .then(
        (r) => {
          const items = Array.isArray(r?.items) ? r.items : [];
          rememberTerminals(id, items);
          return { id, items };
        },
        // 超时：保留上一次拿到的列表，免得卡一下就整组消失
        (e) => ({ id, error: e?.name === "AbortError" ? new ApiError(0, "DEVICE_TIMEOUT") : e }),
      )
      .then((g) => {
        clearTimeout(timer);
        inflight.current.delete(id);
        if (!alive.current || !ids.current.includes(id)) return;
        setGroups((all) => ({ ...all, [id]: g.error ? { ...all[id], id, items: all[id]?.items ?? [], error: g.error } : g }));
      });
  };
  const reload = () => ids.current.forEach(loadOne);
  useEffect(() => {
    alive.current = true;
    setGroups((all) => Object.fromEntries(Object.entries(all).filter(([id]) => ids.current.includes(id))));
    reload();
    return () => {
      alive.current = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key]);
  if (!online) return [null, reload];
  const list = ids.current.map((id) => groups[id]).filter(Boolean);
  const items = list.flatMap((g) => g.items);
  // 还有电脑没返回、其他电脑也都没有终端时继续显示骨架，免得先闪一下“还没有终端”
  const settled = items.length > 0 || list.length === ids.current.length;
  return [settled ? { items, groups } : null, reload];
}

/** 一台电脑的终端：标题（名称、系统、在线状态）+ 卡片；离线的电脑只显示一行“离线”。 */
function DeviceGroup({ device, group, renderCard, match, listClass = "instances" }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const [renaming, setRenaming] = useState(false);
  const [renamed, setRenamed] = useState(null); // 平台下发新名称前先显示刚改的
  const all = group?.items ?? [];
  const items = match ? all.filter(match) : all;
  const name = renamed && renamed.from === device.name ? renamed.name : deviceName(t, device);
  // 搜索时没有匹配的电脑整组不显示
  if (match && device.online && group && items.length === 0) return null;
  return (
    <section id={`device-${device.id}`} className={`device-group ${device.online ? "" : "device-offline"}`} aria-label={name}>
      <h2 className="device-head">
        <Monitor size={17} aria-hidden="true" />
        <span className="ellipsis">{name}</span>
        {device.online && (
          <button type="button" className="btn btn-plain btn-icon device-rename" onClick={() => setRenaming(true)} aria-label={t("devices.rename")} title={t("devices.rename")}>
            <Pencil size={15} />
          </button>
        )}
        {device.os && <span className="muted small">{osLabel(device.os)}</span>}
        <span className={`badge ${device.online ? "badge-ok" : ""}`}>{t(device.online ? "devices.online" : "devices.offline")}</span>
        {device.online && group && (!group.error || all.length > 0) && <span className="muted small">{t("devices.count", { count: all.length })}</span>}
      </h2>
      {device.online && group?.error && <ErrorNote error={group.error} />}
      {device.online &&
        (group?.error && items.length === 0 ? null : !group ? (
          <Skeleton height={120} />
        ) : items.length === 0 ? (
          <p className="muted small device-empty">{t("devices.no_terminals")}</p>
        ) : (
          <div className={listClass}>{items.map(renderCard)}</div>
        ))}
      {renaming && (
        <RenameDeviceDialog
          device={{ ...device, name }}
          onClose={() => setRenaming(false)}
          onDone={(n) => {
            setRenaming(false);
            setRenamed({ from: device.name, name: n });
            setTimeout(() => devices.reload?.(), 1500);
          }}
        />
      )}
    </section>
  );
}

// ---------------- 视图、搜索、工作台 ----------------

const VIEW_KEY = "looklook.view";
const WS_KEY = "looklook.ws";
const SCROLL_KEY = "looklook.scroll";
const QUERY_KEY = "looklook.query";
// 工作台最多同时开着几个终端标签（每个都连着终端，太多会拖慢页面）
const WS_MAX_TABS = 8;

function readJson(storage, key, fallback) {
  try {
    const v = JSON.parse(storage.getItem(key) ?? "null");
    return v ?? fallback;
  } catch {
    return fallback;
  }
}
function writeJson(storage, key, value) {
  try {
    storage.setItem(key, JSON.stringify(value));
  } catch {
    /* 无痕模式等：只是刷新后不恢复 */
  }
}

/** 能不能用工作台：电脑上、窗口够宽。手机上终端直接整页打开。 */
function useWorkspaceOk() {
  const [ok, setOk] = useState(() => !isTouch() && window.innerWidth >= 900);
  useEffect(() => {
    const on = () => setOk(!isTouch() && window.innerWidth >= 900);
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, []);
  return ok;
}

/** 当前视图：卡片 / 列表 / 工作台，记在本机浏览器里。手机默认列表。 */
function useView(wsOk) {
  const [view, setView] = useState(() => {
    const v = readJson(localStorage, VIEW_KEY, null);
    return ["grid", "list", "workspace"].includes(v) ? v : isTouch() ? "list" : "grid";
  });
  const pick = (v) => {
    setView(v);
    writeJson(localStorage, VIEW_KEY, v);
  };
  // 窗口变窄时工作台退回列表（不改记住的选择，变宽后回来）
  return [view === "workspace" && !wsOk ? "list" : view, pick];
}

/** 工作台开着的终端标签和当前标签，记在本机浏览器里：刷新、下次打开都还在。 */
function useWorkspaceTabs() {
  const [ws, setWs] = useState(() => {
    const v = readJson(localStorage, WS_KEY, {});
    const tabs = Array.isArray(v.tabs) ? v.tabs.filter((x) => typeof x === "string").slice(0, WS_MAX_TABS) : [];
    return { tabs, active: tabs.includes(v.active) ? v.active : (tabs[0] ?? null), side: v.side !== false, project: v.project !== false };
  });
  useEffect(() => writeJson(localStorage, WS_KEY, ws), [ws]);
  const open = (id) =>
    setWs((w) => {
      if (w.tabs.includes(id)) return { ...w, active: id };
      // 满了就关掉最早打开、又不是当前的那个
      let tabs = [...w.tabs, id];
      while (tabs.length > WS_MAX_TABS) tabs = tabs.filter((x, i) => i !== tabs.findIndex((y) => y !== w.active));
      return { ...w, tabs, active: id };
    });
  const close = (id) =>
    setWs((w) => {
      const i = w.tabs.indexOf(id);
      const tabs = w.tabs.filter((x) => x !== id);
      const active = w.active === id ? (tabs[Math.min(i, tabs.length - 1)] ?? null) : w.active;
      return { ...w, tabs, active };
    });
  const move = (id, to) =>
    setWs((w) => {
      const tabs = w.tabs.filter((x) => x !== id);
      tabs.splice(to, 0, id);
      return { ...w, tabs };
    });
  const toggleSide = () => setWs((w) => ({ ...w, side: !w.side }));
  const setProject = (project) => setWs((w) => ({ ...w, project }));
  return { ...ws, open, close, move, toggleSide, setProject };
}

/** 卡片 / 列表视图的滚动位置：刷新（例如远程会话续期时的自动跳转）后回到原处。 */
function useScrollMemory(ready, enabled) {
  const restored = useRef(false);
  useEffect(() => {
    if (!enabled) return undefined;
    if (ready && !restored.current) {
      restored.current = true;
      const y = Number(readJson(sessionStorage, SCROLL_KEY, 0));
      if (y > 0) requestAnimationFrame(() => window.scrollTo(0, y));
    }
    let frame = 0;
    const on = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(() => writeJson(sessionStorage, SCROLL_KEY, Math.round(window.scrollY)));
    };
    window.addEventListener("scroll", on, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", on);
    };
  }, [ready, enabled]);
}

/** 搜索：名称、文件夹、启动方式、命令、shell 都算。 */
function matcher(q, t) {
  const words = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return null;
  return (i) => {
    const hay = [i.name, i.workdir, i.command, i.shell, i.launch, t(`launch.${i.launch}.title`)].filter(Boolean).join(" ").toLowerCase();
    return words.every((w) => hay.includes(w));
  };
}

function ViewSwitch({ view, onChange, wsOk }) {
  const { t } = useTranslation();
  const opts = [
    ["grid", LayoutGrid],
    ["list", List],
    wsOk && ["workspace", PanelsTopLeft],
  ].filter(Boolean);
  return (
    <div className="seg view-switch" role="group" aria-label={t("view.label")}>
      {opts.map(([v, Icon]) => (
        <button key={v} type="button" aria-pressed={view === v} onClick={() => onChange(v)} title={t(`view.${v}_hint`)}>
          <Icon size={16} aria-hidden="true" />
          <span className="view-label">{t(`view.${v}`)}</span>
        </button>
      ))}
    </div>
  );
}

function SearchBox({ value, onChange, count, autoFocus }) {
  const { t } = useTranslation();
  return (
    <label className="search-box">
      <Search size={16} aria-hidden="true" />
      <input
        type="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => e.key === "Escape" && onChange("")}
        placeholder={t("view.search")}
        aria-label={t("view.search")}
        autoFocus={autoFocus}
        spellCheck={false}
      />
      {value && count != null && <span className="search-count">{count}</span>}
    </label>
  );
}

/** 让元素正好填满窗口剩下的高度（上面的横幅、提示有多高都行）。 */
function useFillHeight(ref) {
  const [h, setH] = useState(null);
  useLayoutEffect(() => {
    const on = () => {
      const el = ref.current;
      if (!el) return;
      const top = el.getBoundingClientRect().top + window.scrollY;
      setH(Math.max(360, window.innerHeight - top - 12));
    };
    on();
    const ro = new ResizeObserver(on);
    ro.observe(document.body);
    window.addEventListener("resize", on);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", on);
    };
  }, []);
  return h;
}

/** 路径的最后一级（项目名）。 */
function baseName(p) {
  const parts = (p ?? "").split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? p ?? "";
}

/** 按项目（同一台电脑上的同一个文件夹）分组，最近用过的项目排前面。 */
function projectGroups(items, multi) {
  const map = new Map();
  for (const i of items) {
    const device = multi ? terminalDevice(i.id) : null;
    const key = `${device ?? ""}\n${i.workdir}`;
    if (!map.has(key)) map.set(key, { key, device, workdir: i.workdir, items: [] });
    map.get(key).items.push(i);
  }
  const last = (g) => Math.max(0, ...g.items.map((i) => Date.parse(i.last_activity_at || i.started_at || "") || 0));
  return [...map.values()].sort((a, b) => last(b) - last(a) || a.workdir.localeCompare(b.workdir));
}

// 快速新建能选的启动方式（DSH 一台电脑只有一个、自定义命令要填命令，走完整的新建对话框）
const QUICK_LAUNCHES = ["shell", "claude", "codex", "opencode"];

/**
 * 像 ChatGPT 的项目那样快速新建：选好文件夹（在某个项目里点 + 时已经填好），点一下启动方式就建好、启动并打开。
 * 没装的 AI 工具、要改 shell 或权限时转到完整的新建对话框（文件夹带过去）。
 */
function QuickCreateDialog({ open, device: wantDevice, workdir: wantWorkdir, onClose, onMore, onCreated }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const dev = devices.multi ? (wantDevice ?? devices.selected) : null;
  const client = deviceApi(dev);
  const [workdir, setWorkdir] = useState("");
  const [busy, setBusy] = useState(null);
  const [error, setError] = useState(null);
  const [tools] = useLoad(() => (open ? client.get("/tools").catch(() => null) : Promise.resolve(null)), [open, dev]);
  useEffect(() => {
    if (!open) return;
    setWorkdir(wantWorkdir || status.settings.default_workdir || "");
    setBusy(null);
    setError(null);
  }, [open]);
  const missing = (l) => AI_TOOLS.includes(l) && tools && !tools.tools[l];
  const go = async (launch) => {
    if (missing(launch)) {
      onMore({ preset: launch, device: dev, workdir });
      return;
    }
    setBusy(launch);
    setError(null);
    try {
      const r = await client.post("/instances", {
        name: `${t(`launch.${launch}.title`)} · ${baseName(workdir)}`.slice(0, 40),
        workdir,
        launch,
        command: "",
        shell: "",
        auto_start: false,
        full_access: false,
        root_confirmed: false,
        start: true,
      });
      if (dev && r.id) rememberTerminals(dev, [r]);
      onClose();
      onCreated(r);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(null);
    }
  };
  const title = wantWorkdir ? t("ws.quick_title_in", { name: baseName(wantWorkdir) }) : t("ws.quick_title");
  return (
    <Modal open={open} onClose={onClose} title={title}>
      {devices.multi && (
        <p className="muted small" style={{ margin: "0 0 8px" }}>
          <Monitor size={13} aria-hidden="true" /> {deviceName(t, devices.byId(dev))}
        </p>
      )}
      <FolderField value={workdir} onChange={setWorkdir} client={client} hint={t("ws.quick_folder_hint")} />
      <p className="field-label" style={{ margin: "0 0 8px" }}>
        {t("ws.quick_launch")}
      </p>
      <div className="launch-grid" role="group" aria-label={t("ws.quick_launch")}>
        {QUICK_LAUNCHES.map((l) => {
          const Icon = LAUNCH_ICON[l];
          return (
            <button key={l} type="button" className="launch" disabled={Boolean(busy) || !workdir.trim()} aria-busy={busy === l} onClick={() => go(l)}>
              {busy === l ? <Spinner size={20} /> : <Icon size={20} aria-hidden="true" />}
              <b>{t(`launch.${l}.title`)}</b>
              <span className="small muted">{missing(l) ? t("ws.quick_install") : t(`launch.${l}.desc`)}</span>
            </button>
          );
        })}
      </div>
      <ErrorNote error={error} />
      <div className="modal-actions">
        <Button variant="ghost" onClick={() => onMore({ preset: "shell", device: dev, workdir })}>
          {t("ws.quick_more")}
        </Button>
        <Button variant="ghost" onClick={onClose}>
          {t("action.cancel")}
        </Button>
      </div>
    </Modal>
  );
}

/**
 * 工作台：像终端软件一样，左边是开着的终端（顶上一排标签），右边是全部会话的列表（可收起、可搜索，多台电脑时按电脑分组）。
 * 点列表里的会话就在左边打开 / 切过去；开着的标签记在浏览器里，刷新后还在。Alt+1…9 切到第 N 个标签。
 */
function Workspace({ ws, items, groups, multi, loading, query, setQuery, match, renderRow, onNew, onQuick, onStart }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const ref = useRef(null);
  const height = useFillHeight(ref);
  const vars = useTermChrome(status.settings.theme);
  const byId = (id) => items.find((i) => i.id === id);
  const tabsRef = useRef(ws);
  tabsRef.current = ws;
  const [drag, setDrag] = useState(null);
  useEffect(() => {
    const onKey = (e) => {
      if (!e.altKey || e.ctrlKey || e.metaKey || e.shiftKey || !/^Digit[1-9]$/.test(e.code)) return;
      if (e.target?.closest?.("input, textarea, select, [contenteditable]")) return;
      const id = tabsRef.current.tabs[Number(e.code.slice(5)) - 1];
      if (!id) return;
      e.preventDefault();
      tabsRef.current.open(id);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  // 终端存在吗：拿到了它所在电脑的列表、里面却没有，就是被删了（电脑离线、还没加载时不算）
  const missing = (id) => {
    if (loading || byId(id)) return false;
    if (!multi) return true;
    const dev = terminalDevice(id);
    return Boolean(dev && groups?.[dev] && !groups[dev].error);
  };
  const shown = match ? items.filter(match) : items;
  return (
    <div className={`ws ${ws.side ? "" : "ws-no-side"}`} ref={ref} style={height ? { height } : undefined}>
      <div className="ws-main" style={vars} data-scheme={termTheme(status.settings.theme).scheme}>
        <div className="ws-tabs" role="tablist" aria-label={t("ws.tabs")}>
          {ws.tabs.map((id, n) => {
            const inst = byId(id);
            const Icon = LAUNCH_ICON[inst?.launch] ?? SquareTerminal;
            const on = ws.active === id;
            return (
              <div
                key={id}
                className={`ws-tab ${on ? "on" : ""} ${drag === id ? "dragging" : ""}`}
                draggable
                onDragStart={(e) => {
                  setDrag(id);
                  e.dataTransfer.effectAllowed = "move";
                }}
                onDragEnd={() => setDrag(null)}
                onDragOver={(e) => drag && e.preventDefault()}
                onDrop={(e) => {
                  e.preventDefault();
                  if (drag && drag !== id) ws.move(drag, n);
                  setDrag(null);
                }}
                onAuxClick={(e) => e.button === 1 && ws.close(id)}
              >
                <button type="button" role="tab" aria-selected={on} className="ws-tab-main" onClick={() => ws.open(id)} title={`${inst?.name ?? id}${n < 9 ? ` (Alt+${n + 1})` : ""}`}>
                  <Icon size={14} aria-hidden="true" />
                  <span className="ellipsis">{inst?.name ?? "…"}</span>
                  <span className={`dot ${inst?.running ? "on" : ""}`} aria-hidden="true" />
                </button>
                <button type="button" className="ws-tab-x" onClick={() => ws.close(id)} aria-label={t("ws.close_tab", { name: inst?.name ?? id })} title={t("ws.close_tab", { name: inst?.name ?? id })}>
                  <X size={14} />
                </button>
              </div>
            );
          })}
          <button type="button" className="ws-tab-add" onClick={onNew} aria-label={t("terminals.new")} title={t("terminals.new")} disabled={!status.account.allowed}>
            <Plus size={16} />
          </button>
          <span className="grow" />
          <button type="button" className="ws-tab-add" onClick={ws.toggleSide} aria-pressed={ws.side} aria-label={t(ws.side ? "ws.hide_list" : "ws.show_list")} title={t(ws.side ? "ws.hide_list" : "ws.show_list")}>
            {ws.side ? <PanelRightClose size={16} /> : <PanelRightOpen size={16} />}
          </button>
        </div>
        <div className="ws-panes">
          {ws.tabs.map((id) => (
            <TermPane key={id} id={id} inst={byId(id)} missing={missing(id)} active={ws.active === id} onStart={() => onStart(id)} />
          ))}
          {ws.tabs.length === 0 && (
            <div className="pane-notice">
              <div>
                <PanelsTopLeft size={28} aria-hidden="true" />
                <b>{t("ws.empty_title")}</b>
                <p>{t(items.length ? "ws.empty_body" : "ws.empty_none")}</p>
                {!ws.side && items.length > 0 && (
                  <Button variant="secondary" icon={PanelRightOpen} onClick={ws.toggleSide}>
                    {t("ws.show_list")}
                  </Button>
                )}
              </div>
            </div>
          )}
        </div>
      </div>
      {ws.side && (
        <aside className="ws-side" aria-label={t("ws.list")}>
          <SearchBox value={query} onChange={setQuery} count={match ? shown.length : null} />
          <div className="ws-side-tools">
            <div className="seg" role="group" aria-label={t("ws.group_label")}>
              <button type="button" aria-pressed={ws.project} onClick={() => ws.setProject(true)} title={t("ws.by_project_hint")}>
                <Folder size={14} aria-hidden="true" />
                {t("ws.by_project")}
              </button>
              <button type="button" aria-pressed={!ws.project} onClick={() => ws.setProject(false)}>
                <List size={14} aria-hidden="true" />
                {t(multi ? "ws.by_device" : "ws.by_none")}
              </button>
            </div>
            <span className="grow" />
            <button type="button" className="btn btn-plain ws-new-project" onClick={() => onQuick({})} disabled={!status.account.allowed} title={t("ws.new_project_hint")}>
              <FolderPlus size={15} aria-hidden="true" />
              {t("ws.new_project")}
            </button>
          </div>
          <div className="ws-list">
            {loading ? (
              <Skeleton height={160} />
            ) : ws.project ? (
              projectGroups(shown, multi).map((g) => {
                const name = baseName(g.workdir);
                return (
                  <section key={g.key} className="ws-group ws-project" aria-label={name}>
                    <h3 title={g.workdir}>
                      <Folder size={13} aria-hidden="true" />
                      <span className="ellipsis">{name}</span>
                      {multi && g.device && <span className="ws-project-dev ellipsis">{deviceName(t, devices.byId(g.device))}</span>}
                      <span className="grow" />
                      <button
                        type="button"
                        className="btn btn-plain btn-icon ws-project-add"
                        onClick={() => onQuick({ device: g.device, workdir: g.workdir })}
                        disabled={!status.account.allowed}
                        aria-label={t("ws.new_in_project", { name })}
                        title={t("ws.new_in_project", { name })}
                      >
                        <Plus size={14} />
                      </button>
                    </h3>
                    {g.items.map(renderRow)}
                  </section>
                );
              })
            ) : multi ? (
              devices.items.map((d) => {
                const g = groups?.[d.id];
                const list = (g?.items ?? []).filter((i) => !match || match(i));
                if (match && list.length === 0) return null;
                return (
                  <section key={d.id} className="ws-group" aria-label={deviceName(t, d)}>
                    <h3>
                      <Monitor size={13} aria-hidden="true" />
                      <span className="ellipsis">{deviceName(t, d)}</span>
                      {!d.online && <span className="badge">{t("devices.offline")}</span>}
                    </h3>
                    {d.online && (g?.error ? <ErrorNote error={g.error} /> : list.map(renderRow))}
                  </section>
                );
              })
            ) : (
              shown.map(renderRow)
            )}
            {!loading && match && shown.length === 0 && <p className="muted small ws-none">{t("view.no_match")}</p>}
          </div>
        </aside>
      )}
    </div>
  );
}

export default function Terminals() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const multi = devices.multi;
  const online = multi ? devices.items.filter((d) => d.online) : [];
  const onlineKey = online.map((d) => d.id).join(",");
  // 只有一台电脑（或本机/局域网打开）时和以前一样只看这台
  const [single, singleError, reloadSingle] = useLoad(() => (multi ? Promise.resolve(null) : api.get("/instances")), [multi]);
  const [grouped, reloadGroups] = useDeviceGroups(multi ? online : null, `${multi}:${onlineKey}`);
  const data = multi ? grouped : single;
  const error = multi ? null : singleError;
  const reload = () => (multi ? reloadGroups() : reloadSingle());
  const allowed = status.account.allowed;
  const [dialog, setDialog] = useState(null);
  const [quick, setQuick] = useState(null);
  const [logs, setLogs] = useState(null);
  const wsOk = useWorkspaceOk();
  const [view, setView] = useView(wsOk);
  const ws = useWorkspaceTabs();
  const [query, setQueryState] = useState(() => readJson(sessionStorage, QUERY_KEY, ""));
  const setQuery = (q) => {
    setQueryState(q);
    writeJson(sessionStorage, QUERY_KEY, q);
  };
  const match = matcher(query, t);
  const reloadRef = useRef(reload);
  reloadRef.current = reload;
  useEffect(() => {
    const id = setInterval(() => reloadRef.current(), 5000);
    return () => clearInterval(id);
  }, []);
  const items = data?.items ?? [];
  const shown = match ? items.filter(match) : items;
  useScrollMemory(Boolean(data), view !== "workspace");
  const [created, setCreated] = useState(null);
  const { navigate } = useRouter();
  const toast = useToast();
  // 电脑上在工作台里打开；手机上（或窗口太窄）直接整页打开，有按键条。
  const openHere = (id) => {
    if (!wsOk) return navigate(`/t/${id}`);
    ws.open(id);
    if (view !== "workspace") {
      setView("workspace");
      window.scrollTo(0, 0);
    }
  };
  const create = (preset) => setDialog({ preset });
  const saved = (row) => {
    reload();
    if (row) setCreated(row);
  };
  const handlers = {
    onChanged: reload,
    onEdit: (inst) => setDialog({ initial: inst }),
    onLogs: setLogs,
    onOpenHere: openHere,
  };
  const renderCard = (i) => <InstanceCard key={i.id} inst={i} {...handlers} />;
  const renderRow = (i) => <InstanceRow key={i.id} inst={i} {...handlers} />;
  const renderSideRow = (i) => <InstanceRow key={i.id} inst={i} compact current={ws.active === i.id} opened={ws.tabs.includes(i.id)} {...handlers} />;
  const workspace = view === "workspace";
  const loading = !data && !error;
  const searchable = items.length > 0 && !workspace;
  return (
    <>
      <div className={`page-head ${workspace ? "page-head-ws" : ""}`}>
        <h1>
          {t("terminals.title")}
          {items.length > 0 && <span className="title-count">{items.length}</span>}
        </h1>
        <span className="inline page-tools">
          {items.length > 0 && <ViewSwitch view={view} onChange={setView} wsOk={wsOk} />}
          {items.length > 0 && (
            <Button icon={Plus} disabled={!allowed} onClick={() => create("shell")} aria-label={t("terminals.new")}>
              <span className="hide-narrow">{t("terminals.new")}</span>
            </Button>
          )}
        </span>
      </div>
      {!multi && <BackendNote />}
      <ErrorNote error={error} />
      {searchable && items.length > 3 && (
        <div className="list-tools">
          <SearchBox value={query} onChange={setQuery} count={match ? shown.length : null} />
        </div>
      )}
      {workspace && items.length > 0 ? (
        <Workspace
          ws={ws}
          items={items}
          groups={data?.groups}
          multi={multi}
          loading={loading}
          query={query}
          setQuery={setQuery}
          match={match}
          renderRow={renderSideRow}
          onNew={() => create("shell")}
          onQuick={setQuick}
          onStart={async (id) => {
            try {
              await api.post(`/instances/${id}/start`);
            } catch (e) {
              toast(errorText(t, e), "error");
            } finally {
              reload();
            }
          }}
        />
      ) : loading ? (
        <div className="instances">
          <Skeleton height={190} />
          <Skeleton height={190} />
        </div>
      ) : items.length === 0 ? (
        <Card>
          <Empty icon={SquareTerminal} title={t("terminals.empty_title")}>
            <p>{t("terminals.empty_body")}</p>
            <div
              className="guide"
              style={{
                textAlign: "left",
                maxWidth: 520,
                margin: "12px auto 0",
              }}
            >
              <b>{t("guide.title")}</b>
              <ol>
                <li>{t("guide.s1")}</li>
                <li>{t("guide.s2")}</li>
                <li>{t("guide.s3")}</li>
              </ol>
            </div>
            <div className="quick-start">
              {["shell", "codex", "claude"].map((l) => {
                const Icon = LAUNCH_ICON[l];
                return (
                  <button
                    key={l}
                    type="button"
                    className="launch"
                    disabled={!allowed}
                    onClick={() => create(l)}
                  >
                    <Icon size={20} aria-hidden="true" />
                    <b>{t(`launch.${l}.title`)}</b>
                    <span className="small muted">{t(`launch.${l}.desc`)}</span>
                  </button>
                );
              })}
            </div>
          </Empty>
        </Card>
      ) : multi ? (
        <>
          {devices.items.map((d) => (
            <DeviceGroup
              key={d.id}
              device={d}
              group={data?.groups?.[d.id]}
              renderCard={view === "list" ? renderRow : renderCard}
              listClass={view === "list" ? "irows" : "instances"}
              match={match}
            />
          ))}
          {match && shown.length === 0 && <p className="muted">{t("view.no_match")}</p>}
        </>
      ) : shown.length === 0 ? (
        <p className="muted">{t("view.no_match")}</p>
      ) : view === "list" ? (
        <div className="irows">{shown.map(renderRow)}</div>
      ) : (
        <div className="instances">{shown.map(renderCard)}</div>
      )}
      {/* 多台电脑都还没有终端时，仍列出离线的电脑，免得以为它们的终端不见了 */}
      {multi && data && items.length === 0 &&
        devices.items.filter((d) => !d.online).map((d) => <DeviceGroup key={d.id} device={d} renderCard={renderCard} />)}
      {!workspace && (
        <>
          <RemoteCard />
          <SafetyNote />
          <p className="muted small">
            {t("terminals.pages_tip")} <Link to="/pages">{t("nav.pages")}</Link>
          </p>
          <Promotions />
        </>
      )}
      <InstanceDialog
        open={Boolean(dialog)}
        initial={dialog?.initial}
        preset={dialog?.preset}
        workdir={dialog?.workdir}
        device={dialog?.device ?? undefined}
        onClose={() => setDialog(null)}
        onSaved={saved}
      />
      <QuickCreateDialog
        open={Boolean(quick)}
        device={quick?.device}
        workdir={quick?.workdir}
        onClose={() => setQuick(null)}
        onMore={(d) => {
          setQuick(null);
          setDialog(d);
        }}
        onCreated={(r) => {
          reload();
          openHere(r.id);
        }}
      />
      <CreatedDialog
        inst={created}
        onClose={() => setCreated(null)}
        onOpenHere={openHere}
      />
      <LogsDialog inst={logs} onClose={() => setLogs(null)} />
    </>
  );
}
