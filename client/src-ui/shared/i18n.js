// 国际化：zh-CN 为基准；en-US 与之 key 相同，缺的值（空字符串）回退中文。
import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';

import zhCommon from '../locales/zh-CN/common.json';
import zhError from '../locales/zh-CN/error.json';
import enCommon from '../locales/en-US/common.json';
import enError from '../locales/en-US/error.json';

const LOCALE_KEY = 'll_locale';

function browserLocale() {
  return (navigator.language || '').toLowerCase().startsWith('en') ? 'en-US' : 'zh-CN';
}

export function savedLocale() {
  try {
    return localStorage.getItem(LOCALE_KEY) || browserLocale();
  } catch {
    return browserLocale();
  }
}

export function setLocale(l) {
  try {
    localStorage.setItem(LOCALE_KEY, l);
  } catch {
    /* 隐私模式等场景忽略 */
  }
  i18n.changeLanguage(l);
  document.documentElement.lang = l;
}

i18n.use(initReactI18next).init({
  resources: {
    'zh-CN': { common: zhCommon, error: zhError },
    'en-US': { common: enCommon, error: enError },
  },
  lng: savedLocale(),
  fallbackLng: 'zh-CN',
  ns: ['common', 'error'],
  defaultNS: 'common',
  returnEmptyString: false,
  interpolation: { escapeValue: false },
});

/** 错误码 → 文案（`error.<CODE>`，params 用于插值）。 */
export function errorText(t, e) {
  if (!e) return '';
  const code = e.code ?? 'INTERNAL';
  const params = { ...(e.params ?? {}) };
  params.rule_text = params.rule ? t(`error:rule.${params.rule}`, { ...params, defaultValue: params.rule }) : '';
  params.field_text = params.field ? t(`error:field.${params.field}`, { defaultValue: params.field }) : '';
  params.reason_text = params.reason ? t(`error:reason.${params.reason}`, { defaultValue: params.reason }) : '';
  return t(`error:${code}`, { ...params, defaultValue: t('error:INTERNAL') });
}

document.documentElement.lang = i18n.language;

export default i18n;
