// 对话页里的复制、查找、目录（docs/UI_REDESIGN_2026-10.md §4、§5）：
// 手机上长按一条消息弹出的菜单和“选择文字”、用户消息的操作、Ctrl/⌘ F 查找、Alt+↑/↓ 在自己的消息间跳、右侧“目录”栏。
import { ArrowDown, ArrowUp, ClipboardCopy, Copy, Download, PencilLine, TextSelect, X } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import { useTranslation } from 'react-i18next';

import { chatToMarkdown, copyText, exportName } from '../shared/chatCopy.js';
import { isMod } from '../shared/hotkeys.js';
import { CopyButton, Modal, useToast } from '../shared/ui.jsx';
import { imeComposing } from '../shared/ime.js';

/** 把一句话填进这个对话的输入框（输入框听 `ll-chat-fill:{id}`） */
export const fillInput = (id, text) => window.dispatchEvent(new CustomEvent(`ll-chat-fill:${id}`, { detail: text }));

const touchOnly = () => window.matchMedia?.('(hover: none) and (pointer: coarse)').matches;

/**
 * 手机上按住 ~0.5 秒：onLong()。手指移动了（在滚动）就不算；电脑上不起作用（电脑上有悬停出现的按钮）。
 * 消息在手机上关掉了系统自己的长按选择（见 styles.css），选择文字改走“选择文字”面板，不和列表滚动抢手势。
 */
export function useLongPress(onLong) {
  const timer = useRef(0);
  const start = useRef(null);
  // 已经弹出菜单：松手时不再产生点击（不然点击落在刚弹出的遮罩上，菜单马上又关了）
  const fired = useRef(false);
  const cb = useRef(onLong);
  cb.current = onLong;
  const clear = () => {
    clearTimeout(timer.current);
    timer.current = 0;
  };
  useEffect(() => clear, []);
  return {
    onTouchStart: (e) => {
      clear();
      fired.current = false;
      if (e.touches.length !== 1 || !touchOnly() || e.target.closest?.('a, button, summary')) return;
      start.current = [e.touches[0].clientX, e.touches[0].clientY];
      timer.current = setTimeout(() => {
        timer.current = 0;
        fired.current = true;
        navigator.vibrate?.(10);
        cb.current();
      }, 480);
    },
    onTouchMove: (e) => {
      const p = e.touches[0];
      if (timer.current && start.current && Math.hypot(p.clientX - start.current[0], p.clientY - start.current[1]) > 10) clear();
    },
    onTouchEnd: (e) => {
      clear();
      if (fired.current && e.cancelable) e.preventDefault();
    },
    onTouchCancel: clear,
  };
}

/** 长按后从底部弹出：复制全文、选择文字、填回输入框（只有用户消息有） */
export function MessageSheet({ text, onFill, onClose }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [select, setSelect] = useState(false);
  useEffect(() => {
    const esc = (e) => e.key === 'Escape' && onClose();
    window.addEventListener('keydown', esc);
    return () => window.removeEventListener('keydown', esc);
  }, [onClose]);
  if (select) return <SelectText text={text} onClose={onClose} />;
  const copy = async () => {
    if (await copyText(text)) {
      toast(t('action.copied'));
      onClose();
    } else {
      setSelect(true);
    }
  };
  return createPortal(
    <div className="chat-sheet-back" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div className="chat-sheet" role="menu" aria-label={t('chat.msg_menu')}>
        <p className="chat-sheet-peek">{text}</p>
        <button type="button" role="menuitem" onClick={copy}>
          <Copy size={18} aria-hidden="true" />
          {t('chat.copy_all')}
        </button>
        <button type="button" role="menuitem" onClick={() => setSelect(true)}>
          <TextSelect size={18} aria-hidden="true" />
          {t('chat.select_text')}
        </button>
        {onFill && (
          <button
            type="button"
            role="menuitem"
            onClick={() => {
              onFill();
              onClose();
            }}
          >
            <PencilLine size={18} aria-hidden="true" />
            {t('chat.fill_back')}
          </button>
        )}
        <button type="button" className="chat-sheet-cancel" onClick={onClose}>
          {t('action.cancel')}
        </button>
      </div>
    </div>,
    document.body,
  );
}

/** “选择文字”：整条消息的纯文字放在一个全屏面板里，系统的选择手柄可以随便拖，也可以一键全选 */
export function SelectText({ text, onClose }) {
  const { t } = useTranslation();
  const box = useRef(null);
  const all = () => {
    const sel = window.getSelection();
    const r = document.createRange();
    r.selectNodeContents(box.current);
    sel.removeAllRanges();
    sel.addRange(r);
  };
  return (
    <Modal open onClose={onClose} title={t('chat.select_text')} className="chat-select">
      <div className="chat-select-text" ref={box}>
        {text}
      </div>
      <div className="modal-actions">
        <button type="button" className="btn btn-ghost" onClick={all}>
          {t('chat.select_all')}
        </button>
        <CopyButton text={text} variant="primary" />
        <button type="button" className="btn btn-ghost" onClick={onClose}>
          {t('action.close')}
        </button>
      </div>
    </Modal>
  );
}

