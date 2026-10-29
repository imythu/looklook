// 程序报告的进度（OSC 9;4，Windows Terminal / Ghostty 等都支持；tmux 的 progressbar 特性把窗格里的进度转发出来）：
// 终端顶部一条 3px 的细条。state 1 普通（按百分比）、2 出错（红）、3 不确定（来回流动）、4 暂停 / 警告（黄），0 隐藏。
// 普通进度用光标色，跟着配色走。
const COLORS = { 2: '#e5484d', 4: '#e0a846' };

export class ProgressBar {
  constructor(parent, theme) {
    this.theme = theme;
    this.el = document.createElement('div');
    this.el.className = 'll-progress';
    this.el.setAttribute('role', 'progressbar');
    this.fill = document.createElement('i');
    this.el.appendChild(this.fill);
    parent.appendChild(this.el);
    this.hideTimer = 0;
  }

  set({ state, value }) {
    const { el, fill } = this;
    clearTimeout(this.hideTimer);
    if (!state) {
      // 完成时先走满再淡出，免得一闪就没了
      fill.style.width = '100%';
      this.hideTimer = setTimeout(() => el.classList.remove('on'), 300);
      el.removeAttribute('aria-valuenow');
      return;
    }
    el.classList.add('on');
    el.classList.toggle('busy', state === 3);
    fill.style.background = COLORS[state] || this.theme()?.cursor || '#818cf8';
    if (state === 3) {
      fill.style.width = '';
      el.removeAttribute('aria-valuenow');
    } else {
      const v = Math.max(0, Math.min(100, value || 0));
      fill.style.width = v + '%';
      el.setAttribute('aria-valuenow', String(v));
    }
  }
}
