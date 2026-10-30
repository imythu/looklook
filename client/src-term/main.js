// 看看自己的终端页（/i/{id}/ 的首页，网关直接返回，不再用 ttyd 内嵌的页面）。
// 原作 ttyd：Copyright (c) 2016-2022 Shuanglei Tao，MIT 许可（见开源仓库的 THIRD_PARTY_NOTICES.md）。
// 由 ttyd 1.7.7 的前端（html/src，MIT）移植而来：与 ttyd 的 WebSocket 协议、选项（ttyd 的 -t）、重连、流控、trzsz 都照旧，
// window.term / term.fit() 也照旧，term-inject.js 不用改。不同的地方：
// 1. xterm.js 换成 6.0：支持同步输出（DEC 2026）——Claude Code、Codex 等整屏重画时一帧画完，不再闪、不再撕裂；
// 2. 默认用 WebGL 绘制，画不了（不支持、上下文丢失）时自动退回 DOM；
// 3. 程序报告的进度（OSC 9;4，tmux 的 progressbar 特性转发）显示成终端顶部的一条细进度条；
// 4. 去掉了 ZMODEM（tmux 吞掉它的控制字节，ttyd 一直没开）与 Canvas 绘制（xterm.js 6 已移除）。
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { ProgressAddon } from '@xterm/addon-progress';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebglAddon } from '@xterm/addon-webgl';
import { WebLinksAddon } from '@xterm/addon-web-links';
import '@xterm/xterm/css/xterm.css';

import { OverlayAddon } from './overlay.js';
import { ProgressBar } from './progress.js';
import { TrzszAddon } from './trzsz.js';
import './style.css';

// 服务端 → 浏览器 / 浏览器 → 服务端的消息类型（第一个字节）
const OUTPUT = '0', SET_WINDOW_TITLE = '1', SET_PREFERENCES = '2';
const INPUT = '0', RESIZE_TERMINAL = '1', PAUSE = '2', RESUME = '3';

const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
const path = location.pathname.replace(/[/]+$/, '');
const wsUrl = [proto, '//', location.host, path, '/ws', location.search].join('');
const tokenUrl = [location.protocol, '//', location.host, path, '/token'].join('');
const clientOptions = {
  rendererType: 'webgl',
  disableLeaveAlert: false,
  disableResizeOverlay: false,
  enableTrzsz: false,
  isWindows: false,
  unicodeVersion: '11',
};
const termOptions = {
  fontSize: 13,
  fontFamily: 'Consolas,Liberation Mono,Menlo,Courier,monospace',
  allowProposedApi: true,
};
const flowControl = { limit: 100000, highWater: 10, lowWater: 4 };

function listen(target, type, fn) {
  target.addEventListener(type, fn);
  return { dispose: () => target.removeEventListener(type, fn) };
}

class Xterm {
  constructor() {
    this.disposables = [];
    this.encoder = new TextEncoder();
    this.decoder = new TextDecoder();
    this.written = 0;
    this.pending = 0;
    this.fitAddon = new FitAddon();
    this.overlay = new OverlayAddon();
    this.webgl = undefined;
    this.trzsz = undefined;
    this.socket = undefined;
    this.token = '';
    this.opened = false;
    this.title = undefined;
    this.titleFixed = undefined;
    this.resizeOverlay = true;
    this.reconnect = true;
    this.doReconnect = true;
    this.writeFunc = (data) => this.writeData(new Uint8Array(data));
    this.onUnload = this.onUnload.bind(this);
  }

  register(d) {
    this.disposables.push(d);
    return d;
  }

  dispose() {
    for (const d of this.disposables) d.dispose();
    this.disposables.length = 0;
  }

  async refreshToken() {
    try {
      const resp = await fetch(tokenUrl);
      if (resp.ok) this.token = (await resp.json()).token;
    } catch (e) {
      console.error(`[ttyd] fetch ${tokenUrl}: `, e);
    }
  }

  onUnload(event) {
    event.preventDefault();
    if (this.socket?.readyState === WebSocket.OPEN) {
      const message = 'Close terminal? this will also terminate the command.';
      event.returnValue = message;
      return message;
    }
    return undefined;
  }

