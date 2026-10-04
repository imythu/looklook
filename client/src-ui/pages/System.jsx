// 系统：这台电脑的 CPU、内存、网络、磁盘与占用最多的程序。数据由客户端每 5 秒采样一次（src/metrics.rs），
// 只保存在内存里：最近 1 小时逐点、最近 24 小时逐分钟；客户端重启后从头开始。
import { AlertTriangle } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from '../shared/api.js';
import { Card, ErrorNote, Skeleton } from '../shared/ui.jsx';
import { Seg } from './Settings.jsx';

const RANGES = ['5m', '1h', '24h'];
const POLL_MS = 5000;

function fmtBytes(n, digits = 1) {
  if (n == null) return '–';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let i = 0;
  let v = n;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return `${i === 0 ? Math.round(v) : v.toFixed(v >= 100 ? 0 : digits)} ${units[i]}`;
}
const fmtRate = (n) => `${fmtBytes(n)}/s`;
const fmtPct = (n) => (n == null ? '–' : `${n < 10 ? n.toFixed(1) : Math.round(n)}%`);

function fmtClock(ms, withSeconds) {
  return new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', ...(withSeconds ? { second: '2-digit' } : {}) });
}

function fmtUptime(t, secs) {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  if (d > 0) return t('system.uptime_dh', { d, h });
  if (h > 0) return t('system.uptime_hm', { h, m });
  return t('system.uptime_m', { m });
}

/** 把最大值放大到好读的整数刻度（1、2、5 × 10ⁿ，按 1024 进位的字节数也适用）。 */
function niceMax(v) {
  if (!(v > 0)) return 1;
  const p = 10 ** Math.floor(Math.log10(v));
  const f = v / p;
  return (f <= 1 ? 1 : f <= 2 ? 2 : f <= 5 ? 5 : 10) * p;
}

/**
 * 折线图：0 到 max 一根纵轴，横轴是时间。悬停（或手指按住）显示十字线和该时刻每条线的数值。
 * series: [{ key, label, cls }]，cls 决定颜色（styles.css 里的 .viz-s1 / .viz-s2）。
 */
function LineChart({ title, samples, series, max, format, withSeconds, summary }) {
  const ref = useRef(null);
  const [hover, setHover] = useState(null);
  const n = samples.length;
  const top = max ?? niceMax(Math.max(0, ...series.flatMap((s) => samples.map((x) => x[s.key]))));
  const W = 1000;
  const H = 100;
  const x = (i) => (n <= 1 ? W : (i / (n - 1)) * W);
  const y = (v) => H - (Math.min(v, top) / top) * H;
  const path = (key) => samples.map((s, i) => `${i ? 'L' : 'M'}${x(i).toFixed(1)},${y(s[key]).toFixed(1)}`).join('');
  const onMove = (e) => {
    const r = ref.current?.getBoundingClientRect();
    if (!r || n === 0) return;
    const f = Math.min(1, Math.max(0, (e.clientX - r.left) / r.width));
    setHover(Math.round(f * (n - 1)));
  };
  const h = hover != null && hover < n ? samples[hover] : null;
  const last = samples[n - 1];
  return (
    <figure className="viz">
      <figcaption className="viz-head">
        <b>{title}</b>
        {series.length > 1 && (
          <span className="viz-legend">
            {series.map((s) => (
              <span key={s.key} className="viz-key">
                <i className={s.cls} aria-hidden="true" />
                {s.label}
              </span>
            ))}
          </span>
        )}
      </figcaption>
      <div className="viz-body">
        <div className="viz-axis" aria-hidden="true">
          <span>{format(top)}</span>
          <span>{format(top / 2)}</span>
          <span>0</span>
        </div>
        <div
          className="viz-plot"
          ref={ref}
          role="img"
          aria-label={summary}
          onPointerMove={onMove}
          onPointerDown={onMove}
          onPointerLeave={() => setHover(null)}
        >
          <svg viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="none" aria-hidden="true">
            {[0, 0.5, 1].map((g) => (
              <line key={g} className="viz-grid" x1="0" x2={W} y1={H * g} y2={H * g} vectorEffect="non-scaling-stroke" />
            ))}
            {n > 1 && series.length === 1 && <path className={`viz-area ${series[0].cls}`} d={`${path(series[0].key)}L${W},${H}L0,${H}Z`} />}
            {n > 1 && series.map((s) => <path key={s.key} className={`viz-line ${s.cls}`} d={path(s.key)} vectorEffect="non-scaling-stroke" />)}
          </svg>
          {/* 线的末端直接标出当前值（多条线时也能不看颜色认出是哪条） */}
          {last &&
            series.map((s) => (
              <span key={s.key} className="viz-end" style={{ top: `${(y(last[s.key]) / H) * 100}%` }}>
                <i className={s.cls} aria-hidden="true" />
              </span>
            ))}
          {h && (
            <>
              <span className="viz-cross" style={{ left: `${(x(hover) / W) * 100}%` }} aria-hidden="true" />
              {series.map((s) => (
                <span key={s.key} className={`viz-dot ${s.cls}`} style={{ left: `${(x(hover) / W) * 100}%`, top: `${(y(h[s.key]) / H) * 100}%` }} aria-hidden="true" />
              ))}
              <div className={`viz-tip ${x(hover) > W / 2 ? 'left' : ''}`} style={{ left: `${(x(hover) / W) * 100}%` }}>
                <span className="muted">{fmtClock(h.t, withSeconds)}</span>
                {series.map((s) => (
                  <span key={s.key} className="viz-tip-row">
                    <i className={s.cls} aria-hidden="true" />
                    {series.length > 1 && <span className="muted">{s.label}</span>}
                    <b>{format(h[s.key])}</b>
                  </span>
                ))}
              </div>
            </>
          )}
          {n < 2 && <span className="viz-empty muted small">…</span>}
        </div>
      </div>
      <div className="viz-time muted" aria-hidden="true">
        <span>{samples[0] ? fmtClock(samples[0].t, withSeconds) : ''}</span>
        <span>{last ? fmtClock(last.t, withSeconds) : ''}</span>
      </div>
    </figure>
  );
}

