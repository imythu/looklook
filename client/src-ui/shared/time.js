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

export function formatTime(iso, { seconds = false } = {}) {
  if (!iso) return '—';
  const d = new Date(iso);
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

/** <input type="datetime-local"> 的本地值 ↔ ISO UTC。 */
export function toLocalInput(iso) {
  if (!iso) return '';
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function fromLocalInput(v) {
  return v ? new Date(v).toISOString() : null;
}
