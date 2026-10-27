// 快捷键的共用小工具：Mac 上用 ⌘，其他系统用 Ctrl（docs/UI_REDESIGN_2026-10.md §5）。
export const IS_MAC = typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/** 按下的是不是 ⌘（Mac）/ Ctrl（其他）+ 这个键，没有别的修饰键（`shift` 为 true 时要带 Shift）。 */
export function isMod(e, key, { shift = false } = {}) {
  const mod = IS_MAC ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  return mod && !e.altKey && e.shiftKey === shift && e.key.toLowerCase() === key;
}

/** 写给人看的组合键：`mod+K` → Mac 上 `⌘K`，其他 `Ctrl+K`。 */
export function keyLabel(combo) {
  return combo
    .split('+')
    .map((k) => (k === 'mod' ? (IS_MAC ? '⌘' : 'Ctrl') : k === 'shift' ? (IS_MAC ? '⇧' : 'Shift') : k === 'alt' ? (IS_MAC ? '⌥' : 'Alt') : k))
    .join(IS_MAC ? '' : '+');
}
