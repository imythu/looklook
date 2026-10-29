// 页面标题旁的“？”：打开看看网页上对应的使用说明（docs/ONBOARDING_2026-10.md §3.6）。
// 文档在服务端 server-web/src/docs，按页面（`/docs/{page}`）链接；在 App 里由系统浏览器打开（desktop.rs 拦截新窗口）。
import { CircleHelp } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { useStatus, webUrl } from '../App.jsx';

export function HelpLink({ page }) {
  const { t } = useTranslation();
  const { status } = useStatus();
  if (!status?.account?.server) return null;
  return (
    <a className="help-link" href={webUrl(status, `/docs/${page}`)} target="_blank" rel="noopener noreferrer" aria-label={t('tips.help')} title={t('tips.help')}>
      <CircleHelp size={18} aria-hidden="true" />
    </a>
  );
}
