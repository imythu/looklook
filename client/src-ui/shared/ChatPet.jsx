// 对话界面右下角的小胖鱼：跟着对话状态动（在跑摆尾巴冒泡、等你时举鳍、做完翻跟头、闲久了睡觉），像素见 pet.js。
// 点它会蹦一下；鼠标移上去有个小叉可以藏起来，藏起来后留一个很淡的小点，点了叫回来（记在这个浏览器里）。
import { useEffect, useMemo, useReducer, useRef, useState } from 'react';
import { X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { PET_FRAMES, PET_H, PET_W, petMood, petPixels } from './pet.js';

const PET_KEY = 'll-chat-pet';

function readHidden() {
  try {
    return localStorage.getItem(PET_KEY) === 'off';
  } catch {
    return false;
  }
}

export default function ChatPet({ phase }) {
  const { t } = useTranslation();
  const [hidden, setHidden] = useState(readHidden);
  const [boop, setBoop] = useState(0);
  // 什么时候做完的、从什么时候开始闲着：决定 done / sleep
  const prev = useRef(phase);
  const times = useRef({ doneAt: 0, idleSince: Date.now() });
  if (prev.current !== phase) {
    if (phase === 'idle') times.current = { doneAt: prev.current === 'running' || prev.current === 'approval' ? Date.now() : 0, idleSince: Date.now() };
    prev.current = phase;
  }
  const [now, tick] = useReducer(() => Date.now(), Date.now());
  const mood = petMood(phase, { ...times.current, now });
  const [count, every] = PET_FRAMES[mood ?? 'idle'];
  const [frame, setFrame] = useState(0);
  const still = useMemo(() => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches, []);
  useEffect(() => {
    if (!mood || hidden) return undefined;
    const iv = setInterval(() => {
      if (!still) setFrame((f) => (f + 1) % count);
      tick();
    }, every);
    return () => clearInterval(iv);
  }, [mood, hidden, count, every, still]);
  const pixels = useMemo(() => (mood ? petPixels(mood, frame) : []), [mood, frame]);
  const toggle = (v) => {
    setHidden(v);
    try {
      if (v) localStorage.setItem(PET_KEY, 'off');
      else localStorage.removeItem(PET_KEY);
    } catch {
      // 存不了（隐私模式）：这次照样生效
    }
  };
  if (!mood) return null;
  if (hidden) {
    return (
      <div className="chat-pet-dock">
        <button type="button" className="chat-pet-peek" onClick={() => toggle(false)} title={t('chat.pet_show')} aria-label={t('chat.pet_show')} />
      </div>
    );
  }
  const label = t(`chat.pet_${{ work: 'working', wait: 'waiting', done: 'done', sleep: 'sleeping', idle: 'sleeping' }[mood]}`);
  return (
    <div className="chat-pet-dock">
      <div className="chat-pet" data-mood={mood} key={mood === 'done' ? 'done' : 'pet'}>
        {/* 换 key 让蹦一下的动画每次点都重新播 */}
        <button type="button" className="chat-pet-body" key={boop} data-boop={boop ? '' : undefined} onClick={() => setBoop((n) => n + 1)} title={mood === 'idle' ? undefined : label} aria-label={label}>
          <svg viewBox={`0 0 ${PET_W} ${PET_H}`} width={PET_W * 3} height={PET_H * 3} shapeRendering="crispEdges" aria-hidden="true">
            {pixels.map(([x, y, c]) => (
              <rect key={`${x}.${y}`} x={x} y={y} width="1.02" height="1.02" className={`pet-${c}`} />
            ))}
          </svg>
          {mood === 'wait' && <span className="chat-pet-mark">!</span>}
          {mood === 'sleep' && <span className="chat-pet-z">z</span>}
          {mood === 'work' && (
            <span className="chat-pet-bubbles">
              <i />
              <i />
              <i />
            </span>
          )}
        </button>
        <button type="button" className="chat-pet-hide" onClick={() => toggle(true)} title={t('chat.pet_hide')} aria-label={t('chat.pet_hide')}>
          <X size={11} />
        </button>
      </div>
    </div>
  );
}
