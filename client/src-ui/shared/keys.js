// 手机按键条的按键定义与转义序列（xterm 规则）。
// 修饰键（Ctrl / Alt / Shift）有三种状态：null 关、'once' 只作用于下一个键、'lock' 锁定直到再点一次。
// 终端里手机自带键盘输入的字符也会套用修饰键，那部分逻辑在 src/term-inject.js（同一套规则）。

export const MODS = ['ctrl', 'alt', 'shift'];
export const NO_MODS = { ctrl: null, alt: null, shift: null };

export const anyMod = (m) => Boolean(m.ctrl || m.alt || m.shift);
/** 关 → 一次 → 锁定 → 关。 */
export const nextMod = (v) => (v === null ? 'once' : v === 'once' ? 'lock' : null);
/** 按下一个键后清掉“一次”的修饰键，锁定的保留。 */
export const releaseOnce = (m) => Object.fromEntries(MODS.map((k) => [k, m[k] === 'lock' ? 'lock' : null]));

/** xterm 的修饰参数：1 + Shift(1) + Alt(2) + Ctrl(4)。 */
const param = (m) => 1 + (m.shift ? 1 : 0) + (m.alt ? 2 : 0) + (m.ctrl ? 4 : 0);

/** 给一个普通字符套修饰键：Shift 变大写，Ctrl 变控制字符，Alt 前加 ESC。 */
export function modChar(ch, m) {
  let out = m.shift ? ch.toUpperCase() : ch;
  if (m.ctrl) {
    const c = out.toUpperCase().charCodeAt(0);
    if (out.length === 1 && c >= 64 && c <= 95) out = String.fromCharCode(c - 64);
    else if (out === ' ' || out === '2') out = '\x00';
    else if (out >= '3' && out <= '7') out = String.fromCharCode(out.charCodeAt(0) - 24);
    else if (out === '/') out = '\x1f';
    else if (out === '?' || out === '8') out = '\x7f';
  }
  return m.alt ? `\x1b${out}` : out;
}

// 方向键、Home/End：无修饰时按光标模式发 ESC [ x 或 ESC O x（vim、less 等），有修饰时 ESC [ 1 ; m x。
const cursor = (x) => (m, appCursor) => (anyMod(m) ? `\x1b[1;${param(m)}${x}` : appCursor ? `\x1bO${x}` : `\x1b[${x}`);
// PgUp/PgDn/Ins/Del/F5+：ESC [ n ~，有修饰时 ESC [ n ; m ~。
const tilde = (n) => (m) => (anyMod(m) ? `\x1b[${n};${param(m)}~` : `\x1b[${n}~`);
// F1–F4：ESC O P..S，有修饰时 ESC [ 1 ; m P..S。
const ss3 = (x) => (m) => (anyMod(m) ? `\x1b[1;${param(m)}${x}` : `\x1bO${x}`);
const char = (c) => (m) => modChar(c, m);
const fixed = (s) => () => s;

/**
 * 按键：label 显示文字；seq(mods, appCursor) 生成要发送的内容；
 * repeat 按住连发；raw 为固定组合键，不套修饰键也不消耗修饰键；hint 为说明文案的 key。
 */
const K = {
  esc: { label: 'Esc', seq: (m) => (m.alt ? '\x1b\x1b' : '\x1b') },
  tab: { label: 'Tab', seq: (m) => (m.shift ? '\x1b[Z' : m.alt ? '\x1b\t' : '\t') },
  up: { label: '↑', seq: cursor('A'), repeat: true },
  down: { label: '↓', seq: cursor('B'), repeat: true },
  right: { label: '→', seq: cursor('C'), repeat: true },
  left: { label: '←', seq: cursor('D'), repeat: true },
  home: { label: 'Home', seq: cursor('H') },
  end: { label: 'End', seq: cursor('F') },
  pgup: { label: 'PgUp', seq: tilde(5), repeat: true },
  pgdn: { label: 'PgDn', seq: tilde(6), repeat: true },
  ins: { label: 'Ins', seq: tilde(2) },
  del: { label: 'Del', seq: tilde(3), repeat: true },
  enter: { label: '↵', seq: (m) => (m.alt ? '\x1b\r' : '\r') },
  bksp: { label: '⌫', seq: (m) => (m.ctrl ? '\x08' : m.alt ? '\x1b\x7f' : '\x7f'), repeat: true },
};

