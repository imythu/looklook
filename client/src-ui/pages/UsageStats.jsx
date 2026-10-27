// 设置 → AI 助手 → 用量统计：Claude Code、Codex 的会话记录里的 token 用量，按 models.dev 的 API 价格估算费用
// （usage_stats.rs，后台每 10 分钟扫一遍；价格每天取一次）。订阅用户看到的是“按 API 价格折算”的数。
// 只在这台电脑上统计，不上传。
import { RefreshCw } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { formatTime } from '../shared/time.js';
import { Button, Card, ErrorNote, Seg, Skeleton, useAction, useLoad } from '../shared/ui.jsx';

const RANGES = [7, 30, 90];
const AGENT_NAME = { claude: 'Claude Code', codex: 'Codex' };

function money(n) {
  if (!n) return '$0';
  return n < 0.01 ? '<$0.01' : `$${n.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;
}

function tokens(n) {
  if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
  if (n >= 1e6) return `${(n / 1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
  return String(n ?? 0);
}

/** 每天的费用：一根柱子一天，悬停看当天每个助手的费用和 token */
function DayBars({ days }) {
  const { t } = useTranslation();
  const max = Math.max(...days.map((d) => d.cost), 0);
  if (!max) return <p className="small muted">{t('usage.no_cost')}</p>;
  const tip = (d) =>
    [
      `${d.date} · ${money(d.cost)} · ${tokens(d.tokens)} tokens`,
      ...Object.entries(d.agents ?? {}).map(([a, v]) => `${AGENT_NAME[a] ?? a}: ${money(v.cost)} · ${tokens(v.tokens)}`),
    ].join('\n');
  return (
    <div className="usage-chart" role="img" aria-label={t('usage.chart_label')}>
      <span className="usage-max small muted">{money(max)}</span>
      <div className="usage-bars">
        {days.map((d) => (
          <span key={d.date} className="usage-bar-hit" title={tip(d)}>
            <span className="usage-bar" style={{ height: `${Math.max((d.cost / max) * 100, d.cost ? 2 : 0)}%` }} />
          </span>
        ))}
      </div>
      <div className="usage-axis small muted">
        <span>{days[0]?.date.slice(5)}</span>
        <span>{days[days.length - 1]?.date.slice(5)}</span>
      </div>
    </div>
  );
}

export default function UsageStatsCard() {
  const { t } = useTranslation();
  const [days, setDays] = useState(30);
  // 按界面所在时区分天
  const tz = -new Date().getTimezoneOffset();
  const [data, error, reload] = useLoad(() => api.get(`/usage-stats?days=${days}&tz=${tz}`), [days]);
  const { busy, error: scanErr, run } = useAction();
  const rescan = async () => {
    if (await run(() => api.post(`/usage-stats/scan?days=${days}&tz=${tz}`, {}))) reload();
  };
  const ready = data?.ready;
  return (
    <div id="usage" className="anchor">
      <Card
        title={t('usage.title')}
        actions={
          <Button variant="ghost" icon={RefreshCw} busy={busy} onClick={rescan} title={t('usage.rescan_hint')}>
            {t('usage.rescan')}
          </Button>
        }
      >
        <p className="muted small">{t('usage.intro')}</p>
        <ErrorNote error={error ?? scanErr} />
        {!data && !error && <Skeleton height={160} />}
        {data && !ready && <p className="small muted">{t('usage.not_ready')}</p>}
        {ready && (
          <>
            <Seg value={days} label={t('usage.range')} onChange={setDays} options={RANGES.map((d) => [d, t('usage.days', { n: d })])} />
            <div className="usage-tiles">
              <div className="usage-tile">
                <span className="small muted">{t('usage.today')}</span>
                <b>{money(data.today.cost)}</b>
                <span className="small muted">{tokens(data.today.tokens)} tokens</span>
              </div>
              <div className="usage-tile">
                <span className="small muted">{t('usage.period', { n: days })}</span>
                <b>{money(data.total.cost)}</b>
                <span className="small muted">
                  {tokens(data.total.tokens)} tokens · {t('usage.requests', { n: data.total.requests })}
                </span>
              </div>
            </div>
            <DayBars days={data.days} />
            {data.models.length > 0 && (
              <div className="table-wrap">
                <table className="table usage-table">
                  <thead>
                    <tr>
                      <th>{t('usage.col_model')}</th>
                      <th className="num">{t('usage.col_input')}</th>
                      <th className="num">{t('usage.col_cache')}</th>
                      <th className="num">{t('usage.col_output')}</th>
                      <th className="num">{t('usage.col_cost')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.models.map((m) => (
                      <tr key={`${m.agent}/${m.model}`}>
                        <td>
                          <span className="mono">{m.model}</span>
                          <span className="muted"> · {AGENT_NAME[m.agent] ?? m.agent}</span>
                        </td>
                        <td className="num">{tokens(m.input)}</td>
                        <td className="num" title={t('usage.cache_hint', { read: tokens(m.cache_read), write: tokens(m.cache_write) })}>
                          {tokens(m.cache_read + m.cache_write)}
                        </td>
                        <td className="num">{tokens(m.output)}</td>
                        <td className="num" title={m.price_from ? t('usage.price_from', { provider: m.price_from }) : undefined}>
                          {m.priced ? money(m.cost) : <span className="muted">{t('usage.no_price')}</span>}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
            <p className="muted small" style={{ margin: '12px 0 0' }}>
              {t('usage.note')}
              {data.pricing ? ` ${t('usage.pricing_at', { at: formatTime(data.pricing.fetched_at) })}` : ` ${t('usage.pricing_none')}`}
              {data.scanned_at && ` ${t('usage.scanned_at', { at: formatTime(data.scanned_at) })}`}
            </p>
          </>
        )}
      </Card>
    </div>
  );
}
