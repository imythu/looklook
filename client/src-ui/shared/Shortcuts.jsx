// 快捷键说明（⌘/ / Ctrl+/，用户菜单里也能打开）：列出管理台和对话页的全部快捷键。别处发 `ll-shortcuts` 事件打开。
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { isMod, keyLabel } from './hotkeys.js';
import { Modal } from './ui.jsx';

const GROUPS = [
  ['app', [['mod+K', 'quick'], ['mod+shift+O', 'new'], ['mod+shift+S', 'pane'], ['mod+,', 'settings'], ['mod+/', 'help']]],
  ['chat', [['Enter', 'send'], ['shift+Enter', 'newline'], ['mod+F', 'find'], ['alt+↑ / alt+↓', 'jump'], ['↑ / ↓', 'history'], ['Ctrl+R', 'history_search'], ['Tab', 'suggest'], ['shift+Tab', 'mode'], ['Esc', 'stop']]],
];

const label = (combo) => combo.split(' / ').map((c) => keyLabel(c)).join(' / ');

export default function ShortcutsHelp() {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  useEffect(() => {
    const show = () => setOpen(true);
    const key = (e) => {
      if (isMod(e, '/')) {
        e.preventDefault();
        setOpen((o) => !o);
      }
    };
    window.addEventListener('keydown', key);
    window.addEventListener('ll-shortcuts', show);
    return () => {
      window.removeEventListener('keydown', key);
      window.removeEventListener('ll-shortcuts', show);
    };
  }, []);
  return (
    <Modal open={open} onClose={() => setOpen(false)} title={t('keys.title')} wide>
      <div className="keys-help">
        {GROUPS.map(([g, rows]) => (
          <section key={g}>
            <h3>{t(`keys.group.${g}`)}</h3>
            <dl>
              {rows.map(([k, what]) => (
                <div key={what} className="keys-row">
                  <dt>{t(`keys.${what}`)}</dt>
                  <dd>
                    <kbd>{label(k)}</kbd>
                  </dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </div>
      <div className="modal-actions">
        <button type="button" className="btn" onClick={() => setOpen(false)}>
          {t('action.close')}
        </button>
      </div>
    </Modal>
  );
}
