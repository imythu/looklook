// 设置：按分区显示，一次只看一个分区（地址里的 #分区 决定）。
//   通用：电脑名称、语言、附件；终端：字体与配色、新终端的默认文件夹和 shell、新 AI 会话默认的权限；AI 模型：模型服务商；
//   网络与访问：远程线路、访问控制；关于：版本、检查更新、问题记录。
// 终端和通用里的选项改了就自动保存，不需要点保存按钮。
import { Bot, Check, Globe, Info, SlidersHorizontal, SquareTerminal } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { DeviceTag } from '../shared/brands.jsx';
import { LocalOnlyToggle, useDevices } from '../shared/devices.jsx';
import { api } from '../shared/api.js';
import AccessCard from './Access.jsx';
import { AttachmentsCard } from '../shared/Attachments.jsx';
import ClaudeProfilesCard from './ClaudeProfiles.jsx';
import ModelProvidersCard from './ModelProviders.jsx';
import { FolderField, ShellPicker } from './Terminals.jsx';
import { setLocale } from '../shared/i18n.js';
import { termTheme } from '../shared/termTheme.js';
import { DiagCard } from '../shared/Report.jsx';
import RelayPicker, { RELAYS_ANCHOR } from '../shared/RelayPicker.jsx';
import { useRouter } from '../shared/router.jsx';
import { shouldRemind, UpdateCheck } from '../shared/Update.jsx';
import { Card, ErrorNote, Field, useLoad } from '../shared/ui.jsx';

// 开源仓库（客户端 AGPL-3.0-or-later）
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

/** 终端配色：每个选项是一小块终端的样子（栏、提示符、光标、几种 ANSI 颜色） */
const SECTIONS = [
  ['general', SlidersHorizontal],
  ['terminal', SquareTerminal],
  ['ai', Bot],
  ['network', Globe],
  ['about', Info],
];
// 别处链接用的锚点（#model、#claude-accounts、#relays、#access）落到所在的分区
const ALIAS = { model: 'ai', 'claude-accounts': 'ai', [RELAYS_ANCHOR]: 'network', access: 'network' };

