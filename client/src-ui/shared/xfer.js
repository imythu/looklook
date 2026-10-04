// 终端文件传输的纯函数（docs/FILE_TRANSFER.md §3）：去向规则、路径加引号、大小格式化、rz/sz 状态判断。
// 不依赖 React / 浏览器，便于用 node 直接测试（xfer.test.mjs）。

/** AI 实例 = 启动方式是 Codex 或 Claude Code。 */
export const isAiInstance = (inst) => inst?.launch === 'codex' || inst?.launch === 'claude';

/**
 * 去向规则（§3.3）。source：'paste' | 'drop' | 'pick' | 'rz'；
 * aiMode：面板里的“发给 AI / 存到当前目录”切换（仅 AI 实例、仅 pick 有意义）；insertChecked：面板里的“填入路径”勾选。
 * 返回 { dest: 'attach' | 'cwd' | 'rz', insert: boolean }。
 */
export function routeFor(source, { ai, aiMode = 'ai', insertChecked = false } = {}) {
  if (source === 'rz') return { dest: 'rz', insert: false };
  if (source === 'paste' || source === 'drop') return { dest: ai ? 'attach' : 'cwd', insert: true };
  if (ai && aiMode === 'ai') return { dest: 'attach', insert: true };
  return { dest: 'cwd', insert: Boolean(insertChecked) };
}

// POSIX shell 里不需要引号的字符；其余一律用单引号包起来（单引号本身写成 '\''）。
const POSIX_SAFE = /^[A-Za-z0-9_@%+=:,./-]+$/;
// cmd / PowerShell 里有特殊含义或会断开参数的字符。Windows 文件名不能含 "，所以用 "…" 包起来即可。
const WIN_UNSAFE = /[\s&()[\]{}^=;!'+,`~%$@#<>|]/;

/** 按目标系统给一个路径加引号（os 来自 /files 的 os 字段，"windows" 用 "…"，其余按 POSIX shell）。 */
export function quotePath(path, os) {
  if (!path) return "''";
  if (os === 'windows') return WIN_UNSAFE.test(path) ? `"${path}"` : path;
  return POSIX_SAFE.test(path) ? path : `'${path.replace(/'/g, "'\\''")}'`;
}

/** 多个路径用空格分隔，各自加引号。 */
export const joinPaths = (paths, os) => paths.map((p) => quotePath(p, os)).join(' ');

/** 1.2 MB 这样的大小。 */
export function formatBytes(n) {
  if (!Number.isFinite(n) || n < 0) return '';
  if (n < 1024) return `${n} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let v = n / 1024;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${v >= 100 ? Math.round(v) : v.toFixed(1)} ${units[i]}`;
}

/** 下一块的字节范围 [start, end)；offset 已到末尾时返回 null（该调 finish 了）。 */
export function nextChunk(offset, size, chunkSize) {
  if (offset >= size) return null;
  return { start: offset, end: Math.min(size, offset + Math.max(1, chunkSize)) };
}

/** 网络抖动类错误：可以原样重试（从服务端记录的进度继续）。 */
export function isRetryable(e) {
  if (!e) return false;
  if (e.code === 'CLIENT_UNREACHABLE' || e.code === 'UPLOAD_BUSY' || e.status === 0) return true;
  return e.status === 502 || e.status === 503 || e.status === 504 || e.status === 408;
}

/** 第 n 次（从 1 起）重试前等多久：1、2、4、8、15、15… 秒。 */
export const retryDelay = (n) => Math.min(15000, 1000 * 2 ** Math.max(0, n - 1));

const SHELLS = new Set(['bash', 'zsh', 'sh', 'fish', 'dash', 'ksh', 'tcsh', 'csh', 'nu', 'xonsh', 'login', 'tmux', 'pwsh', 'powershell', 'cmd']);

/** 前台是普通 shell：说明 rz/sz 早已结束，看到的横幅只是屏幕重绘出来的旧内容。 */
export function isShellCommand(cmd) {
  if (!cmd) return true;
  const base = String(cmd).trim().split(/[\\/]/).pop().replace(/^-/, '').replace(/\.exe$/i, '').toLowerCase();
  return SHELLS.has(base);
}

/**
 * 收到 looklook:transfer 后怎么做：根据 /files 返回的 transfer 判断。
 * 返回 null（不理会）或 { mode: 'rz' | 'sz' | 'remote', key, transfer }；key 用于“同一次 rz/sz 只弹一次”。
 */
export function transferAction(kind, transfer) {
  if (!transfer) return null;
  if (transfer.local) {
    if (transfer.kind !== 'rz' && transfer.kind !== 'sz') return null;
    return { mode: transfer.kind, key: `${transfer.kind}:${transfer.pid}`, transfer };
  }
  if (isShellCommand(transfer.command)) return null;
  return { mode: 'remote', key: `remote:${kind}:${transfer.command ?? ''}`, transfer: { ...transfer, kind } };
}