  open(parent) {
    const terminal = (this.terminal = new Terminal(termOptions));
    window.term = terminal;
    terminal.fit = () => this.fitAddon.fit();
    terminal.loadAddon(this.fitAddon);
    terminal.loadAddon(this.overlay);
    terminal.loadAddon(new WebLinksAddon());
    const progress = new ProgressAddon();
    terminal.loadAddon(progress);
    terminal.open(parent);
    const bar = new ProgressBar(parent, () => terminal.options.theme);
    progress.onChange((p) => bar.set(p));
    this.fitAddon.fit();
    // 网页字体（终端字体由 /fonts/fonts.css 提供）加载完时 term-inject.js 会让 xterm 重新测量字符宽度；
    // WebGL 的字形图集是按旧字体画的，一并清掉重画。
    document.fonts?.addEventListener?.('loadingdone', () => this.webgl?.clearTextureAtlas());
    // xterm 6 只给里层（.xterm-scrollable-element）涂配色底色，外层 .xterm-viewport 留着 CSS 里的 #000，
    // 盖住了四周 5px 的留白，浅色配色时终端外面一圈黑框。跟着里层的底色一起涂（换配色时 xterm 会改里层的 style）。
    const inner = parent.querySelector('.xterm-scrollable-element');
    const outer = parent.querySelector('.xterm-viewport');
    if (inner && outer) {
      const paint = () => (outer.style.backgroundColor = inner.style.backgroundColor);
      paint();
      new MutationObserver(paint).observe(inner, { attributes: true, attributeFilter: ['style'] });
    }
  }

  initListeners() {
    const { terminal, fitAddon, overlay } = this;
    this.register(terminal.onTitleChange((data) => {
      if (data && !this.titleFixed) document.title = data + ' | ' + this.title;
    }));
    this.register(terminal.onData((data) => this.sendData(data)));
    this.register(terminal.onBinary((data) => this.sendData(Uint8Array.from(data, (v) => v.charCodeAt(0)))));
    this.register(terminal.onResize(({ cols, rows }) => {
      const msg = JSON.stringify({ columns: cols, rows });
      this.socket?.send(this.encoder.encode(RESIZE_TERMINAL + msg));
      if (this.resizeOverlay) overlay.show(`${cols}x${rows}`, 300);
    }));
    this.register(terminal.onSelectionChange(() => {
      if (terminal.getSelection() === '') return;
      try {
        document.execCommand('copy');
      } catch {
        return;
      }
      overlay.show('✂', 200);
    }));
    this.register(listen(window, 'resize', () => fitAddon.fit()));
    this.register(listen(window, 'beforeunload', this.onUnload));
  }

  writeData(data) {
    const { terminal } = this;
    const { limit, highWater, lowWater } = flowControl;
    this.written += data.length;
    if (this.written > limit) {
      terminal.write(data, () => {
        this.pending = Math.max(this.pending - 1, 0);
        if (this.pending < lowWater) this.socket?.send(this.encoder.encode(RESUME));
      });
      this.pending++;
      this.written = 0;
      if (this.pending > highWater) this.socket?.send(this.encoder.encode(PAUSE));
    } else {
      terminal.write(data);
    }
  }

  sendData(data) {
    const { socket } = this;
    if (socket?.readyState !== WebSocket.OPEN) return;
    if (typeof data === 'string') {
      const payload = new Uint8Array(data.length * 3 + 1);
      payload[0] = INPUT.charCodeAt(0);
      const stats = this.encoder.encodeInto(data, payload.subarray(1));
      socket.send(payload.subarray(0, stats.written + 1));
    } else {
      const payload = new Uint8Array(data.length + 1);
      payload[0] = INPUT.charCodeAt(0);
      payload.set(data, 1);
      socket.send(payload);
    }
  }

  // window.WebSocket 在这里现取：term-inject.js 把它换成了观察连接状态（远程时走 WebRTC）的版本
  connect() {
    const socket = (this.socket = new WebSocket(wsUrl, ['tty']));
    socket.binaryType = 'arraybuffer';
    this.register(listen(socket, 'open', () => this.onSocketOpen()));
    this.register(listen(socket, 'message', (e) => this.onSocketData(e)));
    this.register(listen(socket, 'close', (e) => this.onSocketClose(e)));
    this.register(listen(socket, 'error', () => (this.doReconnect = false)));
  }

  onSocketOpen() {
    console.log('[ttyd] websocket connection opened');
    const { terminal, overlay } = this;
    const msg = JSON.stringify({ AuthToken: this.token, columns: terminal.cols, rows: terminal.rows });
    this.socket?.send(this.encoder.encode(msg));
    if (this.opened) {
      terminal.reset();
      terminal.options.disableStdin = false;
      overlay.show('Reconnected', 300);
    } else {
      this.opened = true;
    }
    this.doReconnect = this.reconnect;
    this.initListeners();
    terminal.focus();
  }

  onSocketClose(event) {
    console.log(`[ttyd] websocket connection closed with code: ${event.code}`);
    const { overlay } = this;
    const doReconnect = this.doReconnect;
    overlay.show('Connection Closed');
    this.dispose();
    // 1000：正常关闭
    if (event.code !== 1000 && doReconnect) {
      overlay.show('Reconnecting...');
      this.refreshToken().then(() => this.connect());
    } else {
      const keyDispose = this.terminal.onKey((e) => {
        if (e.domEvent.key === 'Enter') {
          keyDispose.dispose();
          overlay.show('Reconnecting...');
          this.refreshToken().then(() => this.connect());
        }
      });
      overlay.show('Press ⏎ to Reconnect');
    }
  }

