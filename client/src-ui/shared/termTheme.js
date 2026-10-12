// 终端配色在管理台这一侧用到的颜色（完整配色在 src/instances.rs 的 ttyd_args 里传给 xterm.js，两边保持一致）。
// 终端上下的标题栏、按键条、标签栏都跟着终端配色走，不然浅色 / Agent 配色时终端变了、上下两头还是深色。
export const TERM_THEMES = {
  agent: {
    bg: '#1a1915', fg: '#e8e4dc', accent: '#d97757', border: '#3a352e',
    bar: '#211f1a', muted: '#a39e93', key: '#2a2823', keyLine: '#3d3830', keyActive: '#3d3830', hover: 'rgba(255, 255, 255, .07)', scheme: 'dark',
  },
  dark: {
    bg: '#11131a', fg: '#e3e6ee', accent: '#a5b4fc', border: '#343a4a',
    bar: '#171a22', muted: '#8a91a3', key: '#1f232d', keyLine: '#2f3442', keyActive: '#2f3442', hover: 'rgba(255, 255, 255, .06)', scheme: 'dark',
  },
  light: {
    bg: '#fbfbfd', fg: '#1f2330', accent: '#4f46e5', border: '#d4d7e1',
    bar: '#f1f2f7', muted: '#5e6678', key: '#ffffff', keyLine: '#d4d7e1', keyActive: '#e4e6ef', hover: 'rgba(20, 24, 40, .06)', scheme: 'light',
  },
};

export const termTheme = (name) => TERM_THEMES[name] ?? TERM_THEMES.agent;

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
