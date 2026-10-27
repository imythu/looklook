// 第三方模型接口公用的小组件：免责声明、服务商的图标。配置本身在 ClaudeProfiles.jsx（Claude Code 账号、Codex 配置）。
// 平台推荐的服务商（含推广）由平台下发；Key 只保存在这台电脑上。第一次使用必须同意免责声明。
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

export function Disclaimer() {
  const { t } = useTranslation();
  return (
    <ul className="model-terms">
      {['third_party', 'data', 'key', 'cost'].map((k) => (
        <li key={k}>{t(`model.disclaimer.${k}`)}</li>
      ))}
    </ul>
  );
}

export function ProviderLogo({ p }) {
  const [broken, setBroken] = useState(false);
  if (!p.logo_url || broken) return <span className="model-logo">{(p.name || '?').slice(0, 1).toUpperCase()}</span>;
  return <img className="model-logo" src={p.logo_url} alt="" onError={() => setBroken(true)} />;
}
