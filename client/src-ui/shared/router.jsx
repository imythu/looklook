// 极简前端路由（History API）。客户端网关对非 /api 路径回落到入口 HTML。
import { createContext, useContext, useEffect, useState } from 'react';

const Ctx = createContext({ path: '/', search: '', hash: '', navigate: () => {} });

export function Router({ children }) {
  // pop：这次变化是浏览器的前进 / 后退（不是 navigate），抽屉据此算关掉时要后退几步
  const here = (pop = false) => ({ path: window.location.pathname, search: window.location.search, hash: window.location.hash, pop });
  const [loc, setLoc] = useState(here);
  useEffect(() => {
    const on = () => setLoc(here(true));
    window.addEventListener('popstate', on);
    window.addEventListener('hashchange', on);
    return () => {
      window.removeEventListener('popstate', on);
      window.removeEventListener('hashchange', on);
    };
  }, []);
  const navigate = (to, { replace = false } = {}) => {
    if (replace) window.history.replaceState(null, '', to);
    else window.history.pushState(null, '', to);
    const u = new URL(to, window.location.origin);
    setLoc({ path: u.pathname, search: u.search, hash: u.hash, pop: false, replace });
    window.scrollTo(0, 0);
  };
  return <Ctx.Provider value={{ ...loc, navigate }}>{children}</Ctx.Provider>;
}

// 规范化地址：去掉末尾斜杠、/index.html 等入口文件名；不认识的路径一律回到首页。
export function canonicalPath(path, isKnown) {
  let p = path.replace(/\/index\.html$/, '/');
  if (p.length > 1) p = p.replace(/\/+$/, '');
  return isKnown(p) ? p : '/';
}

// 地址不规范时用 replace 改写，不留下历史记录；返回规范后的路径。
export function useCanonicalPath(isKnown) {
  const { path, search, navigate } = useContext(Ctx);
  const canon = canonicalPath(path, isKnown);
  useEffect(() => {
    if (canon !== path) navigate(canon === '/' ? '/' : canon + search, { replace: true });
  }, [canon, path]);
  return canon;
}

export function useRouter() {
  return useContext(Ctx);
}

export function Link({ to, children, className, ...rest }) {
  const { navigate, path } = useRouter();
  const active = path === to;
  return (
    <a
      href={to}
      className={`${className ?? ''}${active ? ' active' : ''}`}
      onClick={(e) => {
        if (e.metaKey || e.ctrlKey || e.shiftKey || e.button !== 0) return;
        e.preventDefault();
        navigate(to);
      }}
      {...rest}
    >
      {children}
    </a>
  );
}

export function query(search, key) {
  return new URLSearchParams(search).get(key);
}