for (const [id, k] of Object.entries(K)) k.id = id;

const combo = (label, seq, hint) => ({ label, seq: fixed(seq), raw: true, hint });

/**
 * 一直显示的两行（第二行的 ← ↓ → 与第一行的 ↑ 组成倒 T，回车常驻右下角）。
 * 'more' 'paste' 'ime'（弹出/收起手机键盘）与修饰键由组件处理；Home / End 在“翻页编辑”分类里。
 */
export const MAIN_ROWS = [
  ['esc', 'tab', 'ctrl', 'alt', 'up', 'ctrl_c', 'more'],
  ['shift', 'ime', 'paste', 'left', 'down', 'right', 'enter'],
];

/** 收起后只留一行：Esc / Tab / Ctrl、方向键、键盘开关和回车；“更多”在展开后的两行里（向上滑把手）。 */
export const COMPACT_ROW = ['esc', 'tab', 'ctrl', 'left', 'up', 'down', 'right', 'ime', 'enter'];

export const KEYS = {
  ...K,
  ctrl_c: combo('^C', '\x03', 'ctrl_c'),
};

// 各系统常用的组合键（分类之间可以重复，方便用户在自己习惯的分类里一次找全）。
const C = {
  ctrl_c: combo('^C', '\x03', 'ctrl_c'),
  ctrl_d: combo('^D', '\x04', 'ctrl_d'),
  ctrl_z: combo('^Z', '\x1a', 'ctrl_z'),
  ctrl_l: combo('^L', '\x0c', 'ctrl_l'),
  ctrl_r: combo('^R', '\x12', 'ctrl_r'),
  ctrl_a: combo('^A', '\x01', 'ctrl_a'),
  ctrl_e: combo('^E', '\x05', 'ctrl_e'),
  ctrl_u: combo('^U', '\x15', 'ctrl_u'),
  ctrl_k: combo('^K', '\x0b', 'ctrl_k'),
  ctrl_w: combo('^W', '\x17', 'ctrl_w'),
  ctrl_y: combo('^Y', '\x19', 'ctrl_y'),
  alt_b: combo('Alt B', '\x1bb', 'alt_b'),
  alt_f: combo('Alt F', '\x1bf', 'alt_f'),
  alt_dot: combo('Alt .', '\x1b.', 'alt_dot'),
  shift_tab: combo('⇧Tab', '\x1b[Z', 'shift_tab'),
  ctrl_backslash: combo('^\\', '\x1c', 'ctrl_backslash'),
};

