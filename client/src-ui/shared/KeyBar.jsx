// 手机按键条：常驻两行（Esc、Tab、Ctrl、Alt、Shift、方向键、^C、粘贴），顶部的小把手可以把它收成一行（向下滑或点一下），
// “更多”展开分类面板：本机系统的常用键排第一（Linux / macOS / Windows 常用），然后 AI 助手、另外两个系统、符号、翻页、F1–F12；
// 分类之间可以有重复的键，方便在自己习惯的分类里一次找全。
// Ctrl / Alt / Shift 点一下只作用于下一个键（包括手机自带键盘打的字），再点一下锁定，第三下取消。
// 只在触屏设备上出现（电脑有实体键盘，不显示按键条也不显示开关）。按键不会弹出/收起手机键盘。
import { ChevronDown, ClipboardPaste, Ellipsis } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { anyMod, COMPACT_ROW, GROUPS, hostProfile, KEYS, MAIN_ROWS, MODS, nextMod, NO_MODS, orderedGroups, releaseOnce } from './keys.js';
import { useToast } from './ui.jsx';

const PREF = 'll_keybar';
const MOD_LABEL = { ctrl: 'Ctrl', alt: 'Alt', shift: '⇧' };
const MOD_NAME = { ctrl: 'Ctrl', alt: 'Alt', shift: 'Shift' };
const ARIA = { up: 'up', down: 'down', left: 'left', right: 'right', bksp: 'bksp', enter: 'enter' };

function loadPref() {
  try {
    const p = JSON.parse(localStorage.getItem(PREF) || '{}');
    // tab 为空时按本机系统选；旧版的 combo 分类已经拆成各系统的常用键
    return { shown: p.shown !== false, open: Boolean(p.open), compact: Boolean(p.compact), tab: GROUPS.some((g) => g.id === p.tab) ? p.tab : null };
  } catch {
    return { shown: true, open: false, compact: false, tab: null };
  }
}

function savePref(p) {
  try {
    localStorage.setItem(PREF, JSON.stringify({ ...loadPref(), ...p }));
  } catch {
    /* 隐私模式等场景忽略 */
  }
}

/** 触屏为主的设备（手机、平板）。带触摸屏的笔记本主指针是鼠标/触控板，不算。 */
export const isTouch = () => Boolean(window.matchMedia?.('(hover: none) and (pointer: coarse)').matches);

/** 按键条显示/隐藏（终端页顶部的键盘按钮），记住用户的选择。 */
export function useKeybarShown() {
  const [shown, setShown] = useState(() => loadPref().shown);
  return [
    shown,
    (v) => {
      setShown(v);
      savePref({ shown: v });
    },
  ];
}

/**
 * 手机键盘弹出时，iOS Safari 与新版 Android Chrome 只缩小“可见区域”，fixed 元素会被键盘盖住。
 * 返回一个 style，让整页终端始终贴合可见区域，按键条留在键盘上方。
 */
export function useVisibleViewport(enabled) {
  const [vp, setVp] = useState(null);
  useEffect(() => {
    const vv = window.visualViewport;
    if (!enabled || !vv) return undefined;
    const on = () => setVp({ top: vv.offsetTop, height: vv.height });
    on();
    vv.addEventListener('resize', on);
    vv.addEventListener('scroll', on);
    return () => {
      vv.removeEventListener('resize', on);
      vv.removeEventListener('scroll', on);
      setVp(null);
    };
  }, [enabled]);
  return vp ? { top: vp.top, height: vp.height, bottom: 'auto' } : undefined;
}

function buzz() {
  try {
    navigator.vibrate?.(8);
  } catch {
    /* 不支持振动 */
  }
}

