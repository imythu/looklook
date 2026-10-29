// 发现更多功能（docs/ONBOARDING_2026-10.md §3.5）：
// - DraftTip：开始页输入框下面的一行小技巧，每次打开换一条（电脑上的跟手机上的分开）；
// - DiscoverDialog：“看看还能做什么”，一张功能清单，每项一句话 + 去那里的按钮。上手清单做完后、用户菜单“上手指南”里都能打开。
import { Bot, CalendarClock, Container, FolderOpen, Globe, History, Keyboard, Laptop, ListTodo, RefreshCw, Server, Settings as SettingsIcon, UserCog } from 'lucide-react';
import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { keyLabel } from '../shared/hotkeys.js';
import { isTouch } from '../shared/KeyBar.jsx';
import { useRouter } from '../shared/router.jsx';
import { Modal } from '../shared/ui.jsx';

// 小技巧：`desk` 只在电脑上，`touch` 只在手机上
const DRAFT_TIPS = [
  { id: 'quick', desk: true },
  { id: 'keys', desk: true },
  { id: 'templates' },
  { id: 'more' },
  { id: 'history' },
  { id: 'multi' },
  { id: 'pull', touch: true },
  { id: 'keybar', touch: true },
  { id: 'landscape', touch: true },
];

/** 开始页输入框下面：按键说明（电脑上）+ 一条小技巧。 */
export function DraftTip() {
  const { t } = useTranslation();
  const touch = isTouch();
  const list = useMemo(() => DRAFT_TIPS.filter((x) => !(x.desk && touch) && !(x.touch && !touch)), [touch]);
  const [n, setN] = useState(() => Math.floor(Math.random() * list.length));
  const tip = list[n % list.length];
  return (
    <p className="draft-hint">
      {!touch && <span className="draft-hint-keys">{t('draft.keys')}</span>}
      <span className="draft-tip">
        {t(`discover.tip.${tip.id}`, { quick: keyLabel('mod+K'), keys: keyLabel('mod+/') })}
        <button type="button" className="btn btn-plain" onClick={() => setN(n + 1)} aria-label={t('tips.howto.next')} title={t('tips.howto.next')}>
          <RefreshCw size={12} />
        </button>
      </span>
    </p>
  );
}

const FEATURES = [
  { id: 'files', icon: FolderOpen, to: '/files' },
  { id: 'ssh', icon: Server, to: '/ssh' },
  { id: 'pages', icon: Globe, to: '/pages' },
  { id: 'ai', icon: Bot, to: '/ai' },
  { id: 'accounts', icon: UserCog, to: '/settings#claude-accounts' },
  { id: 'history', icon: History, to: '/history' },
  { id: 'tasks', icon: ListTodo, to: '/tasks' },
  { id: 'docker', icon: Container, to: '/new' },
  { id: 'computers', icon: Laptop, to: '/devices' },
  { id: 'sync', icon: SettingsIcon, to: '/settings' },
  { id: 'membership', icon: CalendarClock, to: '/membership' },
  { id: 'shortcuts', icon: Keyboard, desk: true, event: 'll-shortcuts' },
];

export function DiscoverDialog({ open, onClose }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const touch = isTouch();
  const go = (f) => {
    onClose();
    if (f.event) window.dispatchEvent(new Event(f.event));
    else navigate(f.to);
  };
  return (
    <Modal open={open} onClose={onClose} title={t('discover.title')} wide>
      <p className="muted small discover-lead">{t('discover.lead')}</p>
      <ul className="discover-list">
        {FEATURES.filter((f) => !(f.desk && touch)).map((f) => (
          <li key={f.id}>
            <button type="button" className="discover-item" onClick={() => go(f)}>
              <span className="discover-icon" aria-hidden="true">
                <f.icon size={18} />
              </span>
              <span className="discover-text">
                <b>{t(`discover.f.${f.id}.title`)}</b>
                <span className="small muted">{t(`discover.f.${f.id}.body`)}</span>
              </span>
            </button>
          </li>
        ))}
      </ul>
    </Modal>
  );
}
