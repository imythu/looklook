// 账户接口（电脑、会员、邀请、账户与安全），数据在看看平台上：
// - 远程打开（WebRTC，顶层是平台的启动页）：直接请求用户站的 /_ll/web/*，不经过电脑，电脑离线或还在连接时也能用；
// - 本机、局域网打开：请求这台电脑的 /api/platform/*，由它签名转发到平台（客户端 gateway/api.rs）。
// 错误和本机接口一样抛出 ApiError。

import { ApiError, rtcMode } from './api.js';

const BASE = rtcMode ? '/_ll/web' : '/api/platform';

async function request(method, path, body) {
  const headers = { Accept: 'application/json' };
  if (method !== 'GET') headers['X-Looklook-Client'] = '1';
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  let res;
  try {
    res = await fetch(`${BASE}${path}`, { method, headers, credentials: 'same-origin', cache: 'no-store', body: body === undefined ? undefined : JSON.stringify(body) });
  } catch {
    throw new ApiError(0, rtcMode ? 'NETWORK' : 'CLIENT_UNREACHABLE');
  }
  if (res.status === 204) return null;
  const data = await res.json().catch(() => null);
  if (!res.ok) {
    const e = data?.error ?? {};
    throw new ApiError(res.status, e.code ?? 'INTERNAL', e.params ?? {});
  }
  return data;
}

export const platform = {
  get: (p) => request('GET', p),
  post: (p, b) => request('POST', p, b ?? {}),
  put: (p, b) => request('PUT', p, b ?? {}),
  patch: (p, b) => request('PATCH', p, b ?? {}),
  del: (p) => request('DELETE', p),
};

/** 账户数据来自哪里（页面右上角的小标签）。 */
export const platformDirect = rtcMode;
