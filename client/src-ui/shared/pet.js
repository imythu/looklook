// 对话界面角落的小胖鱼（看看自己画的像素画，ChatPet.jsx 显示）：按形状一格一格算出来，不放图片。
// 不依赖 React 和浏览器，`node --test` 直接测。

/** 画布大小（格） */
export const PET_W = 20;
export const PET_H = 14;

/**
 * 对话状态 → 小鱼在干什么：
 * - work：在跑（摆尾巴、冒泡泡）；
 * - wait：等你确认、回答（举起侧鳍，头上一个感叹号）；
 * - done：刚做完（翻个跟头，眯眼笑），`doneAt` 之后 4 秒内；
 * - sleep：闲着超过 1 分钟（闭眼、冒 z）；
 * - idle：其他时候（慢慢摆）。
 * 没连上（detached）时不显示：返回 null。
 */
export function petMood(phase, { doneAt = 0, idleSince = 0, now = Date.now() } = {}) {
  if (!phase || phase === 'detached') return null;
  if (phase === 'running') return 'work';
  if (phase === 'approval' || phase === 'tui') return 'wait';
  if (doneAt && now - doneAt < 4000) return 'done';
  if (idleSince && now - idleSince > 60_000) return 'sleep';
  return 'idle';
}

/** 每种状态有几帧、多久换一帧（毫秒） */
export const PET_FRAMES = { work: [4, 180], wait: [2, 400], done: [4, 160], sleep: [2, 900], idle: [4, 500] };

/**
 * 一帧的像素：`[[x, y, 颜色]]`，颜色是 o 描边、b 身体、l 肚子、f 鳍和尾巴、w 眼白、p 眼珠、c 腮红。
 * 鱼头朝左，尾巴在右边；`frame` 决定尾巴摆到哪、鳍的位置。
 */
export function petPixels(mood, frame = 0) {
  const wag = mood === 'sleep' ? 0 : [0, 1, 0, -1][frame % 4];
  const cell = (x, y) => {
    const cx = x + 0.5;
    const cy = y + 0.5;
    // 圆滚滚的身体
    const body = ((cx - 7.5) / 6.6) ** 2 + ((cy - 7.5) / 5.6) ** 2 <= 1;
    if (body) {
      if (mood === 'done' || mood === 'sleep' ? eyeClosed(x, y, mood) : eyeOpen(x, y)) return eyeColor(x, y, mood);
      if ((x === 5 || x === 6) && y === 9) return 'c';
      // 嘴：刚做完时张开
      if (x === 1 && (y === 8 || (mood === 'done' && y === 9))) return 'o';
      if (finSide(x, y, frame, mood)) return 'f';
      return cy > 9.2 ? 'l' : 'b';
    }
    // 尾巴：从身体右边张开的三角，末端分叉，跟着 wag 上下摆
    const dy = Math.abs(cy - 7.5 - wag * (cx - 13) * 0.25);
    if (cx >= 13.5 && cx <= 18.5 && dy <= (cx - 13) * 0.7 && !(cx > 16.5 && dy < (cx - 16.5) * 0.9)) return 'f';
    // 背鳍
    if (y === 1 && x >= 7 && x <= 9) return 'f';
    if (y === 0 && x === 8) return 'f';
    return null;
  };
  const grid = [];
  for (let y = 0; y < PET_H; y++) {
    const row = [];
    for (let x = 0; x < PET_W; x++) row.push(cell(x, y));
    grid.push(row);
  }
  // 描边：空格子挨着有颜色的格子（上下左右）
  const out = [];
  for (let y = 0; y < PET_H; y++) {
    for (let x = 0; x < PET_W; x++) {
      const c = grid[y][x];
      if (c) out.push([x, y, c]);
      else if ([[1, 0], [-1, 0], [0, 1], [0, -1]].some(([dx, dy]) => grid[y + dy]?.[x + dx] && grid[y + dy][x + dx] !== 'o')) out.push([x, y, 'o']);
    }
  }
  return out;
}

// 睁着的眼：2×2 白眼，左下一格眼珠（看着左边的输入）
const eyeOpen = (x, y) => (x === 3 || x === 4) && (y === 5 || y === 6);
const eyeColor = (x, y, mood) => (mood === 'done' || mood === 'sleep' ? 'o' : x === 3 && y === 6 ? 'p' : 'w');
// 闭着的眼：睡觉是一条横线，笑的时候是 ^
const eyeClosed = (x, y, mood) => (mood === 'sleep' ? y === 6 && (x === 3 || x === 4) : (y === 5 && (x === 3 || x === 4)) || (y === 6 && (x === 2 || x === 5)));
// 贴在身上的侧鳍：干活时上下扇；等你时举起来（像举手），一晃一晃
const finSide = (x, y, frame, mood) => {
  if (mood === 'wait') {
    const dx = frame % 2;
    return (y === 7 && x >= 8 && x <= 9) || (y === 6 && x === 9 + dx) || (y === 5 && x === 9 + dx) || (y === 4 && x === 10 + dx);
  }
  const up = mood === 'work' && frame % 2 === 1 ? -1 : 0;
  return (y === 8 + up && x >= 8 && x <= 9) || (y === 9 + up && x >= 9 && x <= 10);
};
