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
// 10. 快速复制：终端里选中文字后旁边出现“复制”小按钮；Windows / Linux 上 Ctrl+Shift+C 复制选中的文字。见 setupCopy。
// 11. 切到别的窗口再回来时重新适配、整屏重画，免得只画了一部分。见 refitOnReturn。
// 12. 选择文字：手机上长按终端时通知管理台打开“选择文字”（用浏览器自己的选择，见 src-ui/shared/TextSelect.jsx）；
//     提供 window.looklookText() 读出终端缓冲区的文字（没有 tmux 时用）。tmux 里拖选后浏览器不让直接写剪贴板时给一个“复制”按钮。
// 13. 同一个终端在几处打开（电脑和手机）：tmux 窗口按最近有输入的那一处的尺寸排版，另一处看到的是按别的屏幕排的画面。
//     页面回到前台、拿到焦点、手指/鼠标按到终端上时重新报一次尺寸，让 tmux 按这一处排版。见 claimSize。
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
    'zh-CN': { copy: '复制', copy_hint: '复制选中的文字', copy_failed: '浏览器不允许复制', copied: '已复制', copy_offer: '复制选中的文字', connecting: '正在连接终端…', lost: '连接断开了，正在自动重连…', back: '已重新连接', reload: '立即重连' },
    'en-US': { copy: 'Copy', copy_hint: 'Copy the selected text', copy_failed: 'The browser blocked copying', copied: 'Copied', copy_offer: 'Copy the selected text', connecting: 'Connecting to the terminal…', lost: 'Connection lost. Reconnecting automatically…', back: 'Reconnected', reload: 'Reconnect now' }
  };
  function tr(key) {
    var lang = 'zh-CN';
    try { lang = (CROSS && HASH['ll-lang']) || localStorage.getItem('ll_locale') || (navigator.language || '').indexOf('en') === 0 && 'en-US' || 'zh-CN'; } catch (e) { /* 忽略 */ }
    return (TEXT[lang] && TEXT[lang][key]) || TEXT['zh-CN'][key];
  }

  // 连接状态提示：首次连接时盖一层“正在连接”，断线后顶部给一条“正在重连”并带按钮，重连成功短暂提示。
  // 在 ttyd 创建 WebSocket 之前包一层，只观察 /ws 连接的开闭，不改变它的行为。
  var overlay = null, banner = null, everOpen = false, socket = null;
  var css = document.createElement('style');
  css.textContent =
    '@keyframes llb{0%,80%,100%{transform:scale(.6);opacity:.5}40%{transform:scale(1);opacity:1}}' +
    '.ll-dots{display:inline-flex;gap:5px;vertical-align:middle;margin-right:8px}' +
    '.ll-dots i{width:8px;height:8px;border-radius:50%;background:#8b93ff;animation:llb 1.2s infinite ease-in-out}' +
    '.ll-dots i:nth-child(2){animation-delay:.15s}.ll-dots i:nth-child(3){animation-delay:.3s}' +
    '@media(prefers-reduced-motion:reduce){.ll-dots i{animation:none}}';
  document.documentElement.appendChild(css);
  var DOTS = '<span class="ll-dots"><i></i><i></i><i></i></span>';
  function mount(el) { document.documentElement.appendChild(el); return el; }
  function showConnecting() {
    if (overlay || everOpen) return;
    overlay = mount(document.createElement('div'));
    overlay.style.cssText = 'position:fixed;inset:0;z-index:98;display:grid;place-items:center;background:#11131a;color:#c5cad6;font:15px/1.6 system-ui,sans-serif';
    overlay.innerHTML = '<div>' + DOTS + tr('connecting') + '</div>';
  }
  function hideConnecting() { if (overlay) { overlay.remove(); overlay = null; } }
  function showLost() {
    hideConnecting();
    if (banner) return;
    banner = mount(document.createElement('div'));
    banner.style.cssText = 'position:fixed;top:0;left:0;right:0;z-index:99;display:flex;justify-content:center;align-items:center;gap:12px;padding:8px 12px;background:rgba(180,83,9,.95);color:#fff;font:14px/1.5 system-ui,sans-serif';
    banner.innerHTML = '<span>' + DOTS + tr('lost') + '</span>';
    var b = document.createElement('button');
    b.textContent = tr('reload');
    b.style.cssText = 'border:0;border-radius:999px;padding:3px 12px;background:#fff;color:#92400e;font:inherit;cursor:pointer';
    b.onclick = function () { location.reload(); };
    banner.appendChild(b);
  }
  function onOpen() {
    var was = Boolean(banner);
    everOpen = true;
    hideConnecting();
    if (banner) { banner.remove(); banner = null; }
    if (was && document.body) toast(tr('back'));
  }
  (function watchSocket() {
    var Native = window.WebSocket;
    if (!Native) return;
    function Wrapped(url, protocols) {
      var ws = protocols === undefined ? new Native(url) : new Native(url, protocols);
      if (/\/ws(\?|$)/.test(String(url))) {
        if (!everOpen) showConnecting();
        ws.addEventListener('open', onOpen);
        ws.addEventListener('close', showLost);
        ws.addEventListener('message', function (e) { if (e.data instanceof ArrayBuffer) watchTransfer(e.data); });
        transferTail = '';
        socket = ws;
        var nativeSend = ws.send;
        ws.send = function (data) {
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
    // 长按（不动 0.5 秒）：嵌在管理台里时打开“选择文字”。xterm.js 在手机上选不了字。
    // 滑动中途按住不动（让惯性停下来、或停一下再接着滑）不算长按，否则一滑就弹出“选择文字”。
    function cancelHold() { clearTimeout(hold); hold = 0; }
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
          longPressAt = Date.now();
          try { navigator.vibrate && navigator.vibrate(10); } catch (err) { /* 不支持振动 */ }
          toParent({ type: 'looklook:select' });
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
      cancelHold();
      if (!start) return;
      var was = scrolling;
      start = null; scrolling = false;
      // 手指动过（哪怕没够滑动的距离）就不算点击：不让浏览器再补发 mousedown / mouseup / click。
      // 否则两下短促的滑动会被当成双击，tmux 选中一个词并复制（剪贴板被改掉，还弹出“已复制”）。
      if (moved > 4 && e.cancelable) e.preventDefault();
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

  function setupCopy(term) {
    if (typeof term.onSelectionChange !== 'function' || typeof term.getSelection !== 'function') return;
    var chip = null, at = null, timer = 0;
    function hide() { clearTimeout(timer); if (chip) { chip.remove(); chip = null; } }
    function show() {
      hide();
      var text = term.getSelection();
      if (!text || !text.trim() || !at) return;
      chip = document.createElement('button');
      chip.type = 'button';
      chip.textContent = tr('copy');
      chip.title = tr('copy_hint') + (MAC ? ' (⌘C)' : ' (Ctrl+Shift+C)');
      var x = Math.min(Math.max(8, at.x + 8), window.innerWidth - 72), y = Math.min(Math.max(8, at.y - 38), window.innerHeight - 40);
      chip.style.cssText = 'position:fixed;left:' + x + 'px;top:' + y + 'px;z-index:97;padding:5px 12px;border:1px solid #59627a;border-radius:9px;' +
        'background:#262a36;color:#e3e6ee;font:600 13px/1.4 system-ui,sans-serif;cursor:pointer;box-shadow:0 6px 18px rgba(0,0,0,.45)';
      // 按下时不让 xterm 拿到事件（否则选择被清掉）
      chip.addEventListener('mousedown', function (e) { e.preventDefault(); e.stopPropagation(); }, true);
      chip.addEventListener('click', function (e) {
        e.preventDefault();
        copyText(term.getSelection() || text);
        term.clearSelection();
        hide();
        term.focus();
      });
      document.body.appendChild(chip);
    }
    document.addEventListener('mouseup', function (e) {
      if (e.button !== 0 || (chip && chip.contains(e.target))) return;
      at = { x: e.clientX, y: e.clientY };
      clearTimeout(timer);
      timer = setTimeout(show, 0); // 等 xterm 处理完这次 mouseup（双击选词、三击选行）
    }, true);
    term.onSelectionChange(function () { if (!term.hasSelection || !term.hasSelection()) hide(); });
    document.addEventListener('keydown', function (e) { if (chip && !(e.ctrlKey && e.shiftKey)) hide(); }, true);
    window.addEventListener('blur', hide);
    window.looklookCopySelection = function () {
      var text = term.getSelection && term.getSelection();
      if (!text) return false;
      copyText(text);
      hide();
      return true;
    };
  }

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

  function setup(term) {
    window.removeEventListener('message', earlyFocus);
    // 要求过聚焦，或者页面已经有焦点：交给终端（手机上不主动弹键盘）
    if (wantFocus || (!touch && document.hasFocus())) term.focus();
    refitOnReturn(term);
    setupIme(term);
    setupTouchScroll(term);
    setupKeys(term);
    setupCopy(term);
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