/** 用户消息下面悬停出现的操作：复制、填回输入框（改一改再发） */
export function UserActs({ id, text }) {
  const { t } = useTranslation();
  return (
    <div className="chat-user-acts">
      <CopyButton text={text} label={false} variant="plain" />
      <button type="button" className="btn btn-plain btn-icon" onClick={() => fillInput(id, text)} aria-label={t('chat.fill_back')} title={t('chat.fill_back')}>
        <PencilLine size={15} />
      </button>
    </div>
  );
}

// ---------------- 查找 ----------------

const HL = 'll-find';
const HL_CUR = 'll-find-cur';
const canHighlight = () => typeof CSS !== 'undefined' && 'highlights' in CSS && typeof Highlight !== 'undefined';

/** root 里看得见的文字中所有的 q（不分大小写）：Range 列表。收起的思考、工具卡片里的不算（看不见） */
function findRanges(root, q) {
  if (!root || !q) return [];
  const needle = q.toLocaleLowerCase();
  const out = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode: (n) => {
      const el = n.parentElement;
      if (!el || el.closest('.chat-find, button, svg') || !el.getClientRects().length) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  for (let n = walker.nextNode(); n && out.length < 2000; n = walker.nextNode()) {
    const hay = n.data.toLocaleLowerCase();
    for (let i = hay.indexOf(needle); i >= 0; i = hay.indexOf(needle, i + needle.length)) {
      const r = document.createRange();
      r.setStart(n, i);
      r.setEnd(n, i + needle.length);
      out.push(r);
    }
  }
  return out;
}

/**
 * 对话里查找（Ctrl/⌘ F，对话页显示时）：高亮所有匹配（CSS Highlight API，不改 DOM；不支持的浏览器只跳过去不高亮），
 * 回车下一个、Shift+回车上一个、Esc 关。只找已经加载的内容。
 * list：滚动的那一层；inner：内容；deps：内容变了重新找。
 */
export function FindBar({ list, inner, more, deps, onClose }) {
  const { t } = useTranslation();
  const [q, setQ] = useState('');
  const [ranges, setRanges] = useState([]);
  const [cur, setCur] = useState(0);
  const input = useRef(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  useEffect(() => {
    const focus = () => {
      input.current?.focus();
      input.current?.select();
    };
    window.addEventListener('ll-chat-find', focus);
    return () => window.removeEventListener('ll-chat-find', focus);
  }, []);
  const lastQ = useRef('');
  useEffect(() => {
    const id = setTimeout(() => {
      const rs = findRanges(inner.current, q.trim());
      setRanges(rs);
      if (lastQ.current !== q) {
        // 换了要找的字：从眼前这屏开始往下找（和浏览器自己的查找一样）
        lastQ.current = q;
        const top = list.current?.getBoundingClientRect().top ?? 0;
        const at = rs.findIndex((r) => r.getBoundingClientRect().top >= top + 48);
        setCur(at < 0 ? Math.max(0, rs.length - 1) : at);
      } else {
        setCur((c) => (c < rs.length ? c : 0));
      }
    }, 120);
    return () => clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- deps 是外面给的内容版本
  }, [q, ...deps]);
  useEffect(() => {
    if (!canHighlight()) return undefined;
    CSS.highlights.set(HL, new Highlight(...ranges));
    if (ranges[cur]) CSS.highlights.set(HL_CUR, new Highlight(ranges[cur]));
    else CSS.highlights.delete(HL_CUR);
    return () => {
      CSS.highlights.delete(HL);
      CSS.highlights.delete(HL_CUR);
    };
  }, [ranges, cur]);
  // 当前这个滚到列表中间
  useEffect(() => {
    const r = ranges[cur];
    const el = list.current;
    if (!r || !el) return;
    const box = r.getBoundingClientRect();
    const view = el.getBoundingClientRect();
    if (box.top < view.top + 48 || box.bottom > view.bottom - 24) el.scrollTo({ top: el.scrollTop + box.top - view.top - view.height / 3 });
  }, [ranges, cur, list]);
  const go = (d) => ranges.length && setCur((c) => (c + d + ranges.length) % ranges.length);
  const empty = q.trim() && !ranges.length;
  return (
    <div className="chat-find" role="search">
      <input
        ref={input}
        value={q}
        onChange={(e) => setQ(e.target.value)}
        placeholder={t('chat.find_ph')}
        aria-label={t('chat.find_ph')}
        data-empty={empty ? '' : undefined}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !imeComposing(e)) {
            e.preventDefault();
            go(e.shiftKey ? -1 : 1);
          } else if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            onClose();
          }
        }}
      />
      <span className="chat-find-count" aria-live="polite">
        {q.trim() ? (ranges.length ? `${cur + 1} / ${ranges.length}` : t('chat.find_none')) : ''}
      </span>
      {more && q.trim() && <span className="chat-find-more">{t('chat.find_more')}</span>}
      <button type="button" className="btn btn-plain btn-icon" onClick={() => go(-1)} disabled={!ranges.length} aria-label={t('chat.find_prev')} title={t('chat.find_prev')}>
        <ArrowUp size={16} />
      </button>
      <button type="button" className="btn btn-plain btn-icon" onClick={() => go(1)} disabled={!ranges.length} aria-label={t('chat.find_next')} title={t('chat.find_next')}>
        <ArrowDown size={16} />
      </button>
      <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('action.close')} title={t('action.close')}>
        <X size={16} />
      </button>
    </div>
  );
}

