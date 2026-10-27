// 办公文件预览用到的纯函数（文件种类、CSV 解析、列名），界面在 pages/OfficePreview.jsx（docs/PLAN_2026-10-27.md §6.1）。

/** 超过这个大小不取回来预览（整个文件要读进浏览器） */
export const OFFICE_MAX = 20 * 1024 * 1024;
export const MAX_ROWS = 500;

const KINDS = [
  ['pdf', /\.pdf$/i],
  ['docx', /\.docx$/i],
  ['xlsx', /\.xlsx$/i],
  ['csv', /\.(csv|tsv)$/i],
  ['other', /\.(doc|xls|xlsm|xlsb|ppt|pptx|odt|ods|odp|key|pages|numbers)$/i],
];

/** 'pdf' | 'docx' | 'xlsx' | 'csv' | 'other'（预览不了的办公文件）| null */
export function officeKind(name) {
  return KINDS.find(([, re]) => re.test(name))?.[0] ?? null;
}

/** 要取回整个文件再渲染的种类（CSV 用文本预览接口） */
export const WHOLE = ['pdf', 'docx', 'xlsx'];

/** CSV / TSV → 行（按 RFC 4180：引号里可以有分隔符、换行，两个引号表示一个引号）。分号分隔的（欧洲地区的 Excel 导出）按第一行猜 */
export function parseCsv(text, name) {
  const first = text.split('\n', 1)[0];
  const sep = /\.tsv$/i.test(name) ? '\t' : first.split(';').length > first.split(',').length ? ';' : ',';
  const rows = [];
  let row = [];
  let cell = '';
  let quoted = false;
  const endCell = () => {
    row.push(cell);
    cell = '';
  };
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (quoted) {
      if (c !== '"') cell += c;
      else if (text[i + 1] === '"') {
        cell += '"';
        i++;
      } else quoted = false;
    } else if (c === '"' && cell === '') {
      quoted = true;
    } else if (c === sep) {
      endCell();
    } else if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') i++;
      endCell();
      rows.push(row);
      row = [];
    } else {
      cell += c;
    }
  }
  if (cell !== '' || row.length) {
    endCell();
    rows.push(row);
  }
  return [{ name: '', rows: rows.slice(0, MAX_ROWS), total: rows.length }];
}

/** 列名 A、B、…、Z、AA… */
export function colName(n) {
  let s = '';
  for (n += 1; n > 0; n = Math.floor((n - 1) / 26)) s = String.fromCharCode(65 + ((n - 1) % 26)) + s;
  return s;
}