function Tile({ label, value, sub }) {
  return (
    <div className="tile">
      <span className="small muted">{label}</span>
      <b className="tile-value">{value}</b>
      {sub && <span className="small muted ellipsis">{sub}</span>}
    </div>
  );
}

/** 用量条：90% 以上标成警告色，并配图标和文字（不只靠颜色）。 */
function Meter({ pct, label }) {
  const full = pct >= 90;
  return (
    <div className="meter" data-full={full || undefined} role="meter" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} aria-label={label}>
      <span style={{ width: `${Math.min(100, pct)}%` }} />
    </div>
  );
}

function stats(samples, key) {
  const v = samples.map((s) => s[key]);
  if (!v.length) return null;
  return { min: Math.min(...v), max: Math.max(...v), avg: v.reduce((a, b) => a + b, 0) / v.length };
}

export default function System() {
  const { t } = useTranslation();
  const [range, setRange] = useState(() => {
    try {
      return RANGES.includes(localStorage.getItem('ll-system-range')) ? localStorage.getItem('ll-system-range') : '5m';
    } catch {
      return '5m';
    }
  });
  const [data, setData] = useState(null);
  const [error, setError] = useState(null);
  useEffect(() => {
    try {
      localStorage.setItem('ll-system-range', range);
    } catch {
      /* 隐私模式 */
    }
    let alive = true;
    const load = () =>
      api
        .get(`/metrics?range=${range}`)
        .then((d) => alive && (setData(d), setError(null)))
        .catch((e) => alive && setError(e));
    load();
    const id = setInterval(load, POLL_MS);
    return () => {
      alive = false;
      clearInterval(id);
    };
  }, [range]);

  const head = (
    <div className="page-head">
      <div>
        <h1>{t('system.title')}</h1>
        {data?.now?.t > 0 && (
          <p className="small muted" style={{ margin: '4px 0 0' }}>
            {[data.now.cpu_brand, t('system.cores', { n: data.now.cores.length }), t('system.uptime', { when: fmtUptime(t, data.now.uptime) })].filter(Boolean).join(' · ')}
          </p>
        )}
      </div>
      <Seg value={range} label={t('system.range')} onChange={setRange} options={RANGES.map((r) => [r, t(`system.range_${r}`)])} />
    </div>
  );
  if (!data || !(data.now?.t > 0)) {
    return (
      <>
        {head}
        <ErrorNote error={error} />
        {!error && <p className="muted small">{t('system.warming')}</p>}
        <Skeleton height={96} />
        <Skeleton height={220} style={{ marginTop: 16 }} />
      </>
    );
  }
  const { now, series: samples } = data;
  const sec = data.step < 60;
  const memPct = now.mem_total ? (now.mem_used / now.mem_total) * 100 : 0;
  const cpuS = stats(samples, 'cpu');
  const memS = stats(samples, 'mem');
  const summary = (name, s, f) => (s ? t('system.summary', { name, min: f(s.min), avg: f(s.avg), max: f(s.max) }) : name);
  return (
    <>
      {head}
      <ErrorNote error={error} />
      <div className="tiles sys-tiles">
        <Tile label={t('system.cpu')} value={fmtPct(now.cpu)} sub={cpuS && t('system.peak', { v: fmtPct(cpuS.max) })} />
        <Tile label={t('system.memory')} value={fmtPct(memPct)} sub={`${fmtBytes(now.mem_used)} / ${fmtBytes(now.mem_total)}`} />
        <Tile label={t('system.network')} value={`↓ ${fmtRate(now.rx)}`} sub={`↑ ${fmtRate(now.tx)}`} />
        {now.load ? (
          <Tile label={t('system.load')} value={now.load[0].toFixed(2)} sub={`${now.load[1].toFixed(2)} · ${now.load[2].toFixed(2)}`} />
        ) : (
          <Tile label={t('system.swap')} value={now.swap_total ? fmtPct((now.swap_used / now.swap_total) * 100) : '–'} sub={now.swap_total ? `${fmtBytes(now.swap_used)} / ${fmtBytes(now.swap_total)}` : t('system.none')} />
        )}
      </div>
      <Card>
        <div className="viz-grid-2">
          <LineChart title={t('system.cpu')} samples={samples} series={[{ key: 'cpu', label: t('system.cpu'), cls: 'viz-s1' }]} max={100} format={fmtPct} withSeconds={sec} summary={summary(t('system.cpu'), cpuS, fmtPct)} />
          <LineChart title={t('system.memory')} samples={samples} series={[{ key: 'mem', label: t('system.memory'), cls: 'viz-s1' }]} max={100} format={fmtPct} withSeconds={sec} summary={summary(t('system.memory'), memS, fmtPct)} />
        </div>
        <LineChart
          title={t('system.network')}
          samples={samples}
          series={[
            { key: 'rx', label: t('system.download'), cls: 'viz-s1' },
            { key: 'tx', label: t('system.upload'), cls: 'viz-s2' },
          ]}
          format={fmtRate}
          withSeconds={sec}
          summary={`${summary(t('system.download'), stats(samples, 'rx'), fmtRate)}; ${summary(t('system.upload'), stats(samples, 'tx'), fmtRate)}`}
        />
        {data.step >= 60 && <p className="small muted" style={{ margin: '8px 0 0' }}>{t('system.averaged')}</p>}
      </Card>
      <Card title={t('system.per_core')}>
        <div className="cores">
          {now.cores.map((c, i) => (
            <div key={i} className="core" title={`#${i + 1} · ${fmtPct(c)}`}>
              <span className="core-bar" style={{ height: `${Math.max(2, c)}%` }} />
              <span className="sr-only">{`#${i + 1} ${fmtPct(c)}`}</span>
            </div>
          ))}
        </div>
      </Card>
      <Card title={t('system.disks')}>
        {now.disks.length === 0 ? (
          <p className="muted small">{t('system.none')}</p>
        ) : (
          <div className="stack">
            {now.disks.map((d) => {
              const used = d.total - d.available;
              const pct = (used / d.total) * 100;
              return (
                <div key={d.mount} className="disk">
                  <div className="between small" style={{ gap: 8 }}>
                    <span className="mono ellipsis">
                      {d.mount} <span className="muted">{d.fs}</span>
                    </span>
                    <span className="nowrap">
                      {pct >= 90 && (
                        <span className="warn-text">
                          <AlertTriangle size={14} aria-hidden="true" style={{ verticalAlign: '-2px' }} /> {t('system.disk_full')}{' '}
                        </span>
                      )}
                      {t('system.disk_free', { free: fmtBytes(d.available), total: fmtBytes(d.total) })}
                    </span>
                  </div>
                  <Meter pct={pct} label={`${d.mount} ${fmtPct(pct)}`} />
                </div>
              );
            })}
          </div>
        )}
      </Card>
      <Card title={t('system.top')} actions={now.processes != null && <span className="small muted">{t('system.process_count', { n: now.processes })}</span>}>
        {now.top.length === 0 ? (
          <p className="muted small">{t('system.top_loading')}</p>
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>{t('system.process')}</th>
                  <th className="num">PID</th>
                  <th className="num">{t('system.cpu')}</th>
                  <th className="num">{t('system.memory')}</th>
                </tr>
              </thead>
              <tbody>
                {now.top.map((p) => (
                  <tr key={p.pid}>
                    <td>
                      <span className="cell-name" title={p.name}>
                        {p.name}
                      </span>
                    </td>
                    <td className="num mono muted">{p.pid}</td>
                    <td className="num">{fmtPct(p.cpu)}</td>
                    <td className="num">{fmtBytes(p.mem)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
    </>
  );
}