/** 滚到第 n 条用户消息（`.chat-user` 按顺序），闪一下让人看见是哪条 */
function scrollToUser(list, el) {
  if (!list || !el) return;
  const top = el.getBoundingClientRect().top - list.getBoundingClientRect().top + list.scrollTop - 16;
  list.scrollTo({ top, behavior: window.matchMedia?.('(prefers-reduced-motion: reduce)').matches ? 'auto' : 'smooth' });
  el.dataset.flash = '';
  setTimeout(() => delete el.dataset.flash, 1200);
}

/**
 * 对话页的快捷键（对话页显示时）：Ctrl/⌘ F 查找，Alt+↑/↓ 跳到上一条 / 下一条自己发的消息。
 * 焦点在终端（iframe）里时浏览器收不到，正好不抢终端的按键。
 */
export function useChatKeys({ active, list, inner, onFind }) {
  useEffect(() => {
    if (!active) return undefined;
    const key = (e) => {
      if (isMod(e, 'f')) {
        e.preventDefault();
        onFind();
        return;
      }
      if (!e.altKey || e.ctrlKey || e.metaKey || (e.key !== 'ArrowUp' && e.key !== 'ArrowDown')) return;
      const box = list.current;
      const users = [...(inner.current?.querySelectorAll('.chat-user') ?? [])];
      if (!box || !users.length) return;
      e.preventDefault();
      const top = box.getBoundingClientRect().top + 20;
      const at = (el) => el.getBoundingClientRect().top;
      const to = e.key === 'ArrowUp' ? users.filter((el) => at(el) < top - 4).pop() : users.find((el) => at(el) > top + 4);
      scrollToUser(box, to);
    };
    window.addEventListener('keydown', key);
    return () => window.removeEventListener('keydown', key);
  }, [active, list, inner, onFind]);
}

// ---------------- 目录 ----------------

/**
 * 右侧“目录”栏：你发过的每一句，点了跳过去；上面是导出（复制成 Markdown、下载 .md）。
 * 只列已经加载的；更早的要往上翻。
 */
export function OutlineTab({ items, list, inner, agent, title, more }) {
  const { t } = useTranslation();
  const toast = useToast();
  const users = useMemo(() => items.filter((x) => x.k === 'user' && x.text.trim()), [items]);
  const md = () => chatToMarkdown(items, { user: t('chat.export_me'), agent, title });
  const jump = (key) => scrollToUser(list.current, inner.current?.querySelector(`.chat-user[data-key="${CSS.escape(key)}"]`));
  const copyMd = async () => {
    const text = md();
    if (await copyText(text)) toast(t('action.copied'));
    else window.prompt('', text);
  };
  const download = () => {
    const url = URL.createObjectURL(new Blob([md()], { type: 'text/markdown;charset=utf-8' }));
    const a = document.createElement('a');
    a.href = url;
    a.download = exportName(title);
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  };
  return (
    <div className="chat-outline">
      <div className="chat-outline-acts">
        <button type="button" className="btn btn-ghost btn-sm" onClick={copyMd} disabled={!items.length}>
          <ClipboardCopy size={15} aria-hidden="true" />
          {t('chat.export_copy')}
        </button>
        <button type="button" className="btn btn-ghost btn-sm" onClick={download} disabled={!items.length}>
          <Download size={15} aria-hidden="true" />
          {t('chat.export_download')}
        </button>
      </div>
      {more && <p className="chat-note">{t('chat.outline_more')}</p>}
      {users.length ? (
        <ol className="chat-outline-list">
          {users.map((u) => (
            <li key={u.key}>
              <button type="button" onClick={() => jump(u.key)} title={u.text}>
                {u.text.trim().split('\n')[0]}
              </button>
            </li>
          ))}
        </ol>
      ) : (
        <p className="chat-note">{t('chat.outline_empty')}</p>
      )}
      <p className="chat-note chat-outline-hint">{t('chat.outline_hint')}</p>
    </div>
  );
}
