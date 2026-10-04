// 极简前端路由（History API）。客户端网关对非 /api 路径回落到入口 HTML。
import { createContext, useContext, useEffect, useState } from 'react';

const Ctx = createContext({ path: '/', search: '', navigate: () => {} });

export function Router({ children }) {
  const [loc, setLoc] = useState({ path: window.location.pathname, search: window.location.search });
  useEffect(() => {
    const on = () => setLoc({ path: window.location.pathname, search: window.location.search });
    window.addEventListener('popstate', on);
    return () => window.removeEventListener('popstate', on);
  }, []);
  const navigate = (to, { replace = false } = {}) => {
    if (replace) window.history.replaceState(null, '', to);
    else window.history.pushState(null, '', to);
    const u = new URL(to, window.location.origin);
    setLoc({ path: u.pathname, search: u.search });
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
