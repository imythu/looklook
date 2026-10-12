// 终端配色在管理台这一侧用到的几种颜色（完整配色在 src/instances.rs 的 ttyd_args 里传给 xterm.js，两边保持一致）。
export const TERM_THEMES = {
  agent: { bg: '#1a1915', fg: '#e8e4dc', accent: '#d97757', border: '#3a352e' },
  dark: { bg: '#11131a', fg: '#e3e6ee', accent: '#a5b4fc', border: '#343a4a' },
  light: { bg: '#fbfbfd', fg: '#1f2330', accent: '#4f46e5', border: '#d4d7e1' },
};

export const termTheme = (name) => TERM_THEMES[name] ?? TERM_THEMES.agent;
