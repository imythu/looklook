// trz / tsz 的浏览器端（照搬 ttyd 1.7.7 的 ZmodemAddon 里 trzsz 那一半）：输出先经过 TrzszFilter，
// 不在传文件时原样写进终端；把文件拖到终端上时自动输入 trz 并上传（嵌在管理台里时 term-inject.js 先接管拖放）。
import { TrzszFilter } from 'trzsz';

export class TrzszAddon {
  constructor(options) {
    this.options = options;
    this.disposables = [];
  }

  activate(terminal) {
    this.terminal = terminal;
    const { sender, writer, windows, dragInitTimeout } = this.options;
    this.filter = new TrzszFilter({
      writeToTerminal: (data) => writer(typeof data === 'string' ? data : new Uint8Array(data)),
      sendToServer: (data) => sender(data),
      terminalColumns: terminal.cols,
      isWindowsShell: windows,
      dragInitTimeout,
    });
    const el = terminal.element;
    this.listen(el, 'dragover', (e) => e.preventDefault());
    this.listen(el, 'drop', (e) => {
      e.preventDefault();
      this.filter
        .uploadFiles(e.dataTransfer?.items)
        .then(() => console.log('[ttyd] upload success'))
        .catch((err) => console.log('[ttyd] upload failed: ' + err));
    });
    this.disposables.push(terminal.onResize((size) => this.filter.setTerminalColumns(size.cols)));
  }

  listen(target, type, fn) {
    target.addEventListener(type, fn);
    this.disposables.push({ dispose: () => target.removeEventListener(type, fn) });
  }

  dispose() {
    for (const d of this.disposables) d.dispose();
    this.disposables.length = 0;
  }

  consume(data) {
    try {
      this.filter.processServerOutput(data);
    } catch (e) {
      console.error('[ttyd] trzsz consume: ', e);
      this.terminal.options.disableStdin = false;
      this.terminal.focus();
    }
  }
}
