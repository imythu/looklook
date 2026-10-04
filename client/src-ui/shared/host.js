// 管理台打开地址的即时校验，与 src/settings.rs 的 parse_open_host 规则一致（服务端仍会再校验一次）。
// 返回错误规则名（对应 error:rule.*），合法时返回 null。

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;

function checkV6(s) {
  // 交给浏览器的 URL 解析器判断 IPv6 是否合法。
  try {
    const h = new URL(`http://[${s}]/`).hostname.slice(1, -1);
    if (h === '::') return 'host_unusable';
    if (/^ff/i.test(h)) return 'host_unusable';
    return null;
  } catch {
    return 'host_ip';
  }
}

export function checkOpenHost(input) {
  const s = input.trim();
  if (!s) return null;
  if (s.includes('://')) return 'host_scheme';
  if (/[/?#@\s]/.test(s)) return 'host_path';
  if (s.startsWith('[')) {
    const end = s.indexOf(']');
    if (end < 0) return 'host_ip';
    if (end !== s.length - 1) return 'host_port';
    return checkV6(s.slice(1, end));
  }
  if ((s.match(/:/g) ?? []).length >= 2) return checkV6(s);
  if (s.includes(':')) return 'host_port';
  const name = s.replace(/\.+$/, '').toLowerCase();
  if (/^[\d.]+$/.test(name)) {
    if (!IPV4.test(name)) return 'host_ip';
    const a = Number(name.split('.')[0]);
    if (a === 0 || (a >= 224 && a <= 239) || name === '255.255.255.255') return 'host_unusable';
    return null;
  }
  if (!name || name.length > 253) return 'host_name';
  const labels = name.split('.');
  if (labels.some((l) => !/^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/.test(l))) return 'host_name';
  if (/^\d+$/.test(labels[labels.length - 1])) return 'host_name';
  return null;
}

/** 放进网址里的主机写法（IPv6 加方括号）。 */
export function urlHost(host) {
  const h = host.trim().replace(/\.+$/, '').toLowerCase();
  return h.includes(':') && !h.startsWith('[') ? `[${h}]` : h;
}
