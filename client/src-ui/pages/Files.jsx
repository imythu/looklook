// 管理台的“文件”页：浏览、预览（含 Markdown）、上传下载、新建文件夹、重命名、删除这台电脑上的文件。
// 多台电脑时按顶栏选中的那台（换了电脑就从那台的主目录重新开始）。
import { useTranslation } from 'react-i18next';

import { selectedDevice } from '../shared/api.js';
import FileBrowser from './FileBrowser.jsx';

export default function Files() {
  const { t } = useTranslation();
  const device = selectedDevice();
  return (
    <>
      <div className="page-head">
        <h1>{t('fb.page_title')}</h1>
      </div>
      <div className="files-page">
        <FileBrowser key={device ?? 'local'} />
      </div>
    </>
  );
}
