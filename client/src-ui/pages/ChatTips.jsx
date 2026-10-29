// 对话页的教学（docs/ONBOARDING_2026-10.md §3.4）：
// - ChatHowTo：空白对话下面一行“能这样用”，每次换一条，点“换一条”看下一条；
// - NotifyAsk：浏览器通知不在第一次发消息时就申请，等第一次需要你确认、或者一轮跑了一分钟以上做完时，
//   在输入框上面说一句为什么要开，用户点了才申请；已经被拒的说一次怎么打开，并推荐飞书等渠道。
import { Bell, BellOff, Lightbulb, RefreshCw } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { keyLabel } from '../shared/hotkeys.js';
import { useRouter } from '../shared/router.jsx';
import { TipBar, useTip } from '../shared/tips.jsx';

// 哪些能这样用：`only` 限定工具，`desk` 只在电脑上
const HOWTO = [
  { id: 'slash' },
  { id: 'paste', desk: true },
  { id: 'photo', touch: true },
  { id: 'mode', only: ['claude'], desk: true },
  { id: 'resume' },
  { id: 'history', desk: true },
  { id: 'find', desk: true },
  { id: 'longpress', touch: true },
];

export function ChatHowTo({ agent, touch }) {
  const { t } = useTranslation();
  const list = useMemo(() => HOWTO.filter((h) => (!h.only || h.only.includes(agent)) && !(h.desk && touch) && !(h.touch && !touch)), [agent, touch]);
  const [n, setN] = useState(() => Math.floor(Math.random() * list.length));
  if (!list.length) return null;
  const h = list[n % list.length];
  return (
    <p className="chat-howto">
      <Lightbulb size={14} aria-hidden="true" />
      <span className="grow">{t(`tips.howto.${h.id}`, { find: keyLabel('mod+F') })}</span>
      {list.length > 1 && (
        <button type="button" className="btn btn-plain btn-sm" onClick={() => setN(n + 1)} title={t('tips.howto.next')} aria-label={t('tips.howto.next')}>
          <RefreshCw size={13} />
        </button>
      )}
    </p>
  );
}

const LONG_RUN_MS = 60_000;
const canNotify = () => typeof Notification !== 'undefined';

/** `active`：对话页正显示着。`phase`：对话的状态（running / approval / tui / idle …）。 */
export function NotifyAsk({ active, phase }) {
  const { t } = useTranslation();
  const { navigate } = useRouter();
  const [moment, setMoment] = useState(false);
  const [perm, setPerm] = useState(() => (canNotify() ? Notification.permission : 'unsupported'));
  const started = useRef(null);
  useEffect(() => {
    if (phase === 'approval' || phase === 'tui') setMoment(true);
    if (phase === 'running') started.current ??= Date.now();
    else if (phase === 'idle' && started.current) {
      if (Date.now() - started.current > LONG_RUN_MS) setMoment(true);
      started.current = null;
    }
  }, [phase]);
  const [ask, askDone] = useTip('chat.notify', active && moment && perm === 'default', 2);
  const [denied, deniedDone] = useTip('chat.notify_denied', active && moment && perm === 'denied', 2);
  if (ask) {
    const allow = async () => {
      try {
        setPerm(await Notification.requestPermission());
      } catch {
        /* 老的 Safari 只认回调写法；拿不到就算了 */
      }
      askDone();
    };
    return (
      <TipBar icon={Bell} onDone={askDone} action={{ label: t('tips.notify.allow'), onClick: allow }} className="chat-tip">
        <p>{t('tips.notify.body')}</p>
      </TipBar>
    );
  }
  if (denied) {
    return (
      <TipBar icon={BellOff} onDone={deniedDone} action={{ label: t('tips.notify.channels'), onClick: () => navigate('/settings#notify') }} className="chat-tip">
        <p>{t('tips.notify.denied')}</p>
      </TipBar>
    );
  }
  return null;
}
