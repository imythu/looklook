// “选择文字”打开时定位到手机上长按的那一行（term-inject.js 发来 looklook:select 的 at）。
// at = { row, lines }：屏幕上按住的那一行（lines[row]）和上下各几行的文字。面板里的文字是 tmux 的
// capture-pane -J（折行已接回长行）或终端缓冲区，与屏幕不是一行对一行，所以按内容找：
// 屏幕上的一行一定是面板里某一行的一部分；先找包含锚点行的候选，再看上下几行能否在附近对上，得分最高的胜出，
// 一样高时取靠后的（离最新输出近，屏幕上看到的多半是它）。

/** 面板文字 `text` 里与 `at` 对应的行号（从 0 开始）；找不到返回 -1。 */
export function locateLine(text, at) {
  const rows = (at?.lines ?? []).map((l) => String(l ?? '').trimEnd());
  const k = Number(at?.row);
  if (!text || !rows.length || !Number.isInteger(k) || k < 0 || k >= rows.length) return -1;
  const lines = text.split('\n');
  // 锚点：按住的那一行；它是空行时用最近的非空行
  let anchor = -1;
  for (let d = 0; d < rows.length && anchor < 0; d++) {
    for (const j of [k - d, k + d]) if (j >= 0 && j < rows.length && rows[j].trim()) { anchor = j; break; }
  }
  if (anchor < 0) return -1;
  const weight = (s) => Math.min(1, s.trim().length / 8); // 很短的行（“>”、“$”）到处都有，算得少
  // 一行对上的程度：整行相同 1，是其中一部分（折行的一段、或碰巧包含）0.6；位置正对的满分，错开一两行（折行）打折
  const match = (line, s) => (line === undefined ? 0 : line.trimEnd() === s ? 1 : line.includes(s) ? 0.6 : 0);
  const near = (idx, s) => {
    let m = match(lines[idx], s);
    for (const d of [1, 2]) m = Math.max(m, 0.5 * match(lines[idx - d], s), 0.5 * match(lines[idx + d], s));
    return m;
  };
  let best = -1, bestScore = -1;
  for (let c = 0; c < lines.length; c++) {
    const own = match(lines[c], rows[anchor]);
    if (!own) continue;
    let score = own * weight(rows[anchor]);
    rows.forEach((s, j) => {
      if (j !== anchor && s.trim()) score += near(c + j - anchor, s) * weight(s);
    });
    if (score >= bestScore) {
      best = c;
      bestScore = score;
    }
  }
  if (best < 0) return -1;
  return Math.max(0, Math.min(lines.length - 1, best + k - anchor));
}
