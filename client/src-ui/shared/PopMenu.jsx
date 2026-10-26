// “更多”菜单：点按钮弹出一列操作。菜单按按钮在屏幕上的位置固定定位（下面放得下就往下，否则往上），
// 不会被滚动列表、终端标题栏裁掉。
import { Check, ChevronDown, MoreHorizontal } from 'lucide-react';
import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

/**
 * items：[{ icon, label, onClick, danger, disabled, hint, checked }]，假值会被跳过，'sep' 是一条分隔线，{ head } 是一组的小标题。
 * className：按钮的样式（默认幽灵按钮）；icon：按钮图标；onPointerDown：例如手机终端里按住不抢键盘焦点。
 * text：按钮上除了图标还显示的文字（下拉选择，例如对话里切换模型）；disabled：按钮不可用。
 * children：按钮里的内容换成这个（例如侧栏的用户卡片）。sheet：窄屏上从底部弹出（选项多、要好点的菜单）。
 */
export default function PopMenu({ items, className = 'btn btn-ghost btn-icon', icon: Icon = MoreHorizontal, label, onPointerDown, text, disabled, children, sheet = false }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [pos, setPos] = useState(null);
  const btn = useRef(null);
  const list = useRef(null);
  useEffect(() => {
    if (!open) return undefined;
    const close = (e) => !btn.current?.contains(e.target) && !list.current?.contains(e.target) && setOpen(false);
    const esc = (e) => {
      if (e.key !== 'Escape') return;
      setOpen(false);
      btn.current?.focus();
    };
    const away = () => setOpen(false);
    document.addEventListener('pointerdown', close);
    document.addEventListener('keydown', esc);
    window.addEventListener('resize', away);
    window.addEventListener('scroll', away, true);
    // 点进终端（iframe）时外面收不到 pointerdown，靠失焦关掉
    window.addEventListener('blur', away);
    return () => {
      document.removeEventListener('pointerdown', close);
      document.removeEventListener('keydown', esc);
      window.removeEventListener('resize', away);
      window.removeEventListener('scroll', away, true);
      window.removeEventListener('blur', away);
    };
  }, [open]);
  const asSheet = sheet && open && window.matchMedia('(max-width: 640px)').matches;
  useLayoutEffect(() => {
    if (!open || !btn.current || !list.current || asSheet) return;
    const r = btn.current.getBoundingClientRect();
    const h = list.current.offsetHeight;
    const w = list.current.offsetWidth;
    const below = window.innerHeight - r.bottom - 8 >= h || r.top < h + 8;
    setPos({
      top: below ? Math.min(r.bottom + 6, window.innerHeight - h - 8) : Math.max(8, r.top - h - 6),
      left: Math.max(8, Math.min(r.right - w, window.innerWidth - w - 8)),
    });
  }, [open]);
  const shown = items.filter(Boolean);
  return (
    <>
      <button
        ref={btn}
        type="button"
        className={className}
        aria-label={label ?? t('action.more')}
        title={label ?? t('action.more')}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onPointerDown={onPointerDown}
        onClick={(e) => {
          e.stopPropagation();
          setPos(null);
          setOpen(!open);
        }}
      >
        {children ?? <Icon size={text ? 14 : 18} aria-hidden="true" />}
        {!children && text && (
          <>
            <span className="ellipsis">{text}</span>
            <ChevronDown size={13} aria-hidden="true" />
          </>
        )}
      </button>
      {asSheet && <div className="menu-pop-backdrop" aria-hidden="true" />}
      {open && (
        <ul ref={list} className={`menu-pop-list ${asSheet ? 'menu-pop-sheet' : 'menu-pop-fixed'}`} role="menu" aria-label={label} style={asSheet ? undefined : pos ?? { visibility: 'hidden', top: 0, left: 0 }}>
          {shown.map((x, i) =>
            x === 'sep' ? (
              <li key={`sep-${i}`} className="menu-pop-sep" role="separator" />
            ) : x.head ? (
              <li key={`head-${i}`} className="menu-pop-head" role="presentation">
                {x.head}
              </li>
            ) : (
            <li key={x.label}>
              <button
                type="button"
                role={x.checked !== undefined ? 'menuitemradio' : 'menuitem'}
                aria-checked={x.checked}
                className={x.danger ? 'danger' : ''}
                disabled={x.disabled}
                aria-describedby={x.hint ? `${x.label}-hint` : undefined}
                onPointerDown={onPointerDown}
                onClick={(e) => {
                  e.stopPropagation();
                  setOpen(false);
                  x.onClick();
                }}
              >
                {x.icon && <x.icon size={17} aria-hidden="true" />}
                {x.checked !== undefined && <Check size={15} aria-hidden="true" style={{ visibility: x.checked ? 'visible' : 'hidden' }} />}
                <span className="grow">
                  {x.label}
                  {x.hint && (
                    <span className="menu-pop-hint" id={`${x.label}-hint`}>
                      {x.hint}
                    </span>
                  )}
                </span>
              </button>
            </li>
            ),
          )}
        </ul>
      )}
    </>
  );
}
