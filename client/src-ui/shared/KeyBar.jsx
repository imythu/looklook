// 手机按键条：常驻两行（Esc、Tab、Ctrl、Alt、Shift、方向键、^C、粘贴、键盘开关、回车），顶部的小把手分三级：
// 一行 ⇄ 两行 ⇄ 分类面板（向上滑展开一级，向下滑收起一级，点一下在一行/两行间切换）。
// “更多”展开分类面板：本机系统的常用键排第一（Linux / macOS / Windows 常用），然后 AI 助手、另外两个系统、符号、翻页、F1–F12；
// 分类之间可以有重复的键，方便在自己习惯的分类里一次找全。
// Ctrl / Alt / Shift 点一下只作用于下一个键（包括手机自带键盘打的字），再点一下锁定，第三下取消。
// 只在触屏设备上出现（电脑有实体键盘，不显示按键条也不显示开关）。
//
// 手机键盘（输入法）什么时候弹出/收起：
// - 进入终端、刷新、断线重连：不弹出（先看输出；键盘会挡住半屏）。点终端才弹出，滑动翻看不弹出。
// - 普通按键、回车、粘贴、隐藏按键条、全屏：保持原样。
// - ⌨ 键：开着就收起，关着就弹出。Ctrl / Alt 第一次点亮时弹出（接下来多半要打字母）。
// - 打开“更多”面板：收起键盘，面板占它的位置；关上面板时，之前开着就重新弹出。面板开着时点终端打字，面板自动关上。
// - 键盘开着且屏幕放不下：按键条自动收成一行（用户动了把手就听用户的），极矮时（横屏）再藏起顶栏；键盘收起后恢复。
import { ChevronDown, ClipboardPaste, CornerDownLeft, Ellipsis, Keyboard, KeyboardOff } from 'lucide-react';
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
    // tab 为空时按本机系统选；旧版的 combo 分类已经拆成各系统的常用键。面板开关不记住：每次进来都是收着的。
    return { shown: p.shown !== false, compact: Boolean(p.compact), tab: GROUPS.some((g) => g.id === p.tab) ? p.tab : null };
  } catch {
    return { shown: true, compact: false, tab: null };
  }
}