  // 地址上的 ?k=v 覆盖选项（ttyd 的行为，按已有值的类型解析）
  optsFromQuery(query) {
    const prefs = {};
    for (const [k, raw] of new URLSearchParams(query)) {
      let v = clientOptions[k];
      if (v === undefined) v = this.terminal.options[k];
      switch (typeof v) {
        case 'boolean':
          prefs[k] = raw === 'true' || raw === '1';
          break;
        case 'number':
        case 'bigint':
          prefs[k] = Number.parseInt(raw, 10);
          break;
        case 'object':
          try {
            prefs[k] = JSON.parse(raw);
          } catch {
            // 忽略写错的值
          }
          break;
        default:
          prefs[k] = raw;
          break;
      }
    }
    return prefs;
  }

  onSocketData(event) {
    const raw = event.data;
    const cmd = String.fromCharCode(new Uint8Array(raw)[0]);
    const data = raw.slice(1);
    switch (cmd) {
      case OUTPUT:
        this.writeFunc(data);
        break;
      case SET_WINDOW_TITLE:
        this.title = this.decoder.decode(data);
        document.title = this.title;
        break;
      case SET_PREFERENCES:
        this.applyPreferences({ ...clientOptions, ...JSON.parse(this.decoder.decode(data)), ...this.optsFromQuery(location.search) });
        break;
      default:
        console.warn(`[ttyd] unknown command: ${cmd}`);
        break;
    }
  }

  applyPreferences(prefs) {
    const { terminal, fitAddon } = this;
    if (prefs.enableTrzsz) {
      this.trzsz = new TrzszAddon({
        windows: prefs.isWindows,
        dragInitTimeout: prefs.trzszDragInitTimeout,
        sender: (d) => this.sendData(d),
        writer: (d) => this.writeData(d),
      });
      this.writeFunc = (data) => this.trzsz?.consume(data);
      terminal.loadAddon(this.register(this.trzsz));
    }
    for (const [key, value] of Object.entries(prefs)) {
      switch (key) {
        case 'rendererType':
          this.setRenderer(value);
          break;
        case 'disableLeaveAlert':
          if (value) window.removeEventListener('beforeunload', this.onUnload);
          break;
        case 'disableResizeOverlay':
          if (value) this.resizeOverlay = false;
          break;
        case 'disableReconnect':
          if (value) {
            this.reconnect = false;
            this.doReconnect = false;
          }
          break;
        case 'titleFixed':
          if (!value) break;
          this.titleFixed = value;
          document.title = value;
          break;
        case 'unicodeVersion':
          if (String(value) !== '6') {
            terminal.loadAddon(new Unicode11Addon());
            terminal.unicode.activeVersion = '11';
          }
          break;
        // 只给本页用的开关（ttyd 自己的选项），不是 xterm.js 的选项
        case 'enableTrzsz':
        case 'enableZmodem':
        case 'enableSixel':
        case 'isWindows':
        case 'trzszDragInitTimeout':
          break;
        default:
          if (terminal.options[key] instanceof Object) terminal.options[key] = Object.assign({}, terminal.options[key], value);
          else terminal.options[key] = value;
          if (key.indexOf('font') === 0) fitAddon.fit();
          break;
      }
    }
    // lineHeight、letterSpacing 也改变格子大小，上面只在 font* 时重新适配，最后统一再来一次（否则最下面几行被裁掉）
    fitAddon.fit();
  }

  setRenderer(type) {
    const dropWebgl = () => {
      try {
        this.webgl?.dispose();
      } catch {
        // 忽略
      }
      this.webgl = undefined;
      document.documentElement.dataset.renderer = 'dom';
    };
    if (type !== 'webgl') return dropWebgl();
    if (this.webgl) return;
    try {
      const webgl = new WebglAddon();
      // 上下文丢失（显卡驱动重置、同时开着的 WebGL 页面太多被浏览器回收）：退回 DOM 绘制，终端照常可用
      webgl.onContextLoss(() => {
        console.log('[ttyd] WebGL context lost, using the DOM renderer');
        dropWebgl();
      });
      this.terminal.loadAddon(webgl);
      this.webgl = webgl;
      document.documentElement.dataset.renderer = 'webgl';
    } catch (e) {
      console.log('[ttyd] WebGL renderer could not be loaded, using the DOM renderer', e);
      dropWebgl();
    }
  }
}

const xterm = new Xterm();
const container = document.getElementById('terminal-container');
(async () => {
  await xterm.refreshToken();
  xterm.open(container);
  xterm.connect();
})();