/** 分类面板。os：属于哪个系统的“常用”，界面把本机系统排在最前并标出来。 */
export const GROUPS = [
  {
    id: 'linux',
    os: 'linux',
    keys: [C.ctrl_c, C.ctrl_d, C.ctrl_z, C.ctrl_l, C.ctrl_r, C.ctrl_a, C.ctrl_e, C.ctrl_u, C.ctrl_k, C.ctrl_w, C.ctrl_y, C.alt_b, C.alt_f, C.alt_dot, C.shift_tab, C.ctrl_backslash],
  },
  {
    id: 'darwin',
    os: 'darwin',
    keys: [
      C.ctrl_c,
      C.ctrl_d,
      C.ctrl_z,
      C.ctrl_l,
      C.ctrl_r,
      C.ctrl_a,
      C.ctrl_e,
      C.ctrl_u,
      C.ctrl_k,
      C.ctrl_w,
      combo('⌥←', '\x1bb', 'alt_b'),
      combo('⌥→', '\x1bf', 'alt_f'),
      combo('⌥⌫', '\x1b\x7f', 'alt_bksp'),
      combo('Esc .', '\x1b.', 'alt_dot'),
      combo('^_', '\x1f', 'undo'),
      C.shift_tab,
    ],
  },
  {
    id: 'windows',
    os: 'windows',
    // PowerShell（PSReadLine 的 Windows 模式）与 cmd 的常用键
    keys: [
      C.ctrl_c,
      combo('Esc', '\x1b', 'clear_line'),
      C.ctrl_l,
      C.ctrl_r,
      combo('^←', '\x1b[1;5D', 'alt_b'),
      combo('^→', '\x1b[1;5C', 'alt_f'),
      combo('^⌫', '\x08', 'ctrl_w'),
      combo('^Del', '\x1b[3;5~', 'kill_word'),
      combo('^Home', '\x1b[1;5H', 'ctrl_u'),
      combo('^End', '\x1b[1;5F', 'ctrl_k'),
      combo('^Z', '\x1a', 'undo'),
      combo('^Space', '\x00', 'menu_complete'),
      C.shift_tab,
      combo('F7', '\x1b[18~', 'history_list'),
      combo('F8', '\x1b[19~', 'history_prefix'),
      combo('^\\', '\x1c', 'ctrl_backslash'),
    ],
  },
  {
    id: 'ai',
    // Claude Code / Codex 里常用的键
    keys: [
      combo('Esc', '\x1b', 'ai_interrupt'),
      combo('Esc Esc', '\x1b\x1b', 'ai_rewind'),
      combo('⇧Tab', '\x1b[Z', 'ai_mode'),
      combo('^C', '\x03', 'ai_cancel'),
      combo('^D', '\x04', 'ctrl_d'),
      combo('^L', '\x0c', 'ctrl_l'),
      combo('^O', '\x0f', 'ai_transcript'),
      combo('^T', '\x14', 'ai_todos'),
      combo('^R', '\x12', 'ctrl_r'),
      combo('Alt ↵', '\x1b\r', 'newline'),
      combo('^J', '\n', 'newline'),
      combo('/', '/', 'ai_slash'),
      combo('@', '@', 'ai_file'),
      combo('!', '!', 'ai_bash'),
      combo('#', '#', 'ai_memory'),
      combo('↵', '\r', 'ai_send'),
    ],
  },
  {
    id: 'symbol',
    keys: ['|', '~', '/', '\\', '-', '_', '=', '+', ':', ';', "'", '"', '`', '<', '>', '{', '}', '[', ']', '(', ')', '$', '&', '*', '#', '!', '?', '@', '%', '^'].map((c) => ({ label: c, seq: char(c) })),
  },
  {
    id: 'nav',
    keys: [K.pgup, K.pgdn, K.ins, K.del, K.home, K.end, K.bksp, K.enter],
  },
  {
    id: 'fn',
    keys: ['P', 'Q', 'R', 'S'].map((x, i) => ({ label: `F${i + 1}`, seq: ss3(x) })).concat([15, 17, 18, 19, 20, 21, 23, 24].map((n, i) => ({ label: `F${i + 5}`, seq: tilde(n) }))),
  },
];

/** 本机系统（status.capabilities.os：linux / darwin / windows …）。其他类 Unix 按 Linux 算。 */
export const hostProfile = (os) => (os === 'darwin' || os === 'windows' ? os : 'linux');

/** 面板里的分类顺序：本机系统的“常用”排第一，然后 AI 助手、另外两个系统，最后符号 / 翻页 / F 键。 */
export function orderedGroups(os) {
  const host = hostProfile(os);
  const rank = (g) => (g.id === host ? 0 : g.id === 'ai' ? 1 : g.os ? 2 : 3);
  return [...GROUPS].sort((a, b) => rank(a) - rank(b));
}
