// macOS：一次给好常用文件夹的访问权限（客户端 api.rs 的 mac_folders）。
// 不然 AI 工具第一次碰到桌面、文稿、下载时才弹窗，命令卡在那里等人点。
// 读文件夹本身就会弹窗，所以只在点了按钮时去读；平时显示上次的结果。
import { CheckCircle2, FolderLock, ShieldCheck, XCircle } from 'lucide-react';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from './api.js';
import { errorText } from './i18n.js';
import { Button, Card, useLoad, useToast } from './ui.jsx';

export function MacFoldersCard() {
  const { t } = useTranslation();
  const toast = useToast();
  const [data, , reload] = useLoad(() => api.get('/mac/folders'));
  const [busy, setBusy] = useState(false);
  const folders = data?.folders ?? [];
  const denied = folders.some((f) => !f.ok);
  const grant = async () => {
    setBusy(true);
    try {
      const r = await api.post('/mac/folders');
      reload();
      if (r.folders?.every((f) => f.ok)) toast(t('macf.all_ok'), 'ok');
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setBusy(false);
    }
  };
  const open = async (pane) => {
    try {
      const r = await api.post('/mac/privacy', { pane });
      if (!r.opened) toast(t('macf.open_failed'), 'error');
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  return (
    <Card title={t('macf.title')}>
      <p className="muted small">{t('macf.intro')}</p>
      {folders.length > 0 && (
        <ul className="macf-list">
          {folders.map((f) => (
            <li key={f.key}>
              {f.ok ? <CheckCircle2 size={16} className="ok" aria-hidden="true" /> : <XCircle size={16} className="bad" aria-hidden="true" />}
              <span className="grow">{t(`macf.f_${f.key}`)}</span>
              <span className="small muted">{t(f.ok ? 'macf.allowed' : 'macf.denied')}</span>
            </li>
          ))}
        </ul>
      )}
      {denied && <p className="small muted">{t('macf.denied_hint')}</p>}
      <div className="inline">
        <Button icon={FolderLock} busy={busy} onClick={grant}>
          {t(folders.length ? 'macf.recheck' : 'macf.grant')}
        </Button>
        <Button variant="ghost" icon={ShieldCheck} onClick={() => open('full_disk')}>
          {t('macf.full_disk')}
        </Button>
        {denied && (
          <Button variant="ghost" onClick={() => open('files')}>
            {t('macf.files_pane')}
          </Button>
        )}
      </div>
    </Card>
  );
}
