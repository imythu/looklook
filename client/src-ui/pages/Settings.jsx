// 设置：终端字体与配色、默认文件夹、电脑名称、远程线路、访问控制、语言；关于与检查更新。
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import AccessCard from './Access.jsx';
import { FolderField, ShellPicker } from './Terminals.jsx';
import { setLocale } from '../shared/i18n.js';
import { DiagCard } from '../shared/Report.jsx';
import RelayPicker from '../shared/RelayPicker.jsx';
import { UpdateCheck } from '../shared/Update.jsx';
import { Button, Card, ErrorNote, Field, useAction, useLoad, useToast } from '../shared/ui.jsx';

// 开源仓库（MIT）
const SOURCE_URL = 'https://github.com/imythu/looklook';

const SYSTEM_CJK = '"PingFang SC", "Hiragino Sans GB", "Microsoft YaHei UI", "Microsoft YaHei", "Noto Sans Mono CJK SC", "Noto Sans CJK SC", monospace';

export function fontFamily(font) {
  return `"JetBrains Mono", ${font === 'wenkai' ? '"LXGW WenKai Mono", ' : ''}${SYSTEM_CJK}`;
}

/** 在管理台里加载终端字体（用于预览）。 */
function useTerminalFonts(enabled) {
  useEffect(() => {
    if (!enabled || document.getElementById('ll-term-fonts')) return;
    const l = document.createElement('link');
    l.id = 'll-term-fonts';
    l.rel = 'stylesheet';
    l.href = '/fonts/fonts.css';
    document.head.appendChild(l);
  }, [enabled]);
}

export function Seg({ value, options, onChange, label }) {
  return (
    <div className="seg" role="group" aria-label={label}>
      {options.map(([v, text]) => (
        <button key={v} type="button" aria-pressed={value === v} onClick={() => onChange(v)}>
          {text}
        </button>
      ))}
    </div>
  );
}

export default function Settings() {
  const { t, i18n } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const [s, setS] = useState(status.settings);
  const [shells] = useLoad(() => api.get('/shells'));
  const { busy, error, run } = useAction();
  const caps = status.capabilities;
  useTerminalFonts(caps.fonts);
  const set = (k) => (v) => setS({ ...s, [k]: v });
  const save = async (e) => {
    e.preventDefault();
    // 自动更新在“关于”里单独开关、立即保存，以当前状态为准，不用这个表单里打开页面时的旧值
    const r = await run(() => api.put('/settings', { ...s, font_size: Number(s.font_size), auto_update: status.settings.auto_update }));
    if (r) {
      setS(r);
      toast(t('common.saved'));
      reload();
    }
  };
  const dark = s.theme !== 'light';
  return (
    <>
      <div className="page-head">
        <h1>{t('settings.title')}</h1>
      </div>
      <form onSubmit={save}>
        <Card title={t('settings.terminal')}>
          <div
            className="font-preview"
            style={{ '--preview-font': fontFamily(s.font), '--term-bg': dark ? '#11131a' : '#fbfbfd', color: dark ? '#e3e6ee' : '#1f2330', fontSize: `${s.font_size}px` }}
          >
            {t('settings.preview')}
          </div>
          <Field label={t('settings.font')} hint={caps.fonts ? t('settings.font_hint') : t('settings.font_missing')}>
            <Seg
              value={s.font}
              label={t('settings.font')}
              onChange={set('font')}
              options={[
                ['wenkai', t('settings.font_wenkai')],
                ['system', t('settings.font_system')],
              ]}
            />
          </Field>
          <Field label={t('settings.font_size')}>
            <Seg value={Number(s.font_size)} label={t('settings.font_size')} onChange={set('font_size')} options={[13, 14, 15, 16, 18, 20].map((n) => [n, String(n)])} />
          </Field>
          <Field label={t('settings.theme')}>
            <Seg
              value={s.theme}
              label={t('settings.theme')}
              onChange={set('theme')}
              options={[
                ['dark', t('settings.theme_dark')],
                ['light', t('settings.theme_light')],
              ]}
            />
          </Field>
          <p className="muted small" style={{ margin: 0 }}>
            {t('settings.apply_note')}
          </p>
        </Card>
        <Card title={t('settings.general')}>
          <FolderField label={t('settings.workdir')} hint={t('settings.workdir_hint')} value={s.default_workdir} onChange={set('default_workdir')} />
          <ShellPicker value={s.default_shell ?? ''} onChange={set('default_shell')} data={shells} />
          <Field label={t('settings.device_name')} hint={t('settings.device_name_hint')}>
            <input value={s.device_name} onChange={(e) => set('device_name')(e.target.value)} maxLength={64} placeholder={t('settings.device_name_auto')} />
          </Field>
          <ErrorNote error={error} />
          <Button type="submit" busy={busy}>
            {t('action.save')}
          </Button>
        </Card>
      </form>
      <RelayPicker />
      <AccessCard />
      <DiagCard />
      <Card title={t('settings.language')}>
        <Seg
          value={i18n.language?.startsWith('en') ? 'en-US' : 'zh-CN'}
          label={t('settings.language')}
          onChange={setLocale}
          options={[
            ['zh-CN', '中文'],
            ['en-US', 'English'],
          ]}
        />
      </Card>
      <Card title={t('settings.about')}>
        <dl className="kv" style={{ marginBottom: 16 }}>
          <dt>{t('settings.version')}</dt>
          <dd className="mono">
            {status.version} · {status.target}
            {/* Windows 版（桌面应用）还在测试阶段 */}
            {status.target?.startsWith('windows') && <> <span className="badge badge-warn">{t('settings.beta')}</span></>}
          </dd>
          <dt>{t('settings.backend')}</dt>
          <dd>{t(caps.persistent ? 'settings.backend_persistent' : 'settings.backend_direct')}</dd>
          <dt>{t('settings.server')}</dt>
          <dd className="mono">{status.account.server}</dd>
          <dt>{t('settings.source')}</dt>
          <dd>
            <a href={SOURCE_URL} target="_blank" rel="noopener noreferrer">github.com/imythu/looklook</a> · MIT
          </dd>
        </dl>
        <UpdateCheck />
      </Card>
    </>
  );
}
