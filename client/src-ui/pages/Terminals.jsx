// 首页：我的终端。每个终端是一个可以长期运行的工作区（例如让 Claude Code 在里面干活），关掉网页也不会停。
import {
  ArrowUp,
  Bot,
  Braces,
  Check,
  ChevronDown,
  ChevronRight,
  Command,
  ExternalLink,
  FileText,
  Fish,
  Folder,
  FolderOpen,
  FolderPlus,
  Globe,
  HardDrive,
  Home,
  Monitor,
  MonitorSmartphone,
  MoreHorizontal,
  PanelTop,
  Pencil,
  Play,
  Plus,
  QrCode as QrCodeIcon,
  RotateCcw,
  Sparkles,
  Square,
  SquareTerminal,
  Trash2,
  Wifi,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import { SafetyNote, useStatus } from "../App.jsx";
import { api, deviceApi, rememberTerminals, terminalDevice } from "../shared/api.js";
import { deviceName, osLabel, RenameDeviceDialog, useDevices } from "../shared/devices.jsx";
import { urlHost } from "../shared/host.js";
import { errorText } from "../shared/i18n.js";
import { Link, useRouter } from "../shared/router.jsx";
import { relativeTime } from "../shared/time.js";
import { EmbeddedTerminal } from "./TerminalView.jsx";
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

function PopMenu({ items }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const ref = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const close = (e) => !ref.current?.contains(e.target) && setOpen(false);
    document.addEventListener("pointerdown", close);
    return () => document.removeEventListener("pointerdown", close);
  }, [open]);
  return (
    <span className="menu-pop" ref={ref}>
      <Button
        variant="ghost"
        icon={MoreHorizontal}
        aria-label={t("action.more")}
        aria-expanded={open}
        onClick={() => setOpen(!open)}
      />
      {open && (
        <ul className="menu-pop-list" role="menu">
          {items.filter(Boolean).map((x) => (
            <li key={x.label}>
              <button
                type="button"
                role="menuitem"
                className={x.danger ? "danger" : ""}
                disabled={x.disabled}
                aria-describedby={x.hint ? `${x.label}-hint` : undefined}
                onClick={() => {
                  setOpen(false);
                  x.onClick();
                }}
              >
                <x.icon size={17} aria-hidden="true" />
                <span className="grow">
                  {x.label}
                  {x.hint && (
                    <span className="menu-pop-hint" id={`${x.label}-hint`}>
                      {x.hint}
                    </span>
                  )}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </span>
  );
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

function InstanceCard({ inst, onChanged, onEdit, onLogs, onOpenHere }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  const confirm = useConfirm();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const state = stateOf(inst);
  // 终端服务或后台任务还在就算“在运行”，这时不能删除，要先停止。
  const live = inst.running || Boolean(inst.task_alive);
  const Icon = LAUNCH_ICON[inst.launch] ?? SquareTerminal;
  const persistent = status.capabilities.persistent;
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
  // 在当前页面内嵌打开（不改变标签页地址）。
  const openHere = async () => {
    if (await ensureRunning()) onOpenHere(inst.id);
  };
  const dsh = inst.launch === "dsh";
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
    if (ok)
      act(() => api.post(`/instances/${inst.id}/stop`), t("instance.stopped"));
  };
  const restart = async () => {
    const ok = await confirm({
      title: t("instance.restart_title", { name: inst.name }),
      body: t("instance.restart_body"),
      confirm: t("instance.restart_confirm"),
      danger: true,
    });
    if (ok)
      act(
        () => api.post(`/instances/${inst.id}/restart`),
        t("instance.restarted"),
      );
  };
  const remove = async () => {
    const ok = await confirm({
      title: t("instance.delete_title", { name: inst.name }),
      body: t("instance.delete_body"),
      confirm: t("action.delete"),
      danger: true,
    });
    if (ok) act(() => api.del(`/instances/${inst.id}`), t("instance.deleted"));
  };
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
        </span>
      </div>
      <div className="instance-meta">
        <span className="one-line">
          {t(`launch.${inst.launch}.title`)}
          {inst.launch === "custom" && (
            <span className="mono ellipsis">· {inst.command}</span>
          )}
          {inst.shell && (
            <span className="mono ellipsis" title={t("shell.label")}>
              · {inst.shell}
            </span>
          )}
        </span>
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
        {inst.error && (
          <span style={{ color: "var(--danger)" }}>
            {t(`instance.error.${inst.error}`, {
              defaultValue: t("instance.error.other"),
            })}
          </span>
        )}
      </div>
      <div className="instance-actions">
        {dsh ? (
          <>
            <Button
              className="grow-btn"
              icon={ExternalLink}
              busy={busy}
              disabled={!status.account.allowed}
              onClick={openPage}
              title={t("dsh.open_hint")}
            >
              {t("dsh.open")}
            </Button>
            {/* DSH 自己的输出（启动报错、日志）还在终端里 */}
            <Button
              variant="secondary"
              icon={SquareTerminal}
              disabled={busy || !status.account.allowed}
              onClick={openHere}
              aria-label={t("dsh.output")}
              title={t("dsh.output")}
            />
          </>
        ) : (
          <>
            <Button
              className="grow-btn"
              icon={inst.running ? PanelTop : Play}
              busy={busy}
              disabled={!status.account.allowed}
              onClick={openHere}
              title={t("instance.open_here_hint")}
            >
              {inst.running ? t("instance.open_here") : t("instance.start_open")}
            </Button>
            <Button
              variant="secondary"
              icon={ExternalLink}
              disabled={busy || !status.account.allowed}
              onClick={openTab}
              aria-label={t("instance.new_window")}
              title={t("instance.new_window")}
            />
          </>
        )}
        {live && (
          <Button variant="ghost" icon={Square} disabled={busy} onClick={stop}>
            {t("instance.stop")}
          </Button>
        )}
        <PopMenu
          items={[
            {
              icon: Pencil,
              label: t("instance.edit"),
              onClick: () => onEdit(inst),
            },
            inst.running && {
              icon: RotateCcw,
              label: t("instance.restart"),
              onClick: restart,
            },
            {
              icon: FileText,
              label: t("instance.logs"),
              onClick: () => onLogs(inst),
            },
            {
              icon: Trash2,
              label: t("action.delete"),
              onClick: remove,
              danger: true,
              disabled: live,
              hint: live ? t("instance.delete_stop_first") : null,
            },
          ]}
        />
      </div>
    </article>
  );
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

/** 文件夹选择弹窗：点文件夹进入，“选择此文件夹”确定当前位置。Windows 上最顶层是“此电脑”（各个盘符）。 */
function FolderPicker({ open, initial, onClose, onPick, client = api }) {
  const { t } = useTranslation();
  const [cur, setCur] = useState("");
  const [typed, setTyped] = useState("");
  const [drives, setDrives] = useState(false);
  const [data, setData] = useState(null);
  const [err, setErr] = useState(null);
  const [hidden, setHidden] = useState(false);
  const [newName, setNewName] = useState("");
  const [loading, setLoading] = useState(false);
  const seq = useRef(0);
  const load = async (path) => {
    const n = ++seq.current;
    setLoading(true);
    try {
      const r = await client.get(
        `/fs/list?path=${encodeURIComponent(path)}&hidden=${hidden ? 1 : 0}`,
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
    setCur(initial ?? "");
    setTyped(initial ?? "");
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
    setCur(path);
    setTyped(path);
  };
  const showDrives = () => {
    setDrives(true);
    setTyped("");
  };
  // 不存在的路径：显示的是最近的已有上级，“选择”仍然选用户要的那个（启动时自动创建）。
  const target = drives ? null : data && !data.exists ? cur : data?.path;
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
  return (
    <Modal open={open} onClose={onClose} title={t("fs.title")} wide className="picker">
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
              !data.exists && (
                <Note>
                  <span className="mono">{cur}</span> · {t("fs.will_create")}
                </Note>
              )
            ))}
          <ul className={`picker-list ${loading ? "loading" : ""}`}>
            {entries.length === 0 && !data.denied && (
              <li className="muted small picker-empty">{t("fs.empty")}</li>
            )}
            {entries.map((e) => {
              const Icon = e.drive ? HardDrive : Folder;
              return (
                <li key={e.path}>
                  <button type="button" onClick={() => go(e.path)}>
                    <Icon size={18} aria-hidden="true" />
                    <span className="mono ellipsis grow">
                      {e.drive ? e.name.replace(/\\$/, "") : e.name}
                    </span>
                    <ChevronRight size={16} className="muted" aria-hidden="true" />
                  </button>
                </li>
              );
            })}
          </ul>
          {!drives && !data.denied && (
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
            {t("fs.show_hidden")}
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
          onClick={() => {
            onPick(target);
            onClose();
          }}
        >
          {t("fs.pick")}
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
export function InstanceDialog({ open, onClose, initial, preset, device: wantDevice, onSaved }) {
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
    setWorkdir(initial?.workdir ?? status.settings.default_workdir);
    workdirTouched.current = false;
    setAutoStart(initial?.auto_start ?? false);
    setFullAccess(initial?.full_access ?? false);
    setRootOk(initial?.root_confirmed ?? false);
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
  const install = async () => {
    const r = await run(() =>
      client.post("/tools/install", {
        tool: launch,
        mirror,
        name: t("tools.install_name", { tool: t(`launch.${launch}.title`) }),
      }),
    );
    if (!r) return;
    if (dev && r.id) rememberTerminals(dev, [r]);
    onClose();
    onSaved?.(r);
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
        {missing && (
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
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} placeholder={t(`launch.${launch}.title`)} />
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

/** 多台电脑：分别向每台在线的电脑要终端列表；一台出错不影响其他。 */
async function loadGroups(online) {
  const groups = await Promise.all(
    online.map((d) =>
      deviceApi(d.id)
        .get("/instances")
        .then(
          (r) => {
            const items = Array.isArray(r?.items) ? r.items : [];
            rememberTerminals(d.id, items);
            return { id: d.id, items };
          },
          (error) => ({ id: d.id, items: [], error }),
        ),
    ),
  );
  return { items: groups.flatMap((g) => g.items), groups: Object.fromEntries(groups.map((g) => [g.id, g])) };
}

/** 一台电脑的终端：标题（名称、系统、在线状态）+ 卡片；离线的电脑只显示一行“离线”。 */
function DeviceGroup({ device, group, renderCard }) {
  const { t } = useTranslation();
  const devices = useDevices();
  const [renaming, setRenaming] = useState(false);
  const [renamed, setRenamed] = useState(null); // 平台下发新名称前先显示刚改的
  const items = group?.items ?? [];
  const name = renamed && renamed.from === device.name ? renamed.name : deviceName(t, device);
  return (
    <section className={`device-group ${device.online ? "" : "device-offline"}`} aria-label={name}>
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
        {device.online && group && !group.error && <span className="muted small">{t("devices.count", { count: items.length })}</span>}
      </h2>
      {device.online &&
        (group?.error ? (
          <ErrorNote error={group.error} />
        ) : !group ? (
          <Skeleton height={120} />
        ) : items.length === 0 ? (
          <p className="muted small device-empty">{t("devices.no_terminals")}</p>
        ) : (
          <div className="instances">{items.map(renderCard)}</div>
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

export default function Terminals() {
  const { t } = useTranslation();
  const { status } = useStatus();
  const devices = useDevices();
  const multi = devices.multi;
  const online = multi ? devices.items.filter((d) => d.online) : [];
  const onlineKey = online.map((d) => d.id).join(",");
  // 只有一台电脑（或本机/局域网打开）时和以前一样只看这台
  const [data, error, reload] = useLoad(() => (multi ? loadGroups(online) : api.get("/instances")), [multi, onlineKey]);
  const allowed = status.account.allowed;
  const [dialog, setDialog] = useState(null);
  const [logs, setLogs] = useState(null);
  const [embedded, setEmbedded] = useState([]);
  useEffect(() => {
    const id = setInterval(reload, 5000);
    return () => clearInterval(id);
  }, []);
  const items = data?.items ?? [];
  const [created, setCreated] = useState(null);
  const { navigate } = useRouter();
  // 手机上的悬浮小窗太挤、也没有按键条，直接进整页终端。
  const openHere = (id) => (isTouch() ? navigate(`/t/${id}`) : setEmbedded((l) => (l.includes(id) ? l : [...l, id])));
  const create = (preset) => setDialog({ preset });
  const saved = (row) => {
    reload();
    if (row) setCreated(row);
  };
  const renderCard = (i) => (
    <InstanceCard
      key={i.id}
      inst={i}
      onChanged={reload}
      onEdit={(inst) => setDialog({ initial: inst })}
      onLogs={setLogs}
      onOpenHere={openHere}
    />
  );
  return (
    <>
      <div className="page-head">
        <h1>{t('terminals.title')}</h1>
        {items.length > 0 && (
          <Button icon={Plus} disabled={!allowed} onClick={() => create('shell')}>
            {t('terminals.new')}
          </Button>
        )}
      </div>
      {/* 终端列表是主角：远程打开卡片与安全提醒放在列表后面，提醒折叠成一行 */}
      {!multi && <BackendNote />}
      <ErrorNote error={error} />
      {!data && !error ? (
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
          {embedded.map((id) => (
            <EmbeddedTerminal
              key={id}
              id={id}
              inst={items.find((i) => i.id === id)}
              onClose={() => setEmbedded((l) => l.filter((x) => x !== id))}
              onStart={async () => {
                await api.post(`/instances/${id}/start`);
                reload();
              }}
            />
          ))}
          {devices.items.map((d) => (
            <DeviceGroup key={d.id} device={d} group={data?.groups?.[d.id]} renderCard={renderCard} />
          ))}
        </>
      ) : (
        <>
          {embedded.map((id) => (
            <EmbeddedTerminal
              key={id}
              id={id}
              inst={items.find((i) => i.id === id)}
              onClose={() => setEmbedded((l) => l.filter((x) => x !== id))}
              onStart={async () => {
                await api.post(`/instances/${id}/start`);
                reload();
              }}
            />
          ))}
          <div className="instances">
            {items.map(renderCard)}
          </div>
        </>
      )}
      {/* 多台电脑都还没有终端时，仍列出离线的电脑，免得以为它们的终端不见了 */}
      {multi && data && items.length === 0 &&
        devices.items.filter((d) => !d.online).map((d) => <DeviceGroup key={d.id} device={d} renderCard={renderCard} />)}
      <RemoteCard />
      <SafetyNote />
      <p className="muted small">
        {t("terminals.pages_tip")} <Link to="/pages">{t("nav.pages")}</Link>
      </p>
      <Promotions />
      <InstanceDialog
        open={Boolean(dialog)}
        initial={dialog?.initial}
        preset={dialog?.preset}
        onClose={() => setDialog(null)}
        onSaved={saved}
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
