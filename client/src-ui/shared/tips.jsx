// 使用提示（docs/ONBOARDING_2026-10.md §3.1）：一次性提示、上手清单的步骤，看过就不再出现。
// - 看过的 id 存在这台电脑上（GET/POST/DELETE /api/tips，同步集合 `tips`）：手机、别的浏览器打开的也是这台电脑，所以共用；
//   开了设置同步时几台电脑也共用。浏览器里另存一份（looklook.tips），打开页面时先用它，免得刚打开时把看过的又闪一下；
// - 同一屏同时满足条件的提示只出一条：useTip 先“排队”，优先级高的（同级先来的）显示；
// - TipPop：贴在按钮下面的气泡（父元素要 position: relative）；TipBar：页面里的一行，右边“知道了”。
import { X } from 'lucide-react';
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { localApi } from './api.js';

const CACHE_KEY = 'looklook.tips';
// 以前各自记在浏览器里的提示：认作看过
const LEGACY = [
  ['devices.switch', () => localStorage.getItem('looklook.device-tip') === '1'],
  ['files.upload', () => JSON.parse(localStorage.getItem('ll-files-tip') || '{}').done === true],
];

function readCache() {
  try {
    const v = JSON.parse(localStorage.getItem(CACHE_KEY) || 'null');
    return Array.isArray(v) ? new Set(v) : null;
  } catch {
    return null;
  }
}

function writeCache(set) {
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify([...set]));
  } catch {
    /* 无痕模式等：只是下次打开时晚一点知道 */
  }
}

function legacySeen() {
  return LEGACY.filter(([, f]) => {
    try {
      return f();
    } catch {
      return false;
    }
  }).map(([id]) => id);
}

const TipsCtx = createContext(null);

export function TipsProvider({ enabled, children }) {
  const cached = useMemo(readCache, []);
  const [seen, setSeen] = useState(cached ?? new Set());
  const [ready, setReady] = useState(Boolean(cached));
  const [claims, setClaims] = useState([]); // [{ id, prio, n }]
  const order = useRef(0);

  useEffect(() => {
    if (!enabled) return undefined;
    let gone = false;
    localApi
      .get('/tips')
      .then(async (r) => {
        let ids = r.seen ?? [];
        const old = legacySeen().filter((id) => !ids.includes(id));
        if (old.length) ids = (await localApi.post('/tips', { ids: old }).catch(() => null))?.seen ?? [...ids, ...old];
        if (gone) return;
        const next = new Set(ids);
        writeCache(next);
        setSeen(next);
        setReady(true);
      })
      // 旧版本的电脑没有这个接口：只记在浏览器里
      .catch(() => !gone && setReady(true));
    return () => {
      gone = true;
    };
  }, [enabled]);

  const update = useCallback((ids, on) => {
    setSeen((s) => {
      const next = new Set(s);
      for (const id of ids) on ? next.add(id) : next.delete(id);
      writeCache(next);
      return next;
    });
  }, []);

  const value = useMemo(
    () => ({
      ready,
      has: (id) => seen.has(id),
      mark: (...ids) => {
        const todo = ids.filter((id) => !seen.has(id));
        if (!todo.length) return;
        update(todo, true);
        localApi.post('/tips', { ids: todo }).catch(() => {});
      },
      unmark: (...ids) => {
        update(ids, false);
        localApi.post('/tips', { ids, seen: false }).catch(() => {});
      },
      reset: async () => {
        await localApi.del('/tips');
        setSeen(new Set());
        writeCache(new Set());
      },
      claim: (id, prio) =>
        setClaims((c) => (c.some((x) => x.id === id) ? c : [...c, { id, prio, n: order.current++ }])),
      release: (id) => setClaims((c) => (c.some((x) => x.id === id) ? c.filter((x) => x.id !== id) : c)),
      winner: claims.reduce((a, b) => (!a || b.prio > a.prio || (b.prio === a.prio && b.n < a.n) ? b : a), null)?.id ?? null,
    }),
    [ready, seen, claims, update],
  );
  return <TipsCtx.Provider value={value}>{children}</TipsCtx.Provider>;
}

const NONE = { ready: false, has: () => true, mark: () => {}, unmark: () => {}, reset: async () => {}, claim: () => {}, release: () => {}, winner: null };

export function useTips() {
  return useContext(TipsCtx) ?? NONE;
}

/**
 * 一条一次性提示：`when` 成立、没看过、轮到它时 show 为 true。done() 记成看过。
 * 同一个 id 只应在一处显示（隐藏着的会话页不要传 when=true）。
 */
export function useTip(id, when = true, prio = 0) {
  const tips = useTips();
  const want = tips.ready && when && !tips.has(id);
  useEffect(() => {
    if (!want) return undefined;
    tips.claim(id, prio);
    return () => tips.release(id);
  }, [want, id, prio]);
  const done = useCallback(() => tips.mark(id), [tips.mark, id]);
  return [want && tips.winner === id, done];
}

/** 贴在元素旁边的气泡：标题、一两句话、“知道了”和可选的动作按钮。`side`：left / right 对齐，`up` 显示在上面。 */
export function TipPop({ title, children, onDone, action, side = 'left', up = false, className = '' }) {
  const { t } = useTranslation();
  return (
    <div className={`tip-pop tip-${side}${up ? ' tip-up' : ''} ${className}`} role="note" onPointerDown={(e) => e.stopPropagation()}>
      {title && <p className="tip-title">{title}</p>}
      <div className="tip-body">{children}</div>
      <div className="tip-actions">
        <button type="button" className="btn btn-plain" onClick={onDone}>
          {t('tips.ok')}
        </button>
        {action && (
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => {
              onDone();
              action.onClick();
            }}
          >
            {action.label}
          </button>
        )}
      </div>
    </div>
  );
}

/** 页面里的一行提示，右边关掉（= 看过）；`action` 是行内的按钮。 */
export function TipBar({ icon: Icon, children, onDone, action, className = '' }) {
  const { t } = useTranslation();
  return (
    <div className={`tip-bar ${className}`} role="note">
      {Icon && <Icon size={16} className="tip-bar-icon" aria-hidden="true" />}
      <div className="grow tip-bar-text">{children}</div>
      {action && (
        <button
          type="button"
          className="btn btn-ghost btn-sm"
          onClick={() => {
            onDone();
            action.onClick();
          }}
        >
          {action.label}
        </button>
      )}
      <button type="button" className="btn btn-plain btn-icon" onClick={onDone} aria-label={t('tips.ok')} title={t('tips.ok')}>
        <X size={15} />
      </button>
    </div>
  );
}
