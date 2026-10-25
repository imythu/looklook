// 终端配色的选择：设置页和终端页（顶栏的配色按钮）共用。
import { Check } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { errorText } from './i18n.js';
import { TERM_THEMES } from './termTheme.js';
import { Modal, useToast } from './ui.jsx';

/** 一排配色小样，点一下选中。 */
export function ThemePicker({ value, onChange }) {
  const { t } = useTranslation();
  return (
    <div className="theme-pick" role="group" aria-label={t('settings.theme')}>
      {Object.entries(TERM_THEMES).map(([id, th]) => (
        <button key={id} type="button" aria-pressed={value === id} onClick={() => onChange(id)} title={t(`settings.theme_${id}`)}>
          <span className="theme-sample" style={{ background: th.bg, color: th.fg, borderColor: th.border }} aria-hidden="true">
            <span className="theme-sample-bar" style={{ background: th.bar, borderColor: th.border }} />
            <span className="theme-sample-line">
              <b style={{ color: th.ansi[1] }}>~</b> $ <i style={{ background: th.accent }} />
            </span>
            <span className="theme-sample-dots">
              {th.ansi.map((c) => (
                <i key={c} style={{ background: c }} />
              ))}
            </span>
          </span>
          <span className="theme-name">
            {value === id && <Check size={14} aria-hidden="true" />}
            {t(`settings.theme_${id}`)}
          </span>
        </button>
      ))}
    </div>
  );
}

/** 终端页里换配色：马上保存到设置，所有开着的终端（useTermTheme）跟着换。 */
export function TermThemeModal({ open, onClose }) {
  const { t } = useTranslation();
  const { status, reload } = useStatus();
  const toast = useToast();
  const pick = async (theme) => {
    if (theme === status.settings.theme) return;
    try {
      await api.put('/settings', { ...status.settings, theme });
      reload();
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  return (
    <Modal open={open} onClose={onClose} title={t('term.theme')}>
      <p className="muted small">{t('term.theme_note')}</p>
      <ThemePicker value={status.settings.theme} onChange={pick} />
    </Modal>
  );
}