function sectionOf(hash) {
  const h = hash.replace(/^#/, '');
  const id = ALIAS[h] ?? h;
  return SECTIONS.some(([k]) => k === id) ? id : 'general';
}

// 自动保存的设置项
const KEYS = ['font', 'font_size', 'theme', 'default_workdir', 'default_shell', 'device_name', 'default_full_access'];
const same = (a, b) => KEYS.every((k) => String(a[k] ?? '') === String(b[k] ?? ''));

/** 改动停下 600 毫秒后保存；离开页面时把没保存的立即发出去。 */
function useAutoSave(status, reload) {
  const [s, setS] = useState(status.settings);
  const [state, setState] = useState({ phase: 'idle', error: null });
  const saved = useRef(status.settings);
  const latest = useRef({ s, status });
  latest.current = { s, status };

  const body = (v) => ({
    ...v,
    font_size: Number(v.font_size),
    // 自动更新、附件保留天数、终端配色（在终端页里换）在别处单独改、立即保存，以当前状态为准
    auto_update: latest.current.status.settings.auto_update,
    theme: latest.current.status.settings.theme,
    attach_days: latest.current.status.settings.attach_days,
  });

  useEffect(() => {
    if (same(s, saved.current)) return;
    const id = setTimeout(async () => {
      const sent = s;
      setState({ phase: 'saving', error: null });
      try {
        const r = await api.put('/settings', body(sent));
        saved.current = r;
        // 保存期间又改了的话，不覆盖正在输入的内容
        if (latest.current.s === sent) setS(r);
        setState({ phase: 'saved', error: null });
        reload();
      } catch (e) {
        setState({ phase: 'error', error: e });
      }
    }, 600);
    return () => clearTimeout(id);
  }, [s]);

  useEffect(
    () => () => {
      if (!same(latest.current.s, saved.current)) api.put('/settings', body(latest.current.s)).then(reload, () => {});
    },
    [],
  );

  return [s, setS, state];
}

function SaveState({ state }) {
  const { t } = useTranslation();
  if (state.phase === 'saving') return <span className="save-state">{t('settings.saving')}</span>;
  if (state.phase === 'saved')
    return (
      <span className="save-state ok">
        <Check size={14} aria-hidden="true" />
        {t('settings.saved')}
      </span>
    );
  return null;
}

export default function Settings() {
  const { t, i18n } = useTranslation();
  const { status, reload } = useStatus();
  const { hash, navigate } = useRouter();
  const devices = useDevices();
  const [s, setS, saveState] = useAutoSave(status, reload);
  const [shells] = useLoad(() => api.get('/shells'));
  const caps = status.capabilities;
  useTerminalFonts(caps.fonts);
  const set = (k) => (v) => setS({ ...s, [k]: v });
  const section = sectionOf(hash);
  const th = termTheme(s.theme);
  const updateDot = shouldRemind(status.update);

  const go = (id) => {
    navigate(`/settings#${id}`, { replace: true });
  };

  const sections = {
    general: (
      <>
        <Card title={t('settings.general')}>
          <Field label={t('settings.device_name')} hint={t('settings.device_name_hint')}>
            <input value={s.device_name} onChange={(e) => set('device_name')(e.target.value)} maxLength={64} placeholder={t('settings.device_name_auto')} />
          </Field>
          <Field label={t('settings.language')} hint={devices.multi ? t('settings.language_hint') : undefined}>
            <Seg
              value={i18n.language?.startsWith('en') ? 'en-US' : 'zh-CN'}
              label={t('settings.language')}
              onChange={setLocale}
              options={[
                ['zh-CN', '中文'],
                ['en-US', 'English'],
              ]}
            />
          </Field>
        </Card>
        <AttachmentsCard />
      </>
    ),
    terminal: (
      <>
        <Card title={t('settings.terminal')}>
          <div
            className="font-preview"
            style={{ '--preview-font': fontFamily(s.font), '--term-bg': th.bg, '--term-accent': th.accent, color: th.fg, fontSize: `${s.font_size}px` }}
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
        </Card>
        <Card title={t('settings.new_terminal')}>
          <FolderField label={t('settings.workdir')} hint={t('settings.workdir_hint')} value={s.default_workdir} onChange={set('default_workdir')} />
          <ShellPicker value={s.default_shell ?? ''} onChange={set('default_shell')} data={shells} />
          <Field label={t('settings.default_full_access')} hint={t('settings.default_full_access_hint')}>
            <Seg
              value={Boolean(s.default_full_access)}
              label={t('settings.default_full_access')}
              onChange={set('default_full_access')}
              options={[
                [false, t('settings.default_full_access_ask')],
                [true, t('settings.default_full_access_on')],
              ]}
            />
          </Field>
        </Card>
      </>
    ),
    ai: (
      <>
        <ClaudeProfilesCard />
        <ModelProvidersCard />
      </>
    ),
    network: (
      <>
        {(devices.localOnly || (devices.canLocalOnly && (!devices.multi || devices.selected === devices.def))) && (
          <Card title={t('devices.mine')}>
            <LocalOnlyToggle menu={false} />
          </Card>
        )}
        <RelayPicker />
        <div id="access" className="anchor">
          <AccessCard />
        </div>
      </>
    ),
    about: (
      <>
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
              <a href={SOURCE_URL} target="_blank" rel="noopener noreferrer">github.com/imythu/looklook</a> · AGPL-3.0
            </dd>
          </dl>
          <UpdateCheck />
        </Card>
        <DiagCard />
      </>
    ),
  };

  // 从别处带着 #access 跳过来时滚到访问控制
  useEffect(() => {
    if (hash === '#access') setTimeout(() => document.getElementById('access')?.scrollIntoView({ behavior: 'smooth', block: 'start' }), 50);
  }, [hash]);

  return (
    <>
      <div className="page-head">
        <h1>{t('settings.title')}</h1>
        {/* 多台电脑时说清楚改的是哪一台（语言除外，它只跟着浏览器） */}
        {devices.multi && (
          <span className="settings-scope small muted">
            {t('settings.scope')}
            <DeviceTag device={devices.byId(devices.selected)} />
          </span>
        )}
      </div>
      <div className="settings">
        <nav className="settings-nav" aria-label={t('settings.title')}>
          {SECTIONS.map(([id, Icon]) => (
            <a
              key={id}
              href={`/settings#${id}`}
              className={`settings-tab${section === id ? ' active' : ''}`}
              aria-current={section === id ? 'page' : undefined}
              onClick={(e) => {
                e.preventDefault();
                go(id);
              }}
            >
              <Icon size={18} aria-hidden="true" />
              <span>{t(`settings.sec_${id}`)}</span>
              {id === 'about' && updateDot && <span className="dot-badge" aria-label={t('update.badge')} />}
            </a>
          ))}
        </nav>
        <div className="settings-body">
          {(section === 'general' || section === 'terminal') && (
            <div className="settings-note">
              <span className="muted small">{t('settings.autosave')}</span>
              <SaveState state={saveState} />
            </div>
          )}
          {(section === 'general' || section === 'terminal') && <ErrorNote error={saveState.error} />}
          {sections[section]}
        </div>
      </div>
    </>
  );
}
