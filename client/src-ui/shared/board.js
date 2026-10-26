// 任务看板（docs/TASK_BOARD.md §5）：会话在看板上属于哪一组、那一句话怎么说，以及“正在看”的心跳。
// 状态都由电脑算好（`GET /instances` 的 `chat`），这里只负责分组和文字；旧版客户端没有 `chat.board` 时按 phase 推。
import { useEffect } from 'react';

import { api } from './api.js';

/** 看板上的组：attn 等你处理 / working 正在干活 / ended 刚结束（没看过）/ idle 空闲 / stopped 未运行 */
export function boardState(i) {
  if (!i?.running) return 'stopped';
  const b = i.chat?.board;
  if (b) return b === 'stopped' ? 'idle' : b;
  if (i.chat?.pending || i.chat?.phase === 'tui') return 'attn';
  if (i.chat?.phase === 'running') return 'working';
  return 'idle';
}

/** 看板组的顺序 */
export const BOARD_GROUPS = ['attn', 'working', 'ended'];

/** 组里的顺序：等你处理按等得久的在前，其余新的在前 */
export function boardSort(group) {
  const at = (i) => (group === 'ended' ? i.chat?.turn?.ended_at : i.chat?.since) ?? 0;
  return group === 'attn' ? (a, b) => at(a) - at(b) : (a, b) => at(b) - at(a);
}

// 工具 → 动词（board.do_*）和颜色种类（同对话界面的 --k-*）
const TOOL_KIND = {
  Bash: 'run',
  Read: 'read',
  Edit: 'edit',
  Write: 'edit',
  MultiEdit: 'edit',
  NotebookEdit: 'edit',
  apply_patch: 'edit',
  Grep: 'search',
  Glob: 'search',
  WebFetch: 'web',
  WebSearch: 'web',
  Task: 'agent',
  Agent: 'agent',
  mcp: 'mcp',
};
export const toolKind = (tool) => TOOL_KIND[tool] ?? 'other';

/**
 * 那一句话：`{ text, kind, code }`。kind 是颜色种类（工具）或 `error`；code 为 true 时等宽显示（命令）。
 * `activityText(activity)`：运行中的状态行换成中文（对话界面的 spinnerText / codexActivity）。
 */
export function boardSummary(t, i, activityText) {
  const c = i.chat ?? {};
  const state = boardState(i);
  if (state === 'attn') {
    const a = c.attn ?? {};
    const d = a.detail ?? '';
    switch (a.kind) {
      case 'permission':
        return a.tool === 'Bash' ? { text: d, kind: 'run', code: true } : { text: t('board.wants_tool', { tool: a.tool, detail: d }), kind: toolKind(a.tool) };
      case 'question':
        return { text: t('board.asks', { q: d }) };
      case 'plan':
        return { text: t('board.plan', { p: d }) };
      case 'limit':
        return { text: t('board.limit') };
      default:
        return { text: d ? t('board.dialog', { title: d }) : t('board.dialog_unknown') };
    }
  }
  if (state === 'working') {
    if (c.limit?.waiting) return { text: t('board.limit_waiting', { at: c.limit.waiting }) };
    const tool = c.doing?.tool;
    const detail = c.doing?.detail;
    if (tool && detail) return { text: t(`board.do_${toolKind(tool)}`, { detail, tool }), kind: toolKind(tool) };
    const act = activityText?.(c.activity, tool);
    return { text: act || t('board.working') };
  }
  if (state === 'ended') {
    const turn = c.turn ?? {};
    if (turn.outcome === 'failed') return { text: t('board.failed', { error: turn.error ?? '' }), kind: 'error' };
    return { text: turn.summary || t('board.done') };
  }
  return { text: '' };
}

/** 现在的时长：等你处理等了多久、正在干活跑了多久、刚结束的这一轮用了多久（毫秒） */
export function boardSpan(i, now = Date.now()) {
  const c = i.chat ?? {};
  const state = boardState(i);
  if (state === 'attn') return c.since ? now - c.since : null;
  if (state === 'working') return c.turn?.started_at ? now - c.turn.started_at : c.since ? now - c.since : null;
  if (state === 'ended' && c.turn?.ended_at && c.turn?.started_at) return c.turn.ended_at - c.turn.started_at;
  return null;
}

/**
 * 会话页可见、有焦点时每 20 秒告诉电脑“有人正在看”：这段时间结束的一轮直接算看过，以后也不发通知。
 * 旧版客户端没有这个接口：出错就不再报。
 */
export function useSeenPing(id, on) {
  useEffect(() => {
    if (!on || !id) return undefined;
    let dead = false;
    const ping = () => {
      if (dead || document.hidden || !document.hasFocus()) return;
      api.post(`/instances/${id}/chat/seen`, { active: true }).catch((e) => {
        if (e?.status === 404 || e?.status === 405) dead = true;
      });
    };
    ping();
    const timer = setInterval(ping, 20000);
    const onFocus = () => ping();
    window.addEventListener('focus', onFocus);
    document.addEventListener('visibilitychange', onFocus);
    return () => {
      clearInterval(timer);
      window.removeEventListener('focus', onFocus);
      document.removeEventListener('visibilitychange', onFocus);
    };
  }, [id, on]);
}