function savePref(p) {
  try {
    const { open, ...rest } = { ...loadPref(), ...p };
    localStorage.setItem(PREF, JSON.stringify(rest));
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

// 键盘开着时，可见高度低于这些值就省地方：按键条收成一行 / 藏起顶栏。
const TIGHT_H = 480;
const CRAMPED_H = 380;

/**
 * 手机上的整页终端：
 * - 手机键盘弹出时，iOS Safari 与新版 Android Chrome 只缩小“可见区域”，fixed 元素会被键盘盖住。
 *   style 让整页终端始终贴合可见区域，按键条留在键盘上方。
 * - ime：终端里的输入框是否聚焦（focused）、键盘是否真的弹着（up，可见高度比没键盘时矮一截），以及 show() / hide()。
 *   终端页（term-inject.js）通过 iframe 元素上的 looklookIme 报告聚焦变化。
 * - tight / cramped：键盘弹着且屏幕放不下，按键条该收成一行 / 顶栏该藏起来。
 */
export function useTouchTerminal(frame, enabled) {
  const [vp, setVp] = useState(null);
  const [focused, setFocused] = useState(false);
  const base = useRef({ w: 0, h: 0 });
  const upRef = useRef(false);
  const el = useRef(null);
  useEffect(() => {
    const vv = window.visualViewport;
    if (!enabled || !vv) return undefined;
    const on = () => {
      // 没键盘时的高度：同一方向下见过的最大值（布局视口不随键盘变化时直接用它）。
      const w = window.innerWidth;
      if (w !== base.current.w) base.current = { w, h: 0 };
      base.current.h = Math.max(base.current.h, vv.height, document.documentElement.clientHeight);
      setVp({ top: vv.offsetTop, height: vv.height, full: base.current.h });
    };
    on();
    vv.addEventListener('resize', on);
    vv.addEventListener('scroll', on);
    return () => {
      vv.removeEventListener('resize', on);
      vv.removeEventListener('scroll', on);
      setVp(null);
    };
  }, [enabled]);

  const up = Boolean(enabled && focused && vp && vp.height < vp.full - 120);
  upRef.current = up;
  // 挂在 iframe 元素上，终端重新加载后依然有效；换了 iframe（刷新）时旧的聚焦状态作废。
  useEffect(() => {
    const cur = frame.current;
    if (cur !== el.current) {
      el.current = cur;
      setFocused(false);
    }
    if (cur) cur.looklookIme = enabled ? { change: (v) => setFocused(Boolean(v)), up: () => upRef.current } : null;
  });

  const call = (open) => frame.current?.contentWindow?.looklookIme?.(open);
  return {
    style: vp ? { top: vp.top, height: vp.height, bottom: 'auto' } : undefined,
    ime: { focused, up, show: () => call(true), hide: () => call(false) },
    tight: up && vp.height < TIGHT_H,
    cramped: up && vp.height < CRAMPED_H,
  };
}

/** 按住时不抢焦点（手机键盘开着就保持开着）。 */
export const keepFocus = (e) => e.preventDefault();

function buzz() {
  try {
    navigator.vibrate?.(8);
  } catch {
    /* 不支持振动 */
  }
}

/**
 * frame：终端 iframe 的 ref（同源 /i/{id}/，由 term-inject.js 提供 looklookSend / looklookIme）；os：终端所在电脑的系统；
 * ime、tight：来自 useTouchTerminal。
 */
export default function KeyBar({ frame, os, ime, tight }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [mods, setMods] = useState(NO_MODS);
  const newline = Boolean(mods.shift && !mods.ctrl && !mods.alt);
  const [pref, setPref] = useState(loadPref);
  const [open, setOpen] = useState(false);
  // 键盘弹着时自动收成一行；用户在这期间动了把手就不再自动收，键盘收起后重置。
  const [manual, setManual] = useState(false);
  const modsRef = useRef(mods);
  const timer = useRef(null);
  const swipe = useRef(null);
  const restoreIme = useRef(false);
  modsRef.current = mods;

  // 面板开着时用户点终端打字：面板让位给键盘。
  useEffect(() => {
    if (ime.focused && open) {
      setOpen(false);
      restoreIme.current = false;
    }
  }, [ime.focused]);
  useEffect(() => {
    if (!ime.up) setManual(false);
  }, [ime.up]);

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
  const auto = tight && !manual;
  const compact = pref.compact || auto;
  const setCompact = (v) => {
    if (ime.up) setManual(true);
    if (v !== pref.compact) update({ compact: v });
  };
  // 打开面板时收起键盘（面板占它的位置），关上时把之前开着的键盘还回去。
  const togglePanel = (v) => {
    if (v === open) return;
    if (v) {
      restoreIme.current = ime.up;
      if (ime.focused) ime.hide();
    } else if (restoreIme.current) {
      restoreIme.current = false;
      ime.show();
    }
    setOpen(v);
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
          onPointerDown={keepFocus}
          onClick={() => {
            buzz();
            const next = nextMod(v);
            setMods({ ...modsRef.current, [id]: next });
            // Ctrl / Alt 之后多半要打字母：键盘没开就弹出来（Shift 多配合按键条上的键，不弹）。
            if (id !== 'shift' && v === null && next && !ime.up && !open) ime.show();
          }}
        >
          {MOD_LABEL[id]}
        </button>
      );
    }
    if (id === 'more') {
      return (
        <button key={id} type="button" className="key" aria-expanded={open} aria-label={t(open ? 'keybar.less' : 'keybar.more')} title={t(open ? 'keybar.less' : 'keybar.more')} onPointerDown={keepFocus} onClick={() => togglePanel(!open)}>
          {open ? <ChevronDown className="key-icon" aria-hidden="true" /> : <Ellipsis className="key-icon" aria-hidden="true" />}
        </button>
      );
    }
    if (id === 'paste') {
      return (
        <button key={id} type="button" className="key" aria-label={t('term.paste')} title={t('term.paste')} onPointerDown={keepFocus} onClick={paste}>
          <ClipboardPaste className="key-icon" aria-hidden="true" />
        </button>
      );
    }
    if (id === 'ime') {
      const label = t(ime.up ? 'keybar.ime_hide' : 'keybar.ime_show');
      return (
        <button
          key={id}
          type="button"
          className="key"
          aria-pressed={ime.up}
          aria-label={label}
          title={label}
          onPointerDown={keepFocus}
          onClick={() => {
            buzz();
            if (ime.up) ime.hide();
            else {
              if (open) {
                restoreIme.current = false;
                setOpen(false);
              }
              ime.show();
            }
          }}
        >
          {ime.up ? <KeyboardOff className="key-icon" aria-hidden="true" /> : <Keyboard className="key-icon" aria-hidden="true" />}
        </button>
      );
    }
    if (id === 'enter') {
      return (
        // 按着 ⇧ 时回车键变成“换行”，顺便告诉用户 ⇧↵ 可以换行
        <button key={id} type="button" className={`key key-enter ${newline ? 'key-newline' : ''}`} aria-label={t(newline ? 'keybar.newline' : 'keybar.enter')} title={t(newline ? 'keybar.newline' : 'keybar.enter')} {...handlers(KEYS.enter)}>
          {newline ? <span>{t('keybar.newline')}</span> : <CornerDownLeft className="key-icon" aria-hidden="true" />}
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

  // 把手分三级：一行 ⇄ 两行 ⇄ 面板。向上滑展开一级，向下滑收起一级；点一下：面板开着就关上，否则在一行/两行间切换。
  const tap = () => (open ? togglePanel(false) : setCompact(!compact));
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
      if (dy > 16) {
        if (open) togglePanel(false);
        else setCompact(true);
      } else if (dy < -16) {
        if (compact) setCompact(false);
        else togglePanel(true);
      } else tap();
    },
    onPointerCancel: () => {
      swipe.current = null;
    },
    onKeyDown: (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        tap();
      }
    },
  };

  return (
    <div className={`keybar ${compact ? 'keybar-compact' : ''}`} role="toolbar" aria-label={t('term.keys')}>
      <button type="button" className="keybar-handle" aria-expanded={!compact} aria-label={t(compact ? 'keybar.expand' : 'keybar.collapse')} title={t(compact ? 'keybar.expand' : 'keybar.collapse')} {...handle}>
        <span aria-hidden="true" />
      </button>
      {open && (
        <div className="keybar-panel">
          <div className="keybar-tabs" role="tablist" aria-label={t('keybar.categories')}>
            {groups.map((g) => (
              <button key={g.id} type="button" role="tab" aria-selected={g.id === group.id} onPointerDown={keepFocus} onClick={() => update({ tab: g.id })}>
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
      {/* 竖屏两行 × 7（方向键成倒 T）；横屏合成一行 × 14，给终端留出高度；收起时只留一行 × 9 */}
      {compact ? <div className="keybar-grid keybar-row">{COMPACT_ROW.map(cell)}</div> : <div className="keybar-grid keybar-main">{MAIN_ROWS.flat().map(cell)}</div>}
    </div>
  );
}
