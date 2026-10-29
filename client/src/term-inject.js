// 看看：注入到 ttyd 终端页面的辅助脚本（由本机网关在 /i/{id}/ 首页的 </head> 前插入）。
// 1. 终端字体是网页字体：加载完成后让 xterm.js 重新测量字符宽度并适配窗口；
// 2. 处理 OSC 52：tmux 里拖选复制的内容写入浏览器剪贴板；
// 3. 提供 window.looklookSend(data)，供外层管理台（手机按键条）向终端发送按键；
// 4. 按键条上点了 Ctrl / Alt / Shift 后，手机键盘打的下一个字符也套用这些修饰键；
// 5. 文件传输（docs/FILE_TRANSFER.md §5）：在输出里发现 rz / sz 时通知管理台；嵌在管理台里时把粘贴/拖进来的文件交给管理台上传；
//    提供 window.looklookPaste(text)，管理台上传完成后把路径填进终端。
// 6. 手机上管住键盘的弹出：只有用户点了终端（或按键条要求）才弹出，并把聚焦变化告诉外层（规则见 src-ui/shared/KeyBar.jsx 开头）。
// 7. 手机上滑动终端翻看上文（含 AI 编程助手的输出），见 setupTouchScroll。
// 8. 本机直连（gateway/direct.rs）：远程子站页面直接嵌入 http://127.0.0.1 上的终端，外层与终端跨源。
//    外层在地址的 # 后面带上自己的来源（ll-parent=…），消息只发给、只收自这个来源的父窗口；
//    能嵌入这里的来源已由本机的 CSP frame-ancestors 限定，所以写错了来源只会收不到消息。
// 9. ⇧↵ 换行（发 Ctrl+J）；Windows / Linux 上 Ctrl+V 只粘贴、不再把 ^V 发给程序。见 setupKeys。
// 10. 复制：⌘C（macOS）/ Ctrl+Shift+C（Windows / Linux）复制选中的文字。见 setupCopy。
//     链接：⌘ / Ctrl + 点击直接打开；普通点击在旁边弹出“打开 / 复制链接”。见 setupLinks。
// 15. 连接状态告诉管理台（looklook:conn）；连不上、正在连接时手机上可以下拉刷新整页。见 setupPullRefresh。
//     管理台说终端所在的电脑离线了（looklook:offline）：连接断着时终端只能看、不能输入，提示由管理台显示；
//     电脑回来后马上重连。见 setOffline。
// 11. 切到别的窗口再回来时重新适配、整屏重画，免得只画了一部分。见 refitOnReturn。
// 12. 选择文字：手机上长按终端（满 0.5 秒那一行亮起，松手）时通知管理台打开“选择文字”并定位到按住的那一行（用浏览器自己的选择，见 src-ui/shared/TextSelect.jsx）；
//     提供 window.looklookText() 读出终端缓冲区的文字（没有 tmux 时用）。tmux 里拖选后浏览器不让直接写剪贴板时给一个“复制”按钮。
// 13. 同一个终端在几处打开（电脑和手机）：tmux 窗口按最近有输入的那一处的尺寸排版，另一处看到的是按别的屏幕排的画面。
//     页面回到前台、拿到焦点、手指/鼠标按到终端上时重新报一次尺寸，让 tmux 按这一处排版。见 claimSize。
// 14. 远程管理台走 WebRTC 时（服务端 docs/06）：顶层是平台的启动页，持有到电脑的 DataChannel；
//     终端的 WebSocket 改由启动页经 DataChannel 打开（window.top.__llrtc），见 RtcSocket。
(function () {
  'use strict';
  var tries = 0;
  var HASH = {};
  (location.hash || '').replace(/^#/, '').split('&').forEach(function (kv) {
    var i = kv.indexOf('=');
    if (i > 0) { try { HASH[kv.slice(0, i)] = decodeURIComponent(kv.slice(i + 1)); } catch (e) { /* 忽略 */ } }
  });
  var PARENT = /^https?:\/\/[a-z0-9.:\[\]-]+$/i.test(HASH['ll-parent'] || '') ? HASH['ll-parent'] : location.origin;
  var CROSS = PARENT !== location.origin;
  function fromParent(e) { return e.source === window.parent && e.origin === PARENT && e.data && typeof e.data === 'object'; }
  // 与管理台共用语言设置（同源 localStorage）；英文未翻译时回退中文。
  var TEXT = {
    'zh-CN': { copy: '复制', copy_hint: '复制选中的文字', copy_failed: '浏览器不允许复制', copied: '已复制', copy_offer: '复制选中的文字', link_open: '打开', link_copy: '复制链接', link_hint: '⌘ / Ctrl + 点击直接打开', pull: '下拉刷新', release: '松开刷新', connecting: '正在连接终端…', lost: '正在重新连接…', lost_slow: '暂时连不上电脑，正在重试…', ended: '终端连接已断开', back: '已重新连接', reload: '立即重试', reopen: '重新连接' },
    'en-US': { copy: 'Copy', copy_hint: 'Copy the selected text', copy_failed: 'The browser blocked copying', copied: 'Copied', copy_offer: 'Copy the selected text', link_open: 'Open', link_copy: 'Copy link', link_hint: '⌘ / Ctrl + click opens directly', pull: 'Pull to refresh', release: 'Release to refresh', connecting: 'Connecting to the terminal…', lost: 'Reconnecting…', lost_slow: 'Cannot reach your computer, still retrying…', ended: 'The terminal disconnected', back: 'Reconnected', reload: 'Retry now', reopen: 'Reconnect' }
  };
  function tr(key) {
    var lang = 'zh-CN';
    try { lang = (CROSS && HASH['ll-lang']) || localStorage.getItem('ll_locale') || (navigator.language || '').indexOf('en') === 0 && 'en-US' || 'zh-CN'; } catch (e) { /* 忽略 */ }
    return (TEXT[lang] && TEXT[lang][key]) || TEXT['zh-CN'][key];
  }

  // 连接状态提示：首次连接时盖一层“正在连接”；断线后由这里负责重连（见 watchSocket）：顶部一枚小提示，
  // 1 秒内连回来就不打扰，连不上时退避重试，回到前台、网络恢复、点“立即重试”时马上再试；重连成功短暂提示。
  // 在 ttyd 创建 WebSocket 之前包一层，只观察 /ws 连接的开闭。
  var overlay = null, banner = null, everOpen = false, socket = null;
  var lostAt = 0, failures = 0, retryAt = 0, wake = null, bannerTimer = 0, ended = false;
  // 管理台说电脑离线了；TERM：准备好的终端（setup 之后才有）
  var parentOffline = false, TERM = null;
  var css = document.createElement('style');
  css.textContent =
    '@keyframes llb{0%,80%,100%{transform:scale(.6);opacity:.5}40%{transform:scale(1);opacity:1}}' +
    '.ll-dots{display:inline-flex;gap:5px;vertical-align:middle;margin-right:8px}' +
    '.ll-dots i{width:8px;height:8px;border-radius:50%;background:#8b93ff;animation:llb 1.2s infinite ease-in-out}' +
    '.ll-dots i:nth-child(2){animation-delay:.15s}.ll-dots i:nth-child(3){animation-delay:.3s}' +
    '.ll-pill{position:fixed;top:10px;left:50%;transform:translateX(-50%);z-index:99;display:flex;align-items:center;gap:10px;max-width:calc(100% - 24px);' +
    'padding:6px 6px 6px 14px;border-radius:999px;background:rgba(24,26,36,.92);color:#e6e8ef;font:13px/1.5 system-ui,sans-serif;' +
    'box-shadow:0 4px 18px rgba(0,0,0,.35);border:1px solid rgba(255,255,255,.08);transition:opacity .2s;white-space:nowrap}' +
    '.ll-pill span{overflow:hidden;text-overflow:ellipsis}' +
    '.ll-pill .ll-dots{margin-right:2px;gap:4px}.ll-pill .ll-dots i{width:6px;height:6px;background:#f5b14c}' +
    '.ll-pill button{flex:none;border:0;border-radius:999px;padding:4px 12px;background:rgba(255,255,255,.12);color:#fff;font:inherit;cursor:pointer}' +
    '.ll-pill button:active{background:rgba(255,255,255,.22)}' +
    // 断开后不显示 ttyd 的“Press ⏎ to Reconnect / Reconnecting...”，由上面的提示代替（它的提示框是 .xterm 下圆角 15px 的 div）
    'html.ll-down .xterm>div[style*="border-radius: 15px"]{display:none!important}' +
    '@media(prefers-reduced-motion:reduce){.ll-dots i{animation:none}}';
  document.documentElement.appendChild(css);
  var DOTS = '<span class="ll-dots"><i></i><i></i><i></i></span>';
  function mount(el) { document.documentElement.appendChild(el); return el; }
  // 终端连得正常吗：告诉管理台，也决定手机上能不能下拉刷新（正常时下拉是翻看终端）
  var connOk = false;
  function connState(ok) {
    connOk = ok;
    try { if (window.parent !== window) window.parent.postMessage({ type: 'looklook:conn', ok: ok }, PARENT); } catch (e) { /* 忽略 */ }
  }
  function showConnecting() {
    if (overlay || everOpen) return;
    overlay = mount(document.createElement('div'));
    overlay.style.cssText = 'position:fixed;inset:0;z-index:98;display:grid;place-items:center;background:#11131a;color:#c5cad6;font:15px/1.6 system-ui,sans-serif';
    overlay.innerHTML = '<div>' + DOTS + tr('connecting') + '</div>';
  }
  function hideConnecting() { if (overlay) { overlay.remove(); overlay = null; } }
  function hideBanner() {
    clearTimeout(bannerTimer);
    if (banner) { banner.remove(); banner = null; }
  }
  function renderBanner() {
    // 电脑离线：管理台那边已经说明了，这里不再叠一个“正在重新连接”
    if (parentOffline && !ended) return hideBanner();
    if (!banner) banner = mount(document.createElement('div'));
    banner.className = 'll-pill';
    banner.setAttribute('role', 'status');
    var slow = !ended && Date.now() - lostAt > 15000;
    banner.innerHTML = (ended ? '' : DOTS) + '<span></span>';
    banner.querySelector('span').textContent = tr(ended ? 'ended' : slow ? 'lost_slow' : 'lost');
    var b = document.createElement('button');
    b.type = 'button';
    b.textContent = tr(ended ? 'reopen' : 'reload');
    b.onclick = retryNow;
    banner.appendChild(b);
    // 过了 15 秒换成“暂时连不上”
    if (!ended && !slow) { clearTimeout(bannerTimer); bannerTimer = setTimeout(renderBanner, 15000 - (Date.now() - lostAt) + 50); }
  }
  /** `clean`：终端正常结束（关闭码 1000，比如程序退出了），ttyd 不会自己重连，等用户点。 */
  function showLost(clean) {
    hideConnecting();
    connState(false);
    if (!lostAt) lostAt = Date.now();
    ended = clean;
    document.documentElement.classList.add('ll-down');
    readOnly();
    clearTimeout(bannerTimer);
    if (banner || clean) renderBanner();
    else bannerTimer = setTimeout(renderBanner, 1000);
  }
  function onOpen() {
    var was = Boolean(banner);
    everOpen = true;
    connState(true);
    lostAt = 0; failures = 0; retryAt = 0; ended = false;
    document.documentElement.classList.remove('ll-down');
    readOnly();
    hideConnecting();
    hideBanner();
    if (was && document.body) toast(tr('back'));
  }
  /** 下一次重连前要等多久：0.5、1、2、4、8 秒，之后每 10 秒。 */
  function scheduleRetry() {
    retryAt = Date.now() + Math.min(500 * Math.pow(2, failures), 10000);
    failures++;
  }
  function backoff() {
    var ms = retryAt - Date.now();
    if (ms <= 0) return Promise.resolve();
    return new Promise(function (resolve) {
      var t = setTimeout(done, ms);
      function done() { clearTimeout(t); if (wake === done) wake = null; resolve(); }
      wake = done;
    });
  }
  function kick() { retryAt = 0; if (wake) wake(); }
  /** 电脑离线、连接也断着时终端不接受输入（打的字没处送，免得以为发出去了）；内容照样能看、能翻、能选 */
  function readOnly() {
    if (TERM && TERM.options) TERM.options.disableStdin = parentOffline && !connOk;
  }
  /**
   * 管理台换了配色（looklook:theme，配色名）：ttyd 启动时 `-t theme=` 定下的配色只管新开的终端，
   * 已经开着的在这里换（配色表由客户端在脚本前面带上，见 gateway/assets.rs）。终端还没准备好时先记下。
   */
  var wantTheme = null;
  function setTheme(name) {
    var th = window.__llThemes && window.__llThemes[name];
    if (!th) return;
    wantTheme = name;
    // 终端周围露出来的那一圈（ttyd 页面本身的底色）也跟着换
    document.documentElement.style.background = th.background;
    if (document.body) document.body.style.background = th.background;
    if (TERM && TERM.options) TERM.options.theme = th;
  }
  /** 管理台告诉我们电脑离线了 / 又在线了（looklook:offline） */
  function setOffline(on) {
    if (parentOffline === on) return;
    parentOffline = on;
    readOnly();
    if (on) { if (!ended) hideBanner(); return; }
    // 电脑回来了：断着的话马上重连，不等退避
    if (!connOk && everOpen) { kick(); if (lostAt) renderBanner(); }
  }
  function retryNow() {
    // 正常结束、或者 ttyd 没在等重连（不在退避中）时整页重开
    if (ended || !wake) return void location.reload();
    kick();
  }
  document.addEventListener('visibilitychange', function () { if (document.visibilityState === 'visible' && lostAt) kick(); });
  window.addEventListener('online', function () { if (lostAt) kick(); });
  // ttyd 重连前先取 /token（refreshToken），在这里退避；取 token 失败它也会接着连，连不上再回到这里
  (function wrapFetch() {
    var fetch0 = window.fetch;
    if (!fetch0) return;
    window.fetch = function (input) {
      var args = arguments;
      var url = typeof input === 'string' ? input : input && input.url;
      if (!lostAt || !/\/token(\?|$)/.test(String(url || ''))) return fetch0.apply(this, args);
      var self = this;
      return backoff().then(function () { return fetch0.apply(self, args); });
    };
  })();
  // 启动页提供的 WebRTC 通道（同源的顶层窗口；本机直连等跨源嵌入时读不到，返回 null）。
  function rtcBridge() {
    try {
      var b = window.top !== window && window.top.__llrtc;
      return b && b.proto === 1 && typeof b.openWs === 'function' ? b : null;
    } catch (e) { return null; }
  }
  // 与 WebSocket 接口一致的替身：数据经启动页的 DataChannel 收发。收到的数据复制成本页面的 ArrayBuffer /
  // 字符串（跨窗口的对象在 instanceof 判断里不算本页的 ArrayBuffer）。
  function RtcSocket(bridge, url, protocols) {
    var self = this;
    var u = new URL(String(url), location.href);
    self.url = u.href;
    self.readyState = 0;
    self.protocol = '';
    self.extensions = '';
    self.binaryType = 'blob';
    self._l = {};
    var dec = new TextDecoder();
    var list = protocols === undefined ? [] : [].concat(protocols).map(String);
    self._h = bridge.openWs(u.pathname + u.search, list, {
      onopen: function (proto) {
        if (self.readyState !== 0) return;
        self.readyState = 1;
        self.protocol = proto || '';
        self._emit('open', new Event('open'));
      },
      onmessage: function (text, bytes) {
        if (self.readyState !== 1) return;
        var data;
        if (text) data = dec.decode(bytes);
        else {
          var copy = new Uint8Array(bytes.length);
          copy.set(bytes);
          data = self.binaryType === 'arraybuffer' ? copy.buffer : new Blob([copy]);
        }
        self._emit('message', new MessageEvent('message', { data: data, origin: location.origin }));
      },
      onclose: function (code) {
        if (self.readyState === 3) return;
        var clean = code === 1000;
        if (!clean) self._emit('error', new Event('error'));
        self.readyState = 3;
        self._emit('close', new CloseEvent('close', { code: code || 1006, reason: '', wasClean: clean }));
      }
    });
  }
  RtcSocket.CONNECTING = 0; RtcSocket.OPEN = 1; RtcSocket.CLOSING = 2; RtcSocket.CLOSED = 3;
  RtcSocket.prototype = {
    CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3,
    onopen: null, onmessage: null, onclose: null, onerror: null,
    get bufferedAmount() { return this._h ? this._h.buffered() : 0; },
    addEventListener: function (type, fn) { (this._l[type] = this._l[type] || []).push(fn); },
    removeEventListener: function (type, fn) {
      var a = this._l[type];
      if (a) { var i = a.indexOf(fn); if (i >= 0) a.splice(i, 1); }
    },
    dispatchEvent: function (ev) { this._emit(ev.type, ev); return true; },
    _emit: function (type, ev) {
      var self = this;
      var on = self['on' + type];
      try { if (typeof on === 'function') on.call(self, ev); } catch (e) { setTimeout(function () { throw e; }); }
      (self._l[type] || []).slice().forEach(function (fn) {
        try { typeof fn === 'function' ? fn.call(self, ev) : fn.handleEvent(ev); } catch (e) { setTimeout(function () { throw e; }); }
      });
    },
    // Blob 要异步读出来：它后面发的消息排在它后面（否则会插队），没有排着的就直接发
    send: function (data) {
      if (this.readyState === 0) throw new DOMException('Still in CONNECTING state.', 'InvalidStateError');
      if (this.readyState !== 1) return;
      var self = this;
      var text = typeof data === 'string';
      var body;
      if (text) body = new TextEncoder().encode(data);
      else if (data instanceof Blob) body = data.arrayBuffer().then(function (b) { return new Uint8Array(b); });
      else body = ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : new Uint8Array(data);
      if (!self._q && !(body instanceof Promise)) return void self._h.send(text, body);
      // 排队时先复制一份：WebSocket.send 之后调用方可以改它的缓冲区
      if (!(body instanceof Promise) && !text) body = body.slice();
      var q = (self._q || Promise.resolve()).then(function () { return body; }).then(function (b) {
        if (self.readyState === 1) self._h.send(text, b);
      }, function () {});
      self._q = q;
      q.then(function () { if (self._q === q) self._q = null; });
    },
    close: function () {
      if (this.readyState >= 2) return;
      this.readyState = 2;
      this._h.close();
    }
  };

  (function watchSocket() {
    var bridge = rtcBridge();
    var Native = bridge
      ? function (url, protocols) { return new RtcSocket(bridge, url, protocols); }
      : window.WebSocket;
    if (!Native) return;
    if (bridge) { Native.prototype = RtcSocket.prototype; ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'].forEach(function (k) { Native[k] = RtcSocket[k]; }); }
    function Wrapped(url, protocols) {
      var ws = protocols === undefined ? new Native(url) : new Native(url, protocols);
      if (/\/ws(\?|$)/.test(String(url))) {
        if (!everOpen) showConnecting();
        ws.addEventListener('open', onOpen);
        ws.addEventListener('close', function (e) {
          var clean = e.code === 1000;
          if (!clean) scheduleRetry();
          showLost(clean);
        });
        // ttyd 收到 error 就不再自动重连，只显示“Press ⏎ to Reconnect”：断线一律由它照常重连，节奏由上面的退避控制
        var add = ws.addEventListener;
        ws.addEventListener = function (type) {
          if (type === 'error') return;
          return add.apply(ws, arguments);
        };
        ws.addEventListener('message', function (e) { if (e.data instanceof ArrayBuffer) watchTransfer(e.data); });
        transferTail = '';
        socket = ws;
        var nativeSend = ws.send;
        ws.send = function (data) {
          if (bareMotion(data)) return;
          if (data instanceof Uint8Array && data[0] === 0x30) lastLocal = Date.now();
          return nativeSend.call(ws, withMods(data));
        };
      }
      return ws;
    }
    Wrapped.prototype = Native.prototype;
    ['CONNECTING', 'OPEN', 'CLOSING', 'CLOSED'].forEach(function (k) { Wrapped[k] = Native[k]; });
    window.WebSocket = Wrapped;
  })();

  // ---------- 文件传输 ----------
  function toParent(msg) {
    try { if (window.parent !== window) window.parent.postMessage(msg, PARENT); } catch (e) { /* 忽略 */ }
  }
  // 管理台在 iframe 元素上设置 looklookFiles = true 才接管粘贴/拖放；裸 /i/{id}/ 保持浏览器默认行为。
  function filesWanted() {
    if (CROSS) return HASH['ll-files'] === '1'; // 跨源时读不到 frameElement
    try { return Boolean(window.frameElement && window.frameElement.looklookFiles); } catch (e) { return false; }
  }

  // rz / sz 横幅：tmux 会吞掉 ZDLE（\x18），所以 \x18 可有可无。输出按块到达，保留上一块末尾一小段，横幅跨块也能匹配。
  // 去重：同一种横幅在 QUIET 毫秒内再次出现（rz 每隔几秒重发 ZRINIT、tmux 重绘）只算一次，每次出现都顺延；
  // 这之后的重复出现由管理台按进程（pid）去重，并忽略 rz/sz 已经退出的情况。
  var BANNERS = [
    { kind: 'rz', re: /rz waiting to receive\.|\*\*\x18?B0100/g },
    { kind: 'sz', re: /\*\*\x18?B00000000000000/g }
  ];
  var TAIL = 40, QUIET = 15000;
  var transferTail = '', lastSeen = {};
  var latin1 = null;
  function watchTransfer(buf) {
    var bytes = new Uint8Array(buf);
    if (bytes[0] !== 0x30 || bytes.length < 2) return;
    try { latin1 = latin1 || new TextDecoder('latin1'); } catch (e) { return; }
    var text = transferTail + latin1.decode(bytes.subarray(1));
    var fresh = transferTail.length;
    transferTail = text.slice(-TAIL);
    var now = Date.now();
    BANNERS.forEach(function (b) {
      b.re.lastIndex = 0;
      var m, hit = false;
      while ((m = b.re.exec(text))) if (m.index + m[0].length > fresh) { hit = true; break; }
      if (!hit) return;
      var quiet = lastSeen[b.kind] && now - lastSeen[b.kind] < QUIET;
      lastSeen[b.kind] = now;
      if (!quiet) toParent({ type: 'looklook:transfer', kind: b.kind });
    });
  }

  function hasFiles(dt) {
    if (!dt) return false;
    if (dt.types && Array.prototype.indexOf.call(dt.types, 'Files') >= 0) return true;
    return Boolean(dt.files && dt.files.length);
  }
  function fileList(dt) {
    var out = [];
    if (dt.items && dt.items.length) {
      for (var i = 0; i < dt.items.length; i++) {
        var it = dt.items[i];
        if (it.kind === 'file') { var f = it.getAsFile(); if (f) out.push(f); }
      }
    }
    if (!out.length && dt.files) for (var j = 0; j < dt.files.length; j++) out.push(dt.files[j]);
    return out;
  }
  function stopEvent(e) { e.preventDefault(); e.stopImmediatePropagation(); e.stopPropagation(); }
  // 捕获阶段，在 xterm（粘贴）和 ttyd 的 trzsz 插件（拖放时自动输入 trz）之前处理。
  // 粘贴文件只在 PC 上接管（手机上走面板里的“选择文件/照片”）。
  function touchDevice() { return Boolean(window.matchMedia && window.matchMedia('(hover: none) and (pointer: coarse)').matches); }
  window.addEventListener('paste', function (e) {
    if (!filesWanted() || !e.clipboardData || touchDevice()) return;
    var files = fileList(e.clipboardData);
    if (!files.length) return; // 纯文本粘贴照常交给终端
    stopEvent(e);
    toParent({ type: 'looklook:files', files: files, source: 'paste' });
  }, true);
  var dragDepth = 0, dragOn = false;
  function dragging(on) { if (on !== dragOn) { dragOn = on; toParent({ type: 'looklook:dragging', on: on }); } }
  window.addEventListener('dragenter', function (e) {
    if (!filesWanted() || !hasFiles(e.dataTransfer)) return;
    stopEvent(e);
    dragDepth++;
    dragging(true);
  }, true);
  window.addEventListener('dragover', function (e) {
    if (!filesWanted() || !hasFiles(e.dataTransfer)) return;
    stopEvent(e);
    e.dataTransfer.dropEffect = 'copy';
    dragging(true); // 有的浏览器在子元素之间移动时也报 relatedTarget 为 null，靠 dragover 补回来
  }, true);
  window.addEventListener('dragleave', function (e) {
    if (!filesWanted() || !hasFiles(e.dataTransfer)) return;
    stopEvent(e);
    // 离开 iframe 时 relatedTarget 为 null；在子元素之间移动时计数不归零。
    if (--dragDepth <= 0 || !e.relatedTarget) { dragDepth = 0; dragging(false); }
  }, true);
  window.addEventListener('drop', function (e) {
    if (!filesWanted() || !hasFiles(e.dataTransfer)) return;
    stopEvent(e);
    dragDepth = 0;
    dragging(false);
    var files = fileList(e.dataTransfer);
    if (files.length) toParent({ type: 'looklook:files', files: files, source: 'drop' });
  }, true);

  // 按键条的修饰键状态由外层挂在 iframe 元素上：{ get() → {ctrl, alt, shift}, set(mods) }，
  // 值为 null / 'once'（只作用于下一个键）/ 'lock'。规则与 src-ui/shared/keys.js 的 modChar 相同。
  var fromKeybar = false;
  function modChar(ch, m) {
    if (ch === '\r' && m.shift && !m.ctrl && !m.alt) return '\n'; // ⇧ + 手机键盘的回车 = 换行（同 ⇧↵）
    var out = m.shift ? ch.toUpperCase() : ch;
    if (m.ctrl) {
      var c = out.toUpperCase().charCodeAt(0);
      if (out.length === 1 && c >= 64 && c <= 95) out = String.fromCharCode(c - 64);
      else if (out === ' ' || out === '2') out = '\x00';
      else if (out >= '3' && out <= '7') out = String.fromCharCode(out.charCodeAt(0) - 24);
      else if (out === '/') out = '\x1f';
      else if (out === '?' || out === '8') out = '\x7f';
    }
    return m.alt ? '\x1b' + out : out;
  }
  // Windows 上的会话保持程序 psmux 开着“鼠标移动也上报”（1003），浏览器里鼠标一动就发一串 ESC[<35;列;行M。
  // 这串字节经 ConPTY 交给 psmux 时偶尔被拆开（网络卡顿后积压的一批一起到），ESC 被当成单独的 Esc 键，
  // 剩下的 [<35;45;31M 作为文字打进 Codex 等程序的输入框。Windows 上不发没按键的鼠标移动：
  // 滚轮、点击、拖选（按着键移动）照常上报，只是程序里的悬停效果没有了。
  var HOST_WIN = window.__llHostWindows === true;
  var MOTION = /^0\x1b\[<(\d+);\d+;\d+M$/;
  function bareMotion(data) {
    if (!HOST_WIN || !(data instanceof Uint8Array) || data[0] !== 0x30 || data.length > 24 || data[1] !== 0x1b) return false;
    var m = MOTION.exec(new TextDecoder().decode(data));
    return Boolean(m && (m[1] & 35) === 35);
  }

  // ttyd 发送输入的格式：'0' + UTF-8 文本。只改单个字符（输入法一次上屏多个字时原样发送）。
  function withMods(data) {
    var box;
    try { box = !fromKeybar && window.frameElement && window.frameElement.looklookMods; } catch (e) { box = null; }
    var m = box && box.get();
    if (!m || !(m.ctrl || m.alt || m.shift) || !(data instanceof Uint8Array) || data[0] !== 0x30) return data;
    var text = new TextDecoder().decode(data.subarray(1));
    if (Array.from(text).length !== 1) return data;
    box.set({ ctrl: m.ctrl === 'lock' ? 'lock' : null, alt: m.alt === 'lock' ? 'lock' : null, shift: m.shift === 'lock' ? 'lock' : null });
    return new TextEncoder().encode('0' + modChar(text, m));
  }

  function send(term, data) {
    fromKeybar = true;
    try {
      if (typeof term.input === 'function') term.input(data, true);
      else if (term._core && term._core.coreService) term._core.coreService.triggerDataEvent(data, true);
    } finally {
      fromKeybar = false;
    }
  }

  function refit(term) {
    try {
      var f = term.options.fontFamily;
      term.options.fontFamily = f + ' ';
      term.options.fontFamily = f;
      if (typeof term.fit === 'function') term.fit();
      term.refresh(0, term.rows - 1);
    } catch (e) {
      /* 终端还没准备好，忽略 */
    }
  }

  // tmux 的窗口尺寸跟着最近有输入（或改过尺寸）的那一处走（window-size latest）。电脑和手机同时开着同一个终端时，
  // 回到手机上看到的还是按电脑屏幕排的画面（宽度、高度都不对，底部的输入框不见了、留下零星的残字），要打一个字才变回来。
  // 尺寸没变时 ttyd 不会再报，这里直接按 ttyd 的格式（'1' + JSON）报一次少一行、再报回原样，tmux 就按这一处重新排版。
  // 已经是这一处的尺寸时只是高度来回变一行，程序（AI 编程助手等）按同样的尺寸重画，看不出变化。
  // lastLocal：这一处最近一次发出输入（或要回尺寸）的时间。刚打过字的话 tmux 已经按这一处排版了。
  var claimAt = 0, claimTimer = 0, lastLocal = 0;
  function claimSize(term, idle) {
    var now = Date.now();
    if (idle && now - lastLocal < idle) return;
    if (now - claimAt < 1500 || document.visibilityState !== 'visible') return;
    if (!socket || socket.readyState !== 1 || !(term.cols > 1) || !(term.rows > 2)) return;
    claimAt = lastLocal = now;
    var enc = new TextEncoder();
    function report(rows) {
      try { if (socket && socket.readyState === 1) socket.send(enc.encode('1' + JSON.stringify({ columns: term.cols, rows: rows }))); } catch (e) { /* 忽略 */ }
    }
    report(term.rows - 1);
    clearTimeout(claimTimer);
    // 隔一会儿再报回来：两次挨得太近时 tmux 只看到最后的尺寸（没变），不会切换
    claimTimer = setTimeout(function () { report(term.rows); }, 80);
  }

  // 切到别的窗口（或手机切到别的应用）再回来：浏览器在后台时会暂停终端的绘制，窗口尺寸也可能在这期间变过，
  // 回来后屏幕只画了一部分（例如 AI 编程助手底部的输入框不见了），要刷新或切到别的终端再切回来才恢复。
  // 回到前台 / 窗口重新拿到焦点时重新测量、适配并整屏重画；布局稳定需要一点时间，过一会儿再来一次。
  function refitOnReturn(term) {
    var timers = [];
    function later() {
      timers.forEach(clearTimeout);
      timers = [60, 400].map(function (ms) { return setTimeout(function () { refit(term); if (ms === 400) claimSize(term); }, ms); });
    }
    document.addEventListener('visibilitychange', function () { if (document.visibilityState === 'visible') later(); });
    window.addEventListener('pageshow', later);
    window.addEventListener('focus', later);
    window.addEventListener('resize', function () { setTimeout(function () { try { term.refresh(0, term.rows - 1); } catch (e) { /* 忽略 */ } }, 120); });
    // 按到终端上（准备打字、滑动翻看）：先把窗口尺寸要回来
    document.addEventListener('pointerdown', function () { claimSize(term, 10000); }, true);
  }

  function decodeBase64Utf8(b64) {
    var bin = atob(b64);
    var bytes = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
    return new TextDecoder().decode(bytes);
  }

  function toast(text) {
    var el = document.createElement('div');
    el.textContent = text;
    el.style.cssText =
      'position:fixed;left:50%;bottom:24px;transform:translateX(-50%);z-index:99;padding:6px 14px;border-radius:999px;' +
      'background:rgba(20,22,30,.86);color:#fff;font:13px/1.6 system-ui,sans-serif;pointer-events:none;transition:opacity .3s';
    document.body.appendChild(el);
    setTimeout(function () { el.style.opacity = '0'; }, 900);
    setTimeout(function () { el.remove(); }, 1300);
  }

  // 手机键盘：外层挂在 iframe 元素上的 looklookIme = { change(focused), up() → 键盘是否弹着 }。
  var touch = Boolean(window.matchMedia && window.matchMedia('(hover: none) and (pointer: coarse)').matches);
  var lastGesture = 0, longPressAt = 0;
  function imeBox() {
    try { return window.frameElement && window.frameElement.looklookIme; } catch (e) { return null; }
  }
  function setupIme(term) {
    var ta = term.textarea;
    if (!ta) return;
    // show：外层按钮的点击里同步调用（算用户操作）。已聚焦但键盘被收起（如安卓返回键）时先失焦再聚焦，键盘才会出来。
    window.looklookIme = function (show) {
      if (!show) { ta.blur(); return; }
      lastGesture = Date.now();
      if (document.activeElement === ta) ta.blur();
      ta.focus({ preventScroll: true });
    };
    ta.addEventListener('focus', function () {
      // 不是用户点出来的聚焦（ttyd 加载/重连时自己聚焦）不让它弹出键盘。
      if (touch && Date.now() - lastGesture > 1500) { ta.blur(); return; }
      var box = imeBox();
      if (box) box.change(true);
    });
    ta.addEventListener('blur', function () {
      var box = imeBox();
      if (box) box.change(false);
    });
    if (!touch) return;
    if (document.activeElement === ta) ta.blur();
    // 点一下终端（不是滑动、不是长按）：已聚焦但键盘没弹着时把键盘叫出来；没聚焦时 xterm 自己会聚焦。
    var tap = null;
    document.addEventListener('pointerdown', function (e) {
      lastGesture = Date.now();
      tap = e.pointerType === 'mouse' ? null : { x: e.clientX, y: e.clientY, t: lastGesture };
    }, true);
    document.addEventListener('pointerup', function (e) {
      var d = tap;
      tap = null;
      if (!d || Math.abs(e.clientX - d.x) + Math.abs(e.clientY - d.y) > 12 || Date.now() - d.t > 600 || Date.now() - longPressAt < 1000) return;
      if (!e.target || !e.target.closest || !e.target.closest('.xterm')) return;
      var box = imeBox();
      if (document.activeElement === ta && box && !box.up()) window.looklookIme(true);
    }, true);
  }

  // 手机上滑动翻看：xterm.js 的触摸滑动只滚动它自己的回滚区。终端跑在会话保持程序（tmux，备用屏幕、开了鼠标）里时
  // 回滚区是空的，滑动什么也不做——AI 编程助手和普通命令行都一样。这里把竖向滑动换成滚轮事件交给 xterm，
  // 和电脑上用鼠标滚轮完全一样：开了鼠标上报时变成滚轮上报（tmux 进入翻看模式，滑回底部自动退出），
  // 否则滚动 xterm 的回滚区，或在备用屏幕里变成上下方向键（xterm 的默认行为）。松手后带惯性继续滚一会儿。
  function setupTouchScroll(term) {
    var el = term.element;
    if (!touch || !el) return;
    var screen = el.querySelector('.xterm-screen') || el;
    // 不让浏览器自己处理竖向拖动（拖动整个页面）；双指缩放照常。
    el.style.touchAction = 'pinch-zoom';
    var start = null, scrolling = false, lastY = 0, lastT = 0, vel = 0, acc = 0, x = 0, y = 0, fling = 0, hold = 0;
    var moved = 0, scrolledAt = 0;
    // 手指在浏览器的“点按容差”范围内移动时，Chrome 不发 touchmove，整个手势算作一次点按（随后补发 mousedown / click，
    // xterm 就聚焦输入框、弹出键盘）。pointermove 照常发，而且在 touchend 之前到：用它判断手指到底动没动。
    var pdown = null, pmoved = 0;
    el.addEventListener('pointerdown', function (e) {
      if (e.pointerType !== 'touch' || !e.isPrimary) return;
      pdown = { x: e.clientX, y: e.clientY };
      pmoved = 0;
    }, { capture: true, passive: true });
    el.addEventListener('pointermove', function (e) {
      if (!pdown || e.pointerType !== 'touch' || !e.isPrimary) return;
      pmoved = Math.max(pmoved, Math.abs(e.clientX - pdown.x) + Math.abs(e.clientY - pdown.y));
      if (pmoved > 8) cancelHold();
    }, { capture: true, passive: true });
    // 长按（不动 0.5 秒）：嵌在管理台里时打开“选择文字”。xterm.js 在手机上选不了字。
    // 滑动中途按住不动（让惯性停下来、或停一下再接着滑）不算长按，否则一滑就弹出“选择文字”。
    // 满 0.5 秒时只把手指下的那一行亮起来（不振动），松手才打开；亮起后又滑动了就是在翻看，取消，
    // 免得“按住看一会儿再滑”被当成长按。打开时带上按住的那一行和上下几行的文字，管理台据此定位（TextSelect.jsx）。
    var armed = null, cue = null;
    function cancelHold() {
      clearTimeout(hold); hold = 0;
      armed = null;
      if (cue) cue.style.opacity = '0';
    }
    function pressedAt(py) {
      var b = term.buffer && term.buffer.active;
      if (!b) return null;
      var r = screen.getBoundingClientRect();
      var row = Math.max(0, Math.min(term.rows - 1, Math.floor((py - r.top) / rowHeight())));
      var lines = [], from = Math.max(0, row - 3), to = Math.min(term.rows - 1, row + 3);
      for (var i = from; i <= to; i++) {
        var l = b.getLine(b.viewportY + i);
        // tmux 翻看模式右上角的位置提示 [12/300] 不是输出
        lines.push(l ? l.translateToString(true).replace(/\s*\[\d+\/\d+\]\s*$/, '') : '');
      }
      return { row: row - from, lines: lines, top: r.top + row * rowHeight(), left: r.left, width: r.width };
    }
    function showCue(at) {
      if (!cue) {
        cue = document.createElement('div');
        cue.style.cssText = 'position:fixed;z-index:5;pointer-events:none;border-radius:3px;opacity:0;transition:opacity .15s';
        document.body.appendChild(cue);
      }
      var th = term.options && term.options.theme;
      cue.style.background = (th && (th.selectionBackground || th.cursor)) || '#3b3f63';
      cue.style.top = at.top + 'px';
      cue.style.left = at.left + 'px';
      cue.style.width = at.width + 'px';
      cue.style.height = rowHeight() + 'px';
      cue.style.opacity = '0.7';
    }
    function rowHeight() { return Math.max(8, screen.clientHeight / (term.rows || 24)); }
    // 开了鼠标上报时每个滚轮事件只上报一次（tmux 一次滚 5 行），攒够约 2.5 行的滑动距离再发一次，滚动速度跟手。
    function mouseMode() { return Boolean(term.modes && term.modes.mouseTrackingMode && term.modes.mouseTrackingMode !== 'none'); }
    function wheel(delta) {
      screen.dispatchEvent(new WheelEvent('wheel', { deltaY: delta, deltaMode: 0, clientX: x, clientY: y, bubbles: true, cancelable: true }));
    }
    // 开了鼠标上报时滚动在电脑那头（tmux）做，每一下都要经网络往返，tmux 每收到一下还要整屏重画一次发回来。
    // 一口气发出去的话，快速滑动会排起几十屏重画，经中转慢慢传回来，越滑越落后，松手后还在滚。
    // 所以同一时间只让一批在路上：等终端收到输出（上一批的重画）再把这期间攒下的一次性发出去，
    // 同一时刻发出的几下 tmux 一次读到，只重画一屏。延迟就是一次往返，不会越积越多。
    var inflight = false, ackTimer = 0;
    function pump() {
      if (inflight) return;
      var step = rowHeight() * 2.5;
      var n = Math.trunc(acc / step);
      if (!n) return;
      acc -= n * step;
      inflight = true;
      for (var i = 0; i < Math.abs(n); i++) wheel(n > 0 ? step : -step);
      ackTimer = setTimeout(ack, 400); // 到顶 / 到底时 tmux 不输出，不能一直等
    }
    function ack() {
      if (!inflight) return;
      inflight = false;
      clearTimeout(ackTimer);
      pump();
    }
    var onOutput = term.onWriteParsed || term.onRender;
    if (typeof onOutput === 'function') onOutput.call(term, ack);
    function scrollBy(d) {
      scrolledAt = Date.now();
      acc += d;
      if (mouseMode()) { pump(); return; }
      // 本地回滚区：立即滚动
      if (acc) { wheel(acc); acc = 0; }
    }
    el.addEventListener('touchstart', function (e) {
      cancelAnimationFrame(fling);
      acc = 0; // 手指按下就停：还没发出去的不再发
      if (e.touches.length !== 1) { start = null; return; }
      var t = e.touches[0];
      var wasScrolling = fling || Date.now() - scrolledAt < 400; // 按下时还在滚（惯性）或刚滚过
      fling = 0;
      start = { x: t.clientX, y: t.clientY };
      scrolling = false;
      moved = 0;
      cancelHold();
      if (window.parent !== window && !wasScrolling) {
        hold = setTimeout(function () {
          hold = 0;
          longPressAt = Date.now(); // 松手时不算点按（不弹键盘）
          armed = pressedAt(start ? start.y : y) || { row: 0, lines: [] };
          if (armed.lines.length) showCue(armed);
        }, 500);
      }
    }, { capture: true, passive: true });
    el.addEventListener('touchmove', function (e) {
      if (!start || e.touches.length !== 1) { start = null; cancelHold(); return; }
      var t = e.touches[0];
      moved = Math.max(moved, Math.abs(t.clientX - start.x) + Math.abs(t.clientY - start.y));
      if (!scrolling) {
        var dx = t.clientX - start.x, dy = t.clientY - start.y;
        if (moved > 8) cancelHold();
        if (Math.abs(dy) < 10 || Math.abs(dy) < Math.abs(dx)) return;
        scrolling = true;
        lastY = t.clientY; lastT = e.timeStamp; vel = 0; acc = 0;
      }
      // 自己处理：不让 xterm 再按它的方式滚一遍，也不让页面跟着动
      e.preventDefault();
      e.stopPropagation();
      x = t.clientX; y = t.clientY;
      var d = lastY - t.clientY; // 手指往上 = 往下看新内容（滚轮向下）
      var dt = Math.max(1, e.timeStamp - lastT);
      vel = 0.8 * (d / dt) + 0.2 * vel;
      lastY = t.clientY; lastT = e.timeStamp;
      if (d) scrollBy(d);
    }, { capture: true, passive: false });
    function end(e) {
      var at = armed;
      cancelHold();
      if (!start) return;
      if (at) {
        // 长按松手：打开“选择文字”。不让浏览器再补发点击（tmux 里会变成一次鼠标点击）。
        start = null; scrolling = false; pdown = null;
        if (e.cancelable) e.preventDefault();
        e.stopPropagation();
        toParent({ type: 'looklook:select', at: { row: at.row, lines: at.lines } });
        return;
      }
      var was = scrolling;
      start = null; scrolling = false;
      // 手指动过（哪怕没够滑动的距离）就不算点击：不让浏览器再补发 mousedown / mouseup / click。
      // 否则一次短促的滑动会聚焦终端、弹出键盘；两下会被当成双击，tmux 选中一个词并复制（剪贴板被改掉，还弹出“已复制”）。
      var swiped = moved > 4 || pmoved > 10;
      pdown = null;
      if (swiped && e.cancelable) e.preventDefault();
      if (!was) return;
      e.stopPropagation();
      // 停顿后才松手不算甩动
      var v = e.timeStamp - lastT > 80 ? 0 : vel;
      if (Math.abs(v) < 0.3) return;
      var prev = performance.now();
      (function step(now) {
        var dt = Math.min(48, now - prev);
        prev = now;
        scrollBy(v * dt);
        v *= Math.pow(0.95, dt / 16);
        fling = Math.abs(v) > 0.05 ? requestAnimationFrame(step) : 0;
      })(prev + 16);
    }
    el.addEventListener('touchend', end, { capture: true, passive: false });
    // 手机上连点两三下（浏览器补发的鼠标事件）在 tmux 里是双击选词、三击选行并立即复制，手机上从来不是想要的：
    // 只把第一下交给终端（点按钮、定位光标照常），紧跟着的几下拦下来。选文字用长按打开的“选择文字”。
    var tapDown = 0, swallowUp = false;
    function fromTouch(e) {
      return Boolean(e.sourceCapabilities ? e.sourceCapabilities.firesTouchEvents : Date.now() - lastGesture < 1000);
    }
    document.addEventListener('mousedown', function (e) {
      if (!fromTouch(e)) return;
      var now = Date.now();
      var repeat = now - tapDown < 500;
      tapDown = now;
      if (!repeat && e.detail < 2) return;
      swallowUp = true;
      e.preventDefault();
      e.stopImmediatePropagation();
    }, true);
    ['mouseup', 'dblclick'].forEach(function (type) {
      document.addEventListener(type, function (e) {
        if (!fromTouch(e) || (type === 'mouseup' && !swallowUp)) return;
        if (type === 'mouseup') swallowUp = false;
        e.preventDefault();
        e.stopImmediatePropagation();
      }, true);
    });
    el.addEventListener('touchcancel', function () { cancelHold(); start = null; scrolling = false; }, { capture: true, passive: true });
  }

  // 快速复制。xterm.js 自己的选择（没开鼠标上报时直接拖选；tmux 等开了鼠标时按住 Shift 拖选）不会自动进剪贴板：
  // 松开鼠标后在旁边显示“复制”小按钮，点一下复制并提示。tmux 自己的拖选松手即复制（OSC 52，见 setup），不受影响。
  function copyText(text) {
    var lines = text.split('\n'); // 去掉末尾的空行（全选时有一大串）
    while (lines.length > 1 && !lines[lines.length - 1].trim()) lines.pop();
    text = lines.join('\n');
    function fallback() {
      var ta = document.createElement('textarea');
      ta.value = text;
      ta.style.cssText = 'position:fixed;top:-1000px;opacity:0';
      document.body.appendChild(ta);
      ta.select();
      var ok = false;
      try { ok = document.execCommand('copy'); } catch (e) { ok = false; }
      ta.remove();
      return ok;
    }
    var done = function (ok) { toast(tr(ok ? 'copied' : 'copy_failed')); };
    if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(text).then(function () { done(true); }, function () { done(fallback()); });
    else done(fallback());
  }
  // tmux 里拖选松手后靠 OSC 52 写剪贴板；Safari、Firefox 等要求“用户刚点过”才让写，这时给一个按钮，点一下再复制。
  var offer = null;
  function offerCopy(text) {
    if (offer) offer.remove();
    var b = offer = document.createElement('button');
    b.type = 'button';
    b.textContent = tr('copy_offer');
    b.style.cssText = 'position:fixed;left:50%;bottom:24px;transform:translateX(-50%);z-index:99;padding:7px 16px;border:1px solid #59627a;border-radius:999px;' +
      'background:#262a36;color:#e3e6ee;font:600 13px/1.4 system-ui,sans-serif;cursor:pointer;box-shadow:0 6px 18px rgba(0,0,0,.45)';
    b.addEventListener('mousedown', function (e) { e.preventDefault(); e.stopPropagation(); }, true);
    b.addEventListener('click', function () { copyText(text); b.remove(); if (offer === b) offer = null; });
    document.body.appendChild(b);
    setTimeout(function () { b.remove(); if (offer === b) offer = null; }, 8000);
  }

  // 终端缓冲区里的文字（回滚区 + 屏幕），折行接回原来的长行，去掉末尾的空行。
  function bufferText(term) {
    var b = term.buffer && term.buffer.active;
    if (!b) return '';
    var out = [];
    for (var i = 0; i < b.length; i++) {
      var l = b.getLine(i);
      if (!l) continue;
      var s = l.translateToString(true);
      if (l.isWrapped && out.length) out[out.length - 1] += s;
      else out.push(s);
    }
    while (out.length && !out[out.length - 1].trim()) out.pop();
    return out.join('\n');
  }

  // 选中文字后不再弹“复制”小按钮（不显眼、在 iframe 里点了也常常写不进剪贴板）：用 ⌘C / Ctrl+Shift+C（见 setupKeys）。
  function setupCopy(term) {
    if (typeof term.getSelection !== 'function') return;
    window.looklookCopySelection = function () {
      var text = term.getSelection && term.getSelection();
      if (!text) return false;
      copyText(text);
      return true;
    };
  }

  // 链接（xterm.js 的网址识别、OSC 8 超链接）：它们最后都调 window.open（网址识别是先开空白窗口再设地址，
  // OSC 8 先 confirm 一下）。⌘ / Ctrl + 点击直接打开；普通点击在鼠标旁边弹出“打开 / 复制链接”，免得点一下终端就跳走。
  function setupLinks() {
    var nativeOpen = window.open, nativeConfirm = window.confirm;
    var press = { t: 0, x: 0, y: 0, mod: false };
    document.addEventListener('mouseup', function (e) {
      press = { t: Date.now(), x: e.clientX, y: e.clientY, mod: e.metaKey || e.ctrlKey };
    }, true);
    var pop = null;
    function hidePop() { if (pop) { pop.remove(); pop = null; } }
    document.addEventListener('mousedown', function (e) { if (pop && !pop.contains(e.target)) hidePop(); }, true);
    document.addEventListener('keydown', function (e) { if (pop && e.key === 'Escape') hidePop(); }, true);
    window.addEventListener('blur', hidePop);
    function openUrl(url) { nativeOpen.call(window, url, '_blank', 'noopener'); }
    function offer(url) {
      hidePop();
      var p = pop = mount(document.createElement('div'));
      p.style.cssText = 'position:fixed;z-index:99;display:flex;align-items:center;gap:6px;max-width:min(460px,calc(100% - 16px));padding:6px 6px 6px 10px;' +
        'border:1px solid #59627a;border-radius:10px;background:#262a36;color:#e3e6ee;font:13px/1.4 system-ui,sans-serif;box-shadow:0 6px 18px rgba(0,0,0,.45)';
      var u = document.createElement('span');
      u.textContent = url;
      u.title = tr('link_hint');
      u.style.cssText = 'flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;color:#9fb0ff';
      p.appendChild(u);
      [['link_open', function () { openUrl(url); }], ['link_copy', function () { copyText(url); }]].forEach(function (a, i) {
        var b = document.createElement('button');
        b.type = 'button';
        b.textContent = tr(a[0]);
        b.style.cssText = 'flex:none;border:0;border-radius:7px;padding:4px 10px;cursor:pointer;font:600 12.5px/1.4 system-ui,sans-serif;' +
          (i === 0 ? 'background:#8b93ff;color:#11131a' : 'background:rgba(255,255,255,.12);color:#fff');
        b.addEventListener('mousedown', function (e) { e.preventDefault(); e.stopPropagation(); }, true);
        b.addEventListener('click', function () { a[1](); hidePop(); });
        p.appendChild(b);
      });
      var w = p.offsetWidth, h = p.offsetHeight;
      p.style.left = Math.max(8, Math.min(press.x - 12, window.innerWidth - w - 8)) + 'px';
      p.style.top = (press.y + 14 + h < window.innerHeight ? press.y + 14 : Math.max(8, press.y - h - 14)) + 'px';
    }
    // 是不是刚才那一下点击打开的链接（触屏上照常打开，没有悬停和修饰键）
    function fromClick() { return Date.now() - press.t < 800 && !touch; }
    function handle(url) {
      if (!/^(https?|ftp|mailto):/i.test(url)) return false;
      if (press.mod) openUrl(url);
      else offer(url);
      return true;
    }
    window.open = function (url) {
      if (!fromClick()) return nativeOpen.apply(window, arguments);
      if (url === undefined || url === null || url === '' || url === 'about:blank') {
        // 网址识别：先 window.open() 拿空白窗口，再 opener = null、location.href = 网址
        var fake = { opener: null, closed: false, close: function () {}, focus: function () {}, location: {} };
        Object.defineProperty(fake.location, 'href', { set: function (u) { if (!handle(String(u))) openUrl(String(u)); }, get: function () { return ''; } });
        return fake;
      }
      if (handle(String(url))) return null;
      return nativeOpen.apply(window, arguments);
    };
    // OSC 8 链接：xterm 先 confirm“确定要打开…”，我们的气泡就是确认
    window.confirm = function (msg) {
      if (fromClick() && /navigate to|https?:\/\//i.test(String(msg || ''))) return true;
      return nativeConfirm.apply(window, arguments);
    };
  }

  // 手机上：终端连不上、正在连接时，从顶部往下拉刷新整个页面（让管理台刷新）；终端正常时下拉是翻看终端，不处理。
  function setupPullRefresh() {
    if (!touch) return;
    var start = null, ind = null, dist = 0;
    var LIMIT = 72;
    function render() {
      if (!ind) {
        ind = mount(document.createElement('div'));
        ind.style.cssText = 'position:fixed;left:50%;top:6px;z-index:100;transform:translateX(-50%);padding:5px 14px;border-radius:999px;' +
          'background:rgba(24,26,36,.94);color:#e6e8ef;font:13px/1.5 system-ui,sans-serif;border:1px solid rgba(255,255,255,.12);pointer-events:none';
      }
      ind.textContent = (dist >= LIMIT ? '↻ ' : '↓ ') + tr(dist >= LIMIT ? 'release' : 'pull');
      ind.style.opacity = String(Math.min(1, dist / LIMIT));
      ind.style.marginTop = Math.min(dist, LIMIT) / 2 + 'px';
    }
    function reset() { start = null; dist = 0; if (ind) { ind.remove(); ind = null; } }
    document.addEventListener('touchstart', function (e) {
      if (connOk || e.touches.length !== 1) return reset();
      start = e.touches[0].clientY;
    }, { capture: true, passive: true });
    document.addEventListener('touchmove', function (e) {
      if (start === null) return;
      dist = Math.max(0, e.touches[0].clientY - start);
      if (dist > 8) render();
    }, { capture: true, passive: true });
    document.addEventListener('touchend', function () {
      var go = start !== null && dist >= LIMIT;
      reset();
      if (!go) return;
      if (window.parent !== window) {
        try { window.parent.postMessage({ type: 'looklook:pull-refresh' }, PARENT); return; } catch (e) { /* 忽略 */ }
      }
      location.reload();
    }, { capture: true, passive: true });
    document.addEventListener('touchcancel', reset, { capture: true, passive: true });
  }
  setupPullRefresh();

  // 在捕获阶段拦下几个键，不替换 ttyd 自己的按键处理。输入法正在组字时不管。
  // - ⇧↵ 换行：xterm.js 把 Shift+Enter 也当回车（\r）发出去，AI 编程助手就直接提交了。
  //   改发 Ctrl+J（\n）：Claude Code、Codex、OpenCode 都把它当输入框里的换行，普通 shell 里与回车一样。
  // - Ctrl+V（Windows / Linux 的粘贴键）：xterm.js 除了粘贴文字，还会把 ^V 发给程序，Claude Code 收到 ^V
  //   就去读那台电脑的剪贴板找图片，提示“No image found in clipboard”。这里只让浏览器粘贴，不发 ^V；
  //   图片由管理台上传后填路径。macOS 上粘贴是 ⌘V，^V 照常发送（vim 的列选择等）。
  // - 中文输入法下的标点（Shift+9 → “（”、逗号 → “，”…）：有的输入法 / WebView 报的按键不是 229 而是普通的 “(” 键，
  //   xterm.js 就自己发出英文 “(” 并取消事件，输入法没机会换成全角。数字和标点键改由浏览器正常输入，
  //   在随后的 input 事件里把实际上屏的字（输入法给的全角或原样的半角）发给终端；字母、空格仍走 xterm 的处理。
  var MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || '');
  function setupKeys(term) {
    var punct = null; // 放行给浏览器的标点键：{ key, timer }
    function sendText(text) {
      if (typeof term.input === 'function') term.input(text, true);
      else if (term._core && term._core.coreService) term._core.coreService.triggerDataEvent(text, true);
    }
    document.addEventListener('keypress', function (e) {
      if (punct && e.target === term.textarea) e.stopImmediatePropagation(); // 不让 xterm 在 keypress 里发英文、取消输入
    }, true);
    document.addEventListener('input', function (e) {
      if (!punct || e.target !== term.textarea) return;
      clearTimeout(punct.timer);
      punct = null;
      if (e.inputType !== 'insertText' || !e.data || e.isComposing) return;
      e.stopImmediatePropagation();
      sendText(e.data);
    }, true);
    document.addEventListener('keydown', function (e) {
      if (!term.textarea || e.target !== term.textarea || e.isComposing || e.keyCode === 229) return;
      if (e.key && e.key.length === 1 && !/[A-Za-z ]/.test(e.key) && !e.ctrlKey && !e.altKey && !e.metaKey) {
        e.stopImmediatePropagation(); // 不 preventDefault：输入法照常处理这个键
        if (punct) clearTimeout(punct.timer);
        var key = e.key;
        // 浏览器没有触发 input（不该发生）时按原样补发，免得丢字。
        punct = { key: key, timer: setTimeout(function () { if (punct && punct.key === key) { punct = null; sendText(key); } }, 50) };
        return;
      }
      if (e.key === 'Enter' && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
        e.preventDefault();
        e.stopImmediatePropagation();
        send(term, '\n');
      } else if (!MAC && e.ctrlKey && e.shiftKey && !e.altKey && !e.metaKey && (e.key === 'c' || e.key === 'C') && window.looklookCopySelection) {
        // Ctrl+Shift+C：有选中的文字就复制（同常见终端）；没有时照常交给程序
        if (window.looklookCopySelection()) {
          e.preventDefault();
          e.stopImmediatePropagation();
        }
      } else if (!MAC && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && (e.key === 'v' || e.key === 'V')) {
        e.stopImmediatePropagation(); // 不 preventDefault：浏览器照常触发粘贴
      }
    }, true);
  }

  // 管理台在终端还没准备好时（iframe 刚加载完）就要求聚焦：先记下，准备好后再聚焦（消息不会因为来得太早丢掉）
  var wantFocus = false;
  function earlyFocus(e) {
    if (fromParent(e) && e.data && e.data.type === 'looklook:focus') wantFocus = true;
  }
  window.addEventListener('message', earlyFocus);
  // 电脑离线 / 在线：终端还没准备好时也要记下（准备好时 setup 按它决定能不能输入）
  window.addEventListener('message', function (e) {
    if (fromParent(e) && e.data.type === 'looklook:offline') setOffline(Boolean(e.data.data));
    if (fromParent(e) && e.data.type === 'looklook:theme') setTheme(e.data.data);
  });

  function setup(term) {
    window.removeEventListener('message', earlyFocus);
    TERM = term;
    readOnly();
    if (wantTheme) setTheme(wantTheme);
    // 要求过聚焦，或者页面已经有焦点：交给终端（手机上不主动弹键盘）
    if (wantFocus || (!touch && document.hasFocus())) term.focus();
    refitOnReturn(term);
    setupIme(term);
    setupTouchScroll(term);
    setupKeys(term);
    setupCopy(term);
    setupLinks();
    // opts.focus === false：按键条发送时不聚焦终端，免得手机键盘被弹出来。
    window.looklookSend = function (data, opts) { send(term, data); if (!opts || opts.focus !== false) term.focus(); };
    // 管理台上传完成后填路径：term.paste 会在程序开启 bracketed paste 时自动包上 ESC[200~ … ESC[201~。
    window.looklookText = function () { return bufferText(term); };
    window.looklookPaste = function (text, opts) {
      fromKeybar = true; // 不套用按键条的修饰键
      try { term.paste(text); } finally { fromKeybar = false; }
      if (!opts || opts.focus !== false) term.focus();
    };

    // OSC 52：`ESC ] 52 ; c ; <base64> BEL`，只处理写入，不允许程序读取剪贴板。
    if (term.parser && term.parser.registerOscHandler) {
      term.parser.registerOscHandler(52, function (data) {
        var i = data.indexOf(';');
        var payload = i >= 0 ? data.slice(i + 1) : data;
        if (!payload || payload === '?') return true;
        try {
          var text = decodeBase64Utf8(payload);
          if (navigator.clipboard && navigator.clipboard.writeText) {
            navigator.clipboard.writeText(text).then(function () { toast(tr('copied')); }, function () { offerCopy(text); });
          } else {
            offerCopy(text);
          }
        } catch (e) {
          /* 内容不合法，忽略 */
        }
        return true;
      });
    }

    var size = term.options.fontSize || 15;
    var loads = [400, 700].map(function (w) { return document.fonts.load(w + ' ' + size + 'px "JetBrains Mono"'); });
    Promise.all(loads).then(function () { refit(term); }, function () {});
    // 中文字体按需分片下载，完成后再适配一次。
    document.fonts.addEventListener && document.fonts.addEventListener('loadingdone', function () { refit(term); });
    // 父窗口（管理台）尺寸变化时同步。
    // 跨源（本机直连）时外层不能直接调用 looklookSend / looklookPaste，改发消息。
    window.addEventListener('message', function (e) {
      if (!fromParent(e)) return;
      var d = e.data, opts = d.opts && typeof d.opts === 'object' ? d.opts : undefined;
      if (d.type === 'looklook:send' && typeof d.data === 'string') window.looklookSend(d.data, opts);
      if (d.type === 'looklook:paste' && typeof d.data === 'string') window.looklookPaste(d.data, opts);
      if (d.type === 'looklook:focus') term.focus();
      if (d.type === 'looklook:fit') { refit(term); claimSize(term); }
      if (d.type === 'looklook:text') toParent({ type: 'looklook:text', text: bufferText(term) });
    });
    // 告诉管理台终端准备好了：当前显示的终端由它再聚焦一次
    toParent({ type: 'looklook:ready' });
  }

  (function wait() {
    if (window.term && window.term.options) return setup(window.term);
    if (tries++ < 300) setTimeout(wait, 100);
  })();
})();