/** frame：终端 iframe 的 ref（同源 /i/{id}/，由 term-inject.js 提供 looklookSend）；os：终端所在电脑的系统。 */
export default function KeyBar({ frame, os }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [mods, setMods] = useState(NO_MODS);
  const [pref, setPref] = useState(loadPref);
  const modsRef = useRef(mods);
  const timer = useRef(null);
  const swipe = useRef(null);
  modsRef.current = mods;

  // 让终端页读取/消耗修饰键（手机键盘输入时用）。挂在 iframe 元素上，终端重新加载后依然有效。
  useEffect(() => {
    const el = frame.current;
    if (el) el.looklookMods = { get: () => modsRef.current, set: setMods };
  });
  useEffect(() => () => {
    if (frame.current) frame.current.looklookMods = null;
    clearTimeout(timer.current);
  }, []);

  const update = (p) => {
    setPref({ ...pref, ...p });
    savePref(p);
  };

  const fire = (key) => {
    const w = frame.current?.contentWindow;
    if (!w?.looklookSend) return;
    const m = modsRef.current;
    // 不抢焦点：手机键盘开着就保持开着，收起的就不弹出来（看 TUI 时用方向键翻页很常见）。
    w.looklookSend(key.seq(key.raw ? NO_MODS : m, Boolean(w.term?.modes?.applicationCursorKeysMode)), { focus: false });
    if (!key.raw && anyMod(m)) {
      modsRef.current = releaseOnce(m);
      setMods(modsRef.current);
    }
  };
  const stop = () => clearTimeout(timer.current);
  // 可连发的键在按下时就发送，按住 0.4 秒后连发；其他键在点击（抬起）时发送，避免滑动时误触。
  const handlers = (key) => ({
    onPointerDown: (e) => {
      e.preventDefault();
      if (!key.repeat) return;
      buzz();
      fire(key);
      stop();
      const loop = () => {
        fire(key);
        timer.current = setTimeout(loop, 60);
      };
      timer.current = setTimeout(loop, 400);
    },
    onPointerUp: stop,
    onPointerLeave: stop,
    onPointerCancel: stop,
    onContextMenu: (e) => e.preventDefault(),
    onClick: (e) => {
      // 键盘操作（Enter / 空格）时 detail 为 0，连发键也要在这里发一次。
      if (key.repeat && e.detail !== 0) return;
      buzz();
      fire(key);
    },
  });

  const paste = async () => {
    try {
      const text = await navigator.clipboard.readText();
      if (text) frame.current?.contentWindow?.looklookSend?.(text, { focus: false });
    } catch {
      toast(t('term.paste_denied'), 'error');
    }
  };

  // compact：常驻两行里不显示说明文字（太窄），只放在 title 里。
  const keyButton = (key, compact) => (
    <button
      key={key.label}
      type="button"
      className={`key ${key.hint && !compact ? 'key-hint' : ''} ${[...key.label].length === 1 ? 'key-glyph' : ''}`}
      aria-label={ARIA[key.id] ? t(`keybar.${ARIA[key.id]}`) : undefined}
      title={key.hint ? t(`keybar.hint.${key.hint}`) : undefined}
      {...handlers(key)}
    >
      <span>{key.label}</span>
      {key.hint && !compact && <small>{t(`keybar.hint.${key.hint}`)}</small>}
    </button>
  );

  const cell = (id) => {
    if (MODS.includes(id)) {
      const v = mods[id];
      return (
        <button
          key={id}
          type="button"
          className={`key key-mod ${id === 'shift' ? 'key-glyph' : ''}`}
          data-state={v ?? 'off'}
          aria-pressed={Boolean(v)}
          aria-label={v === 'lock' ? t('keybar.locked', { key: MOD_NAME[id] }) : MOD_NAME[id]}
          onPointerDown={(e) => e.preventDefault()}
          onClick={() => {
            buzz();
            setMods({ ...modsRef.current, [id]: nextMod(v) });
          }}
        >
          {MOD_LABEL[id]}
        </button>
      );
    }
    if (id === 'more') {
      return (
        <button key={id} type="button" className="key" aria-expanded={pref.open} aria-label={t(pref.open ? 'keybar.less' : 'keybar.more')} title={t(pref.open ? 'keybar.less' : 'keybar.more')} onPointerDown={(e) => e.preventDefault()} onClick={() => update({ open: !pref.open })}>
          {pref.open ? <ChevronDown size={18} aria-hidden="true" /> : <Ellipsis size={18} aria-hidden="true" />}
        </button>
      );
    }
    if (id === 'paste') {
      return (
        <button key={id} type="button" className="key" aria-label={t('term.paste')} title={t('term.paste')} onPointerDown={(e) => e.preventDefault()} onClick={paste}>
          <ClipboardPaste size={18} aria-hidden="true" />
        </button>
      );
    }
    return keyButton(KEYS[id], true);
  };

  const active = MODS.filter((k) => mods[k]);
  const allLocked = active.length > 0 && active.every((k) => mods[k] === 'lock');
  const host = hostProfile(os);
  const groups = orderedGroups(os);
  const group = groups.find((g) => g.id === pref.tab) ?? groups[0];

  // 把手：点一下切换收起/展开；向下滑收起，向上滑展开。
  const handle = {
    onPointerDown: (e) => {
      e.preventDefault();
      swipe.current = e.clientY;
    },
    onPointerUp: (e) => {
      const start = swipe.current;
      swipe.current = null;
      if (start == null) return;
      const dy = e.clientY - start;
      buzz();
      if (dy > 16) update({ compact: true });
      else if (dy < -16) update({ compact: false });
      else update({ compact: !pref.compact });
    },
    onPointerCancel: () => {
      swipe.current = null;
    },
    onKeyDown: (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        update({ compact: !pref.compact });
      }
    },
  };

  return (
    <div className={`keybar ${pref.compact ? 'keybar-compact' : ''}`} role="toolbar" aria-label={t('term.keys')}>
      <button type="button" className="keybar-handle" aria-expanded={!pref.compact} aria-label={t(pref.compact ? 'keybar.expand' : 'keybar.collapse')} title={t(pref.compact ? 'keybar.expand' : 'keybar.collapse')} {...handle}>
        <span aria-hidden="true" />
      </button>
      {pref.open && (
        <div className="keybar-panel">
          <div className="keybar-tabs" role="tablist" aria-label={t('keybar.categories')}>
            {groups.map((g) => (
              <button key={g.id} type="button" role="tab" aria-selected={g.id === group.id} onPointerDown={(e) => e.preventDefault()} onClick={() => update({ tab: g.id })}>
                {t(`keybar.tab.${g.id}`)}
                {g.id === host && <span className="keybar-here">{t('keybar.this_computer')}</span>}
              </button>
            ))}
          </div>
          <div className={`keybar-grid keybar-${group.os ? 'combo' : group.id === 'ai' ? 'combo' : group.id}`} role="tabpanel">
            {group.keys.map((k) => keyButton(k))}
          </div>
        </div>
      )}
      {active.length > 0 && (
        <p className="keybar-mods" aria-live="polite">
          {allLocked ? t('keybar.mods_locked', { keys: active.map((k) => MOD_NAME[k]).join(' + ') }) : t('keybar.mods_once', { keys: active.map((k) => MOD_NAME[k]).join(' + ') })}
        </p>
      )}
      {/* 竖屏两行 × 7（方向键成倒 T）；横屏合成一行 × 14，给终端留出高度；收起时只留一行 × 8 */}
      {pref.compact ? <div className="keybar-grid keybar-row">{COMPACT_ROW.map(cell)}</div> : <div className="keybar-grid keybar-main">{MAIN_ROWS.flat().map(cell)}</div>}
    </div>
  );
}
