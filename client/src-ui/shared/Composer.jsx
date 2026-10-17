// 输入框：在手机上往终端里打长提示词很难受（一个字一个字进终端、改错要按方向键）。这里先在普通的多行
// 输入框里写好（输入法、联想、语音输入都照常用），再整段发进终端：走终端的粘贴（AI 编程助手按粘贴处理，
// 多行不会被当成好几次回车），然后按需补一个回车提交。草稿按终端存在这个浏览器里。
//
// 语音：浏览器自带的语音识别（Web Speech API）。Chrome 里它要连 Google 的服务，国内多半用不了；
// iOS Safari 用苹果的服务。不支持或连不上时提示改用输入法自带的语音键——那个在输入框里本来就能用。
import { Mic, MicOff, SendHorizontal, X } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { termFocus, termPaste, termSend } from './direct.js';
import { useToast } from './ui.jsx';

const DRAFT_KEY = 'looklook.draft.';
const SUBMIT_KEY = 'looklook.composer.submit';
// 粘贴之后等多久再按回车：太快的话 Codex 会把回车当成粘贴的一部分吞掉，内容停在输入框里没发出去。
// Windows 上粘贴的内容是一个个字符经过 ConPTY 送进去的，越长越慢，所以按长度多等一会儿。
const submitDelay = (text) => Math.min(2500, 500 + text.length * 5);

function readLocal(key, fallback) {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v;
  } catch {
    return fallback;
  }
}
function writeLocal(key, value) {
  try {
    if (value === null || value === '') localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* 无痕模式等：只是不保存草稿 */
  }
}

const Recognition = typeof window !== 'undefined' ? window.SpeechRecognition || window.webkitSpeechRecognition : undefined;

/** 浏览器语音识别：说完一句就追加到输入框。 */
function useDictation(onText) {
  const { i18n, t } = useTranslation();
  const toast = useToast();
  const rec = useRef(null);
  const [on, setOn] = useState(false);
  useEffect(() => () => rec.current?.abort?.(), []);
  if (!Recognition) return { supported: false };
  const stop = () => rec.current?.stop?.();
  const start = () => {
    if (rec.current) return;
    const r = new Recognition();
    r.lang = i18n.language?.startsWith('en') ? 'en-US' : 'zh-CN';
    r.continuous = true;
    r.interimResults = false;
    r.onresult = (e) => {
      for (let i = e.resultIndex; i < e.results.length; i++) {
        if (e.results[i].isFinal) onText(e.results[i][0].transcript);
      }
    };
    r.onerror = (e) => {
      if (e.error === 'no-speech' || e.error === 'aborted') return;
      toast(t(e.error === 'not-allowed' || e.error === 'service-not-allowed' ? 'composer.voice_denied' : 'composer.voice_failed'), 'error');
    };
    r.onend = () => {
      rec.current = null;
      setOn(false);
    };
    rec.current = r;
    setOn(true);
    try {
      r.start();
    } catch {
      rec.current = null;
      setOn(false);
    }
  };
  return { supported: true, on, toggle: () => (on ? stop() : start()) };
}

/**
 * 终端下方的输入框。`frame`：终端 iframe；`id`：终端 id（草稿按它保存）；`touch`：手机上发完不抢焦点（免得弹出终端的键盘）。
 */
export default function Composer({ frame, id, touch, onClose }) {
  const { t } = useTranslation();
  const [text, setText] = useState(() => readLocal(DRAFT_KEY + id, ''));
  const [submit, setSubmit] = useState(() => readLocal(SUBMIT_KEY, '1') === '1');
  const area = useRef(null);
  useEffect(() => writeLocal(DRAFT_KEY + id, text), [id, text]);
  useEffect(() => {
    if (!touch) area.current?.focus();
  }, []);
  // 按内容长高，最多占屏幕高度的四成
  useEffect(() => {
    const el = area.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [text]);
  const append = (s) => setText((cur) => (cur && !/\s$/.test(cur) ? `${cur} ${s.trim()}` : `${cur}${s.trim()}`));
  const voice = useDictation(append);
  const send = () => {
    const body = text.replace(/\s+$/, '');
    if (!body) return;
    termPaste(frame, body, { focus: !touch });
    // 粘贴和回车走同一条通道、按顺序到达；稍等一下，让程序先处理完粘贴
    if (submit) setTimeout(() => termSend(frame, '\r', { focus: !touch }), submitDelay(body));
    setText('');
    if (!touch) termFocus(frame);
  };
  const toggleSubmit = (v) => {
    setSubmit(v);
    writeLocal(SUBMIT_KEY, v ? '1' : '0');
  };
  return (
    <div className="composer" role="group" aria-label={t('composer.label')}>
      <textarea
        ref={area}
        value={text}
        rows={2}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            send();
          } else if (e.key === 'Escape') {
            onClose?.();
          }
        }}
        placeholder={t(touch ? 'composer.placeholder_touch' : 'composer.placeholder')}
        aria-label={t('composer.label')}
        enterKeyHint="enter"
      />
      <div className="composer-bar">
        <label className="composer-submit" title={t('composer.submit_hint')}>
          <input type="checkbox" checked={submit} onChange={(e) => toggleSubmit(e.target.checked)} />
          {t('composer.submit')}
        </label>
        <span className="grow" />
        {voice.supported && (
          <button
            type="button"
            className={`btn btn-plain btn-icon ${voice.on ? 'composer-rec' : ''}`}
            onClick={voice.toggle}
            aria-pressed={voice.on}
            aria-label={t(voice.on ? 'composer.voice_stop' : 'composer.voice')}
            title={t(voice.on ? 'composer.voice_stop' : 'composer.voice_hint')}
          >
            {voice.on ? <MicOff size={18} /> : <Mic size={18} />}
          </button>
        )}
        {onClose && (
          <button type="button" className="btn btn-plain btn-icon" onClick={onClose} aria-label={t('composer.close')} title={t('composer.close')}>
            <X size={18} />
          </button>
        )}
        <button type="button" className="btn btn-primary composer-send" onClick={send} disabled={!text.trim()} title={t('composer.send_hint')}>
          <SendHorizontal size={16} aria-hidden="true" />
          {t('composer.send')}
        </button>
      </div>
    </div>
  );
}
