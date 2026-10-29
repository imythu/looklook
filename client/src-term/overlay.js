// 终端中间的提示浮层（“Reconnected”等），照搬 ttyd 1.7.7（移植自 hterm 的 showOverlay）。
// term-inject.js 按“.xterm 下圆角 15px 的 div”找到它并在断线时藏起来，样式不要改。
export class OverlayAddon {
  constructor() {
    this.node = document.createElement('div');
    this.node.style.cssText = `border-radius: 15px;
font-size: xx-large;
opacity: 0.75;
padding: 0.2em 0.5em 0.2em 0.5em;
position: absolute;
-webkit-user-select: none;
-webkit-transition: opacity 180ms ease-in;
-moz-user-select: none;
-moz-transition: opacity 180ms ease-in;`;
    this.node.addEventListener('mousedown', (e) => {
      e.preventDefault();
      e.stopPropagation();
    }, true);
    this.timer = undefined;
  }

  activate(terminal) {
    this.terminal = terminal;
  }

  dispose() {}

  show(msg, timeout) {
    const { terminal, node } = this;
    if (!terminal.element) return;
    node.style.color = '#101010';
    node.style.backgroundColor = '#f0f0f0';
    node.textContent = msg;
    node.style.opacity = '0.75';
    if (!node.parentNode) terminal.element.appendChild(node);
    const box = terminal.element.getBoundingClientRect();
    const own = node.getBoundingClientRect();
    node.style.top = (box.height - own.height) / 2 + 'px';
    node.style.left = (box.width - own.width) / 2 + 'px';
    if (this.timer) clearTimeout(this.timer);
    if (!timeout) return;
    this.timer = setTimeout(() => {
      node.style.opacity = '0';
      this.timer = setTimeout(() => {
        node.remove();
        this.timer = undefined;
        node.style.opacity = '0.75';
      }, 200);
    }, timeout);
  }
}
