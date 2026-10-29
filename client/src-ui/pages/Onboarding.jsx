// 上手清单（docs/ONBOARDING_2026-10.md §3.2）：开始页草稿下面（手机上在会话列表上面）一张可收起的卡片，五步按真实状态自动打勾：
// 添加项目 → 开 AI 会话 → 在手机上打开 → 设好通知 → 看一眼任务。走过的步骤记在使用提示里（shared/tips.jsx 的 step.*），
// 之后条件不成立了（比如项目删了）也算做过。全部做完后剩一行“都会了”，关掉（onboard.hide）就不再出现；用户菜单“上手指南”能重新打开。
// OnboardTracker 挂在整个管理台里：在手机上打开过、打开过任务页，不管在哪一页都记下。
import { Bell, Check, ChevronDown, FolderPlus, ListTodo, MessageSquare, Smartphone, X } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from '../shared/api.js';
import { CHAT_LAUNCHES } from '../shared/chat.js';
import { isTouch } from '../shared/KeyBar.jsx';
import { platform } from '../shared/platform.js';
import { useRouter } from '../shared/router.jsx';
import { useSessions } from '../shared/sessions.jsx';
import { TipBar, useTip, useTips } from '../shared/tips.jsx';
import { Button } from '../shared/ui.jsx';
import { useSessionUi } from './Sessions.jsx';
import { RemoteCard } from './Terminals.jsx';
import { DiscoverDialog } from './Discover.jsx';

const STEPS = [
  { id: 'project', icon: FolderPlus },
  { id: 'ai', icon: MessageSquare },
  { id: 'phone', icon: Smartphone },
  { id: 'notify', icon: Bell },
  { id: 'tasks', icon: ListTodo },
];
const HIDE = 'onboard.hide';
const FOLD_KEY = 'looklook.onboard-fold';
const MOBILE_UA = /iPhone|iPad|Android|Mobile|HarmonyOS/i;

const notifyAllowed = () => typeof Notification !== 'undefined' && Notification.permission === 'granted';

/** 在手机上打开过、打开过任务页：在哪一页都记下。 */
export function OnboardTracker() {
  const tips = useTips();
  const { path } = useRouter();
  useEffect(() => {
    if (!tips.ready) return;
    if (isTouch()) tips.mark('step.phone');
    if (path === '/tasks') tips.mark('step.tasks');
    if (notifyAllowed()) tips.mark('step.notify');
  }, [tips.ready, path]);
  return null;
}

/** 清单的状态：哪几步做过了、要不要显示。 */
function useGuide() {
  const tips = useTips();
  const s = useSessions();
  const { status } = useStatus();
  const loggedIn = status.account.logged_in;
  const hidden = !tips.ready || tips.has(HIDE);
  const done = Object.fromEntries(STEPS.map(({ id }) => [id, tips.has(`step.${id}`)]));
  const hasProject = s.projects.length > 0;
  const hasAi = s.items.some((i) => CHAT_LAUNCHES.includes(i.launch) || i.launch === 'opencode');
  useEffect(() => {
    if (hidden) return;
    const ids = [hasProject && !done.project && 'step.project', hasAi && !done.ai && 'step.ai'].filter(Boolean);
    if (ids.length) tips.mark(...ids);
  }, [hidden, hasProject, hasAi, done.project, done.ai]);
  // 手机：平台上登录过的浏览器里有手机就算；通知：配了飞书或别的渠道就算。只在还没做的时候查一次
  useEffect(() => {
    if (hidden || done.phone || !loggedIn) return;
    platform
      .get('/sessions')
      .then((r) => r?.items?.some((x) => MOBILE_UA.test(x.user_agent ?? '')) && tips.mark('step.phone'))
      .catch(() => {});
  }, [hidden, done.phone, loggedIn]);
  useEffect(() => {
    if (hidden || done.notify) return;
    api
      .get('/notify')
      .then((r) => (r?.feishu || r?.hooks?.length > 0) && tips.mark('step.notify'))
      .catch(() => {});
  }, [hidden, done.notify]);
  return { hidden, done, count: STEPS.filter(({ id }) => done[id]).length, hide: () => tips.mark(HIDE) };
}

function readFold() {
  try {
    return localStorage.getItem(FOLD_KEY) === '1';
  } catch {
    return false;
  }
}

/**
 * 上手清单卡片。`onTry(text)`：第二步“试试”，把一句常用开头填进开始页的输入框（手机的会话列表上没有输入框，就去新会话）。
 * `remote`：第三步展开时显示专属地址和二维码（开始页上同一张卡片就不再单独显示）。
 */
