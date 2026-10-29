// 管理台的“文件”页：浏览、预览（含 Markdown）、上传下载、新建文件夹、重命名、删除这台电脑上的文件。
// 多台电脑时按顶栏选中的那台（换了电脑就从那台的主目录重新开始）。
import { Lightbulb } from 'lucide-react';
import { useTranslation } from 'react-i18next';

import { selectedDevice } from '../shared/api.js';
import { HelpLink } from '../shared/help.jsx';
import { TipBar, useTip } from '../shared/tips.jsx';
import FileBrowser from './FileBrowser.jsx';

export default function Files() {
  const { t } = useTranslation();
  const device = selectedDevice();
  return (
    <>
      <div className="page-head">
        <h1 className="inline">
          {t('fb.page_title')} <HelpLink page="files" />
        </h1>
      </div>
      <FilesTip />
      <div className="files-page">
        <FileBrowser key={device ?? 'local'} />
      </div>
    </>
  );
}

/** 第一次来文件页时说一次能做什么。 */
function FilesTip() {
  const { t } = useTranslation();
  const [show, done] = useTip('files.page');
  if (!show) return null;
  return (
    <TipBar icon={Lightbulb} onDone={done} className="page-tip">
      <p>{t('tips.files_page')}</p>
    </TipBar>
  );
}
