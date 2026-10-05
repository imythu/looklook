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
    'zh-CN': { copied: '已复制', connecting: '正在连接终端…', lost: '连接断开了，正在自动重连…', back: '已重新连接', reload: '立即重连' },
    'en-US': { copied: 'Copied', connecting: 'Connecting to the terminal…', lost: 'Connection lost. Reconnecting automatically…', back: 'Reconnected', reload: 'Reconnect now' }
  };
  function tr(key) {
    var lang = 'zh-CN';
    try { lang = (CROSS && HASH['ll-lang']) || localStorage.getItem('ll_locale') || (navigator.language || '').indexOf('en') === 0 && 'en-US' || 'zh-CN'; } catch (e) { /* 忽略 */ }
    return (TEXT[lang] && TEXT[lang][key]) || TEXT['zh-CN'][key];
  }

  // 连接状态提示：首次连接时盖一层“正在连接”，断线后顶部给一条“正在重连”并带按钮，重连成功短暂提示。
  // 在 ttyd 创建 WebSocket 之前包一层，只观察 /ws 连接的开闭，不改变它的行为。
  var overlay = null, banner = null, everOpen = false;
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
        var nativeSend = ws.send;
        ws.send = function (data) { return nativeSend.call(ws, withMods(data)); };
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
    } catch (e) {
      /* 终端还没准备好，忽略 */
    }
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
  var lastGesture = 0;
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
      if (!d || Math.abs(e.clientX - d.x) + Math.abs(e.clientY - d.y) > 12 || Date.now() - d.t > 600) return;
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
    var start = null, scrolling = false, lastY = 0, lastT = 0, vel = 0, acc = 0, x = 0, y = 0, fling = 0;
    function rowHeight() { return Math.max(8, screen.clientHeight / (term.rows || 24)); }
    // 开了鼠标上报时每个滚轮事件只上报一次（tmux 一次滚 5 行），攒够约 2.5 行的滑动距离再发一次，滚动速度跟手。
    function mouseMode() { return Boolean(term.modes && term.modes.mouseTrackingMode && term.modes.mouseTrackingMode !== 'none'); }
    function scrollBy(d) {
      acc += d;
      var step = mouseMode() ? rowHeight() * 2.5 : 1;
      while (Math.abs(acc) >= step) {
        var delta = acc > 0 ? step : -step;
        if (step === 1) delta = acc;
        acc -= delta;
        screen.dispatchEvent(new WheelEvent('wheel', { deltaY: delta, deltaMode: 0, clientX: x, clientY: y, bubbles: true, cancelable: true }));
      }
    }
    el.addEventListener('touchstart', function (e) {
      cancelAnimationFrame(fling);
      if (e.touches.length !== 1) { start = null; return; }
      var t = e.touches[0];
      start = { x: t.clientX, y: t.clientY };
      scrolling = false;
    }, { capture: true, passive: true });
    el.addEventListener('touchmove', function (e) {
      if (!start || e.touches.length !== 1) { start = null; return; }
      var t = e.touches[0];
      if (!scrolling) {
        var dx = t.clientX - start.x, dy = t.clientY - start.y;
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
      if (!start) return;
      var was = scrolling;
      start = null; scrolling = false;
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
        if (Math.abs(v) > 0.05) fling = requestAnimationFrame(step);
      })(prev + 16);
    }
    el.addEventListener('touchend', end, { capture: true, passive: true });
    el.addEventListener('touchcancel', function () { start = null; scrolling = false; }, { capture: true, passive: true });
  }

  // 在捕获阶段拦下几个键，不替换 ttyd 自己的按键处理。输入法正在组字时不管。
  // - ⇧↵ 换行：xterm.js 把 Shift+Enter 也当回车（\r）发出去，AI 编程助手就直接提交了。
  //   改发 Ctrl+J（\n）：Claude Code、Codex、OpenCode 都把它当输入框里的换行，普通 shell 里与回车一样。
  // - Ctrl+V（Windows / Linux 的粘贴键）：xterm.js 除了粘贴文字，还会把 ^V 发给程序，Claude Code 收到 ^V
  //   就去读那台电脑的剪贴板找图片，提示“No image found in clipboard”。这里只让浏览器粘贴，不发 ^V；
  //   图片由管理台上传后填路径。macOS 上粘贴是 ⌘V，^V 照常发送（vim 的列选择等）。
  var MAC = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent || '');
  function setupKeys(term) {
    document.addEventListener('keydown', function (e) {
      if (!term.textarea || e.target !== term.textarea || e.isComposing || e.keyCode === 229) return;
      if (e.key === 'Enter' && e.shiftKey && !e.ctrlKey && !e.altKey && !e.metaKey) {
        e.preventDefault();
        e.stopImmediatePropagation();
        send(term, '\n');
      } else if (!MAC && e.ctrlKey && !e.shiftKey && !e.altKey && !e.metaKey && (e.key === 'v' || e.key === 'V')) {
        e.stopImmediatePropagation(); // 不 preventDefault：浏览器照常触发粘贴
      }
    }, true);
  }

  function setup(term) {
    setupIme(term);
    setupTouchScroll(term);
    setupKeys(term);
    // opts.focus === false：按键条发送时不聚焦终端，免得手机键盘被弹出来。
    window.looklookSend = function (data, opts) { send(term, data); if (!opts || opts.focus !== false) term.focus(); };
    // 管理台上传完成后填路径：term.paste 会在程序开启 bracketed paste 时自动包上 ESC[200~ … ESC[201~。
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
            navigator.clipboard.writeText(text).then(function () { toast(tr('copied')); }, function () {});
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
      if (d.type === 'looklook:fit') refit(term);
    });
  }

  (function wait() {
    if (window.term && window.term.options) return setup(window.term);
    if (tries++ < 300) setTimeout(wait, 100);
  })();
})();
