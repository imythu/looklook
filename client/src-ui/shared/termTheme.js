// 终端配色在管理台这一侧用到的颜色（完整配色在 src/instances.rs 的 ttyd_args 里传给 xterm.js，两边保持一致）。
// 终端上下的标题栏、按键条、标签栏都跟着终端配色走，不然浅色 / Agent 配色时终端变了、上下两头还是深色。
// ansi：设置页配色小样上的几个色点（红、绿、黄、蓝、品红、青）。顺序就是设置页里的顺序，第一个是默认。
export const TERM_THEMES = {
  tangerine: {
    bg: '#fff7ee', fg: '#3a2414', accent: '#ff6a1f', border: '#f5d2b3',
    bar: '#ffe7d1', muted: '#8a6448', key: '#ffffff', keyLine: '#f0cfb2', keyActive: '#ffd9b8', hover: 'rgba(255, 106, 31, .12)', scheme: 'light',
    ansi: ['#e53935', '#2e9e44', '#c77700', '#1e6fe0', '#c2185b', '#00897b'],
  },
  lagoon: {
    bg: '#16204a', fg: '#e9eeff', accent: '#ffcc4d', border: '#2f3d7a',
    bar: '#1d2a5e', muted: '#9fb0e6', key: '#24346f', keyLine: '#36488c', keyActive: '#36488c', hover: 'rgba(255, 255, 255, .09)', scheme: 'dark',
    ansi: ['#ff6b81', '#4ee2a0', '#ffcc4d', '#6aa8ff', '#d68cff', '#3fd8f2'],
  },
  sunset: {
    bg: '#2b1736', fg: '#fbeaf0', accent: '#ff8a5c', border: '#523064',
    bar: '#371d46', muted: '#cba6cc', key: '#43255a', keyLine: '#5d3574', keyActive: '#5d3574', hover: 'rgba(255, 255, 255, .09)', scheme: 'dark',
    ansi: ['#ff6b8b', '#9ee37d', '#ffc857', '#7fb2ff', '#ff7ad9', '#5ee0d0'],
  },
  mint: {
    bg: '#f2fbf7', fg: '#15352b', accent: '#00a676', border: '#cdebdf',
    bar: '#ffffff', muted: '#4f7a6b', key: '#ffffff', keyLine: '#c4e5d8', keyActive: '#d9f3e8', hover: 'rgba(0, 140, 95, .08)', scheme: 'light',
    ansi: ['#e5484d', '#0f9d58', '#c27c00', '#2563eb', '#b5179e', '#0891b2'],
  },
  agent: {
    bg: '#1a1915', fg: '#e8e4dc', accent: '#d97757', border: '#3a352e',
    bar: '#211f1a', muted: '#a39e93', key: '#2a2823', keyLine: '#3d3830', keyActive: '#3d3830', hover: 'rgba(255, 255, 255, .07)', scheme: 'dark',
    ansi: ['#e06c5a', '#8fbf7a', '#e3b35c', '#7aa2d6', '#c48bc9', '#6fbfb5'],
  },
  dark: {
    bg: '#11131a', fg: '#e3e6ee', accent: '#a5b4fc', border: '#343a4a',
    bar: '#171a22', muted: '#8a91a3', key: '#1f232d', keyLine: '#2f3442', keyActive: '#2f3442', hover: 'rgba(255, 255, 255, .06)', scheme: 'dark',
    ansi: ['#f06a7e', '#3ccf7c', '#e0a846', '#818cf8', '#c084fc', '#22d3ee'],
  },
  light: {
    bg: '#fbfbfd', fg: '#1f2330', accent: '#4f46e5', border: '#d4d7e1',
    bar: '#f1f2f7', muted: '#5e6678', key: '#ffffff', keyLine: '#d4d7e1', keyActive: '#e4e6ef', hover: 'rgba(20, 24, 40, .06)', scheme: 'light',
    ansi: ['#d9364f', '#12a150', '#b7791f', '#4f46e5', '#a23fd1', '#0891b2'],
  },
};

export const termTheme = (name) => TERM_THEMES[name] ?? TERM_THEMES.tangerine;

/** 终端页面外框用的 CSS 变量（styles.css 里的 --term-*）。 */
export function termVars(name) {
  const th = termTheme(name);
  return {
    '--term-bg': th.bg,
    '--term-fg': th.fg,
    '--term-accent': th.accent,
    '--term-line': th.border,
    '--term-bar': th.bar,
    '--term-muted': th.muted,
    '--term-key': th.key,
    '--term-key-line': th.keyLine,
    '--term-key-active': th.keyActive,
    '--term-hover': th.hover,
  };
}

/** 对话 / 改动 / 文件标签时上下两头不跟终端配色，跟管理台一样（不然浅色的对话上下夹着两条深色栏）。 */
export const APP_CHROME = {
  '--term-bg': 'var(--bg)',
  '--term-fg': 'var(--text)',
  '--term-accent': 'var(--brand)',
  '--term-line': 'var(--line)',
  '--term-bar': 'var(--panel)',
  '--term-muted': 'var(--muted)',
  '--term-key': 'var(--panel)',
  '--term-key-line': 'var(--line)',
  '--term-key-active': 'var(--panel)',
  '--term-hover': 'var(--panel-2)',
};
