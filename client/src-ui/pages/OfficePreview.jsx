// 文件栏的办公文件预览（docs/PLAN_2026-10-27.md §6.1）：PDF 用浏览器自带的查看器，Word（.docx）经 mammoth 转成 HTML
// 再用 DOMPurify 清洗，Excel（.xlsx）用 read-excel-file 读成表格，CSV / TSV 自己解析。PowerPoint 和旧格式没有轻量的库，
// 提示下载后打开。两个库都在用到时才加载（动态 import），不进首屏。表格只显示前 MAX_ROWS 行。
import DOMPurify from 'dompurify';
import { useState } from 'react';
import { useTranslation } from 'react-i18next';

import { colName, MAX_ROWS } from '../shared/office.js';

/** 把取回的文件转成要显示的内容：{ url }（PDF，用完要 revoke）、{ html }、{ sheets: [{ name, rows, total }] } */
export async function renderOffice(kind, blob) {
  if (kind === 'pdf') return { url: URL.createObjectURL(new Blob([blob], { type: 'application/pdf' })) };
  if (kind === 'docx') {
    const { default: mammoth } = await import('mammoth');
    const { value } = await mammoth.convertToHtml({ arrayBuffer: await blob.arrayBuffer() });
    return { html: DOMPurify.sanitize(value) };
  }
  const { default: readXlsxFile } = await import('read-excel-file/browser');
  const sheets = await readXlsxFile(blob);
  return { sheets: sheets.map(({ sheet, data }) => ({ name: sheet, rows: data.slice(0, MAX_ROWS), total: data.length })) };
}

function cellText(v) {
  if (v == null) return '';
  if (v instanceof Date) return v.getUTCHours() || v.getUTCMinutes() ? v.toLocaleString() : v.toLocaleDateString();
  return String(v);
}

/** 表格：像电子表格那样带列名、行号；多个工作表时上面一排标签 */
export function SheetView({ sheets }) {
  const { t } = useTranslation();
  const [n, setN] = useState(0);
  const sheet = sheets[Math.min(n, sheets.length - 1)];
  const cols = sheet.rows.reduce((m, r) => Math.max(m, r.length), 0);
  return (
    <>
      {sheets.length > 1 && (
        <div className="fb-sheets" role="tablist">
          {sheets.map((s, i) => (
            <button key={s.name} type="button" role="tab" aria-selected={i === n} className={i === n ? 'on' : ''} onClick={() => setN(i)}>
              {s.name}
            </button>
          ))}
        </div>
      )}
      {sheet.total > sheet.rows.length && <p className="muted small fb-pad fb-note">{t('fb.rows_truncated', { n: sheet.rows.length, total: sheet.total })}</p>}
      {sheet.rows.length === 0 ? (
        <p className="muted small fb-pad">{t('fb.sheet_empty')}</p>
      ) : (
        <table className="fb-sheet">
          <thead>
            <tr>
              <th />
              {Array.from({ length: cols }, (_, c) => (
                <th key={c}>{colName(c)}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {sheet.rows.map((r, i) => (
              // eslint-disable-next-line react/no-array-index-key -- 行没有别的标识，内容不会重排
              <tr key={i}>
                <th>{i + 1}</th>
                {Array.from({ length: cols }, (_, c) => (
                  <td key={c}>{cellText(r[c])}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </>
  );
}