export function Onboarding({ onTry }) {
  const { t } = useTranslation();
  const g = useGuide();
  const ui = useSessionUi();
  const { navigate } = useRouter();
  const [fold, setFold] = useState(readFold);
  const [qr, setQr] = useState(false);
  const [discover, setDiscover] = useState(false);
  if (g.hidden) return null;
  const toggle = () => {
    setFold(!fold);
    try {
      localStorage.setItem(FOLD_KEY, fold ? '0' : '1');
    } catch {
      /* 只是不记住 */
    }
  };
  const all = g.count === STEPS.length;
  if (all) {
    return (
      <div className="onboard onboard-done">
        <Check size={16} aria-hidden="true" />
        <span className="grow">{t('onboard.all_done')}</span>
        <button type="button" className="btn btn-ghost btn-sm" onClick={() => setDiscover(true)}>
          {t('discover.open')}
        </button>
        <button type="button" className="btn btn-plain btn-icon" onClick={g.hide} aria-label={t('onboard.hide')} title={t('onboard.hide')}>
          <X size={15} />
        </button>
        <DiscoverDialog open={discover} onClose={() => setDiscover(false)} />
      </div>
    );
  }
  const next = STEPS.find(({ id }) => !g.done[id])?.id;
  const actions = {
    project: { label: t('onboard.project.go'), onClick: () => ui.addProject() },
    ai: { label: t('onboard.ai.go'), onClick: () => (onTry ? onTry(t('onboard.ai.sample')) : navigate('/new')) },
    phone: { label: t(qr ? 'onboard.phone.hide' : 'onboard.phone.go'), onClick: () => setQr(!qr) },
    notify: { label: t('onboard.notify.go'), onClick: () => navigate('/settings#notify') },
    tasks: { label: t('onboard.tasks.go'), onClick: () => navigate('/tasks') },
  };
  return (
    <section className="onboard" aria-label={t('onboard.title')}>
      <button type="button" className="onboard-head" onClick={toggle} aria-expanded={!fold}>
        <b className="grow">{t('onboard.title')}</b>
        <span className="onboard-count">
          {g.count}/{STEPS.length}
        </span>
        <ChevronDown size={16} className={`onboard-chev ${fold ? '' : 'open'}`} aria-hidden="true" />
      </button>
      {!fold && (
        <>
          <p className="onboard-lead">{t('onboard.lead')}</p>
          <ol className="onboard-steps">
            {STEPS.map(({ id, icon: Icon }) => {
              const done = g.done[id];
              const a = actions[id];
              return (
                <li key={id} className={`onboard-step ${done ? 'done' : ''} ${id === next ? 'next' : ''}`}>
                  <span className="onboard-mark" aria-hidden="true">
                    {done ? <Check size={14} /> : <Icon size={14} />}
                  </span>
                  <span className="grow onboard-text">
                    <b>{t(`onboard.${id}.title`)}</b>
                    {id === next && <span className="small muted">{t(`onboard.${id}.body`)}</span>}
                  </span>
                  {!done && (
                    <Button variant={id === next ? 'primary' : 'ghost'} className="btn-sm" onClick={a.onClick}>
                      {a.label}
                    </Button>
                  )}
                  {id === 'phone' && qr && !done && (
                    <div className="onboard-extra">
                      <RemoteCard initialOpen />
                    </div>
                  )}
                </li>
              );
            })}
          </ol>
          <div className="onboard-foot">
            <button type="button" className="btn btn-plain btn-sm onboard-more" onClick={() => setDiscover(true)}>
              {t('discover.open')}
            </button>
            <button type="button" className="btn btn-plain btn-sm" onClick={g.hide}>
              {t('onboard.skip')}
            </button>
          </div>
          <DiscoverDialog open={discover} onClose={() => setDiscover(false)} />
        </>
      )}
    </section>
  );
}

const standalone = () => window.matchMedia?.('(display-mode: standalone)').matches || window.navigator.standalone === true;

/** 手机浏览器里打开时说一次怎么加到主屏幕（已经从主屏幕打开的不说）。 */
export function HomeScreenTip() {
  const { t } = useTranslation();
  const [show, done] = useTip('mobile.homescreen', isTouch() && !standalone(), 1);
  if (!show) return null;
  const ios = /iPhone|iPad/i.test(navigator.userAgent);
  return (
    <TipBar icon={Smartphone} onDone={done} className="slist-tip">
      <p>{t(ios ? 'tips.homescreen.ios' : 'tips.homescreen.android')}</p>
    </TipBar>
  );
}

/** 开始页上单独的专属地址卡片：清单在、第三步还没做时由清单显示，这里不重复。 */
export function useGuideShowsRemote() {
  const tips = useTips();
  return tips.ready && !tips.has(HIDE) && !tips.has('step.phone');
}
