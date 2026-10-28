// 输入法正在组字时的按键（中文输入法里打英文、按回车上屏）：这时的回车不是发送 / 确认。
// Safari 和 macOS 桌面版的 WKWebView 上，回车上屏时 compositionend 先到，随后那个 keydown 的 isComposing 已经是 false，
// 只能靠 keyCode 229 认出来；有的版本连 229 都没有，就看是不是紧跟在 compositionend 后面。

let endedAt = -1e9;
if (typeof document !== 'undefined') {
  document.addEventListener('compositionend', () => (endedAt = performance.now()), true);
}

/** React 合成事件或原生 KeyboardEvent 都行 */
export function imeComposing(e) {
  const n = e.nativeEvent ?? e;
  return Boolean(n.isComposing) || n.keyCode === 229 || (n.key === 'Enter' && performance.now() - endedAt < 50);
}
