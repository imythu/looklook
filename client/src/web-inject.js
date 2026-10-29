// 注入到会话标签里打开的网页界面（DSH，/w/{id}/）首页的脚本。
// 远程管理台走 WebRTC 时（服务端 docs/06）：普通请求由启动页的 Service Worker 经 DataChannel 转发，WebSocket 不经过它，
// 这里把页面的 WebSocket 换成经启动页（window.top.__llrtc）打开的替身。其他情况什么也不做。
// 替身与 term-inject.js 里的 RtcSocket 相同（终端那份还要管重连提示，这里只要接口一致）。
(function () {
  'use strict';
  var bridge;
  try {
    var b = window.top !== window && window.top.__llrtc;
    bridge = b && b.proto === 1 && typeof b.openWs === 'function' ? b : null;
  } catch (e) { bridge = null; }
  if (!bridge) return;

  function RtcSocket(url, protocols) {
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
  window.WebSocket = RtcSocket;
})();
