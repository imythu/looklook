// 所有时间以 UTC 传输；按浏览器时区显示，并标注时区偏移：2026-09-28 20:00 (UTC+08:00)。

function pad(n) {
  return String(n).padStart(2, '0');
}

export function offsetLabel(d) {
  const m = -d.getTimezoneOffset();
  const sign = m >= 0 ? '+' : '-';
  const a = Math.abs(m);
  return `UTC${sign}${pad(Math.floor(a / 60))}:${pad(a % 60)}`;
}

/**
 * 时间点 → 毫秒。接受 ISO 字符串（小数秒超过 3 位的截到 3 位，有的浏览器解析不了 `.943648`；
 * 没写时区的按 UTC）、秒或毫秒数。解析不了时 NaN。
 */
export function parseInstant(v) {
  if (typeof v === 'number') return v < 1e12 ? v * 1000 : v;
  if (typeof v !== 'string' || !v) return NaN;
  let s = v.trim().replace(' ', 'T').replace(/(\.\d{3})\d+/, '$1');
  if (/T\d{2}:\d{2}/.test(s) && !/(Z|[+-]\d{2}:?\d{2})$/i.test(s)) s += 'Z';
  return Date.parse(s);
}

export function formatTime(iso, { seconds = false } = {}) {
  if (!iso && iso !== 0) return '—';
  const d = new Date(parseInstant(iso));
  if (Number.isNaN(d.getTime())) return '—';
  const date = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
  const time = `${pad(d.getHours())}:${pad(d.getMinutes())}${seconds ? `:${pad(d.getSeconds())}` : ''}`;
  return `${date} ${time} (${offsetLabel(d)})`;
}

/** 只显示日期（年月日，按界面语言），如 2027年9月29日 / September 29, 2027。 */
export function formatDate(iso, locale) {
  if (!iso) return '—';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '—';
  return new Intl.DateTimeFormat(locale, { year: 'numeric', month: 'long', day: 'numeric' }).format(d);
}

/** 距 `iso` 还剩的整天数（向上取整，不小于 0）。 */
export function daysLeft(iso, now = Date.now()) {
  return Math.max(0, Math.ceil((new Date(iso).getTime() - now) / 86400000));
}

/** 相对时间，作为补充显示。 */
export function relativeTime(t, iso, now = Date.now()) {
  if (!iso) return '';
  const diff = Math.round((new Date(iso).getTime() - now) / 1000);
  const abs = Math.abs(diff);
  const [n, unit] =
    abs < 60 ? [abs, 'second'] : abs < 3600 ? [Math.round(abs / 60), 'minute'] : abs < 86400 ? [Math.round(abs / 3600), 'hour'] : [Math.round(abs / 86400), 'day'];
  return t(diff < 0 ? 'time.ago' : 'time.in', { n, unit: t(`time.unit.${unit}`, { count: n }) });
}

/** 一段时长，最大到天：3 天 4 小时 / 2 小时 5 分钟 / 12 分钟（不足一分钟按一分钟算）。 */
export function spanText(t, ms) {
  const mins = Math.max(1, Math.ceil(ms / 60000));
  const d = Math.floor(mins / 1440);
  const h = Math.floor((mins % 1440) / 60);
  const m = mins % 60;
  if (d > 0) return t('time.span_dh', { d, h });
  if (h > 0) return t('time.span_hm', { h, m });
  return t('time.span_m', { m });
}

/** <input type="datetime-local"> 的本地值 ↔ ISO UTC。 */
export function toLocalInput(iso) {
  if (!iso) return '';
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function fromLocalInput(v) {
  return v ? new Date(v).toISOString() : null;
}
