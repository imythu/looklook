import assert from 'node:assert/strict';
import { test } from 'node:test';

import { colName, officeKind, parseCsv } from './office.js';

test('officeKind', () => {
  assert.equal(officeKind('a.PDF'), 'pdf');
  assert.equal(officeKind('报告.docx'), 'docx');
  assert.equal(officeKind('x.xlsx'), 'xlsx');
  assert.equal(officeKind('x.tsv'), 'csv');
  assert.equal(officeKind('slides.pptx'), 'other');
  assert.equal(officeKind('old.doc'), 'other');
  assert.equal(officeKind('main.rs'), null);
});

test('parseCsv quotes, newlines and separators', () => {
  const [s] = parseCsv('a,b,c\r\n1,"x, y","he said ""hi"""\n2,"line1\nline2",\n', 'f.csv');
  assert.deepEqual(s.rows, [
    ['a', 'b', 'c'],
    ['1', 'x, y', 'he said "hi"'],
    ['2', 'line1\nline2', ''],
  ]);
  assert.equal(s.total, 3);
  // 没有结尾换行的最后一行
  assert.deepEqual(parseCsv('a,b\n1,2', 'f.csv')[0].rows, [['a', 'b'], ['1', '2']]);
  // 分号分隔（欧洲地区 Excel 导出的 CSV）、TSV
  assert.deepEqual(parseCsv('a;b;c\n1,5;2;3', 'f.csv')[0].rows, [['a', 'b', 'c'], ['1,5', '2', '3']]);
  assert.deepEqual(parseCsv('a\tb\n1\t2', 'f.tsv')[0].rows, [['a', 'b'], ['1', '2']]);
});

test('parseCsv keeps only the first rows', () => {
  const [s] = parseCsv(Array.from({ length: 800 }, (_, i) => `${i}`).join('\n'), 'f.csv');
  assert.equal(s.rows.length, 500);
  assert.equal(s.total, 800);
});

test('colName', () => {
  assert.deepEqual([0, 25, 26, 27, 51, 52, 701, 702].map(colName), ['A', 'Z', 'AA', 'AB', 'AZ', 'BA', 'ZZ', 'AAA']);
});
