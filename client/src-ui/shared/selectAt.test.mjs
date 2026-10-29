import assert from 'node:assert/strict';
import test from 'node:test';

import { locateLine } from './selectAt.js';

const text = ['$ ls', 'a.txt', 'b.txt', '$ cat a.txt', 'hello', 'world', '$ ls', 'a.txt', 'b.txt', '$ '].join('\n');

test('finds the pressed row by its neighbours', () => {
  // 屏幕上按住第二次 ls 输出里的 a.txt
  assert.equal(locateLine(text, { row: 2, lines: ['world', '$ ls', 'a.txt', 'b.txt', '$'] }), 7);
  // 第一次 ls 的 a.txt：上下文不同
  assert.equal(locateLine(text, { row: 1, lines: ['$ ls', 'a.txt', 'b.txt', '$ cat a.txt'] }), 1);
});

test('ties go to the latest output', () => {
  assert.equal(locateLine(text, { row: 0, lines: ['a.txt'] }), 7);
});

test('a wrapped screen row matches the joined line', () => {
  const t = ['short', 'x'.repeat(30) + 'TAIL-OF-LONG-LINE', 'next'].join('\n');
  assert.equal(locateLine(t, { row: 1, lines: ['x'.repeat(30), 'TAIL-OF-LONG-LINE', 'next'] }), 1);
});

test('a blank pressed row uses the nearest non-blank row', () => {
  const t = ['one', 'two', '', 'three'].join('\n');
  assert.equal(locateLine(t, { row: 1, lines: ['two', '', 'three'] }), 2);
});

test('nothing usable', () => {
  assert.equal(locateLine(text, { row: 0, lines: ['not there at all'] }), -1);
  assert.equal(locateLine(text, { row: 0, lines: [''] }), -1);
  assert.equal(locateLine('', { row: 0, lines: ['a'] }), -1);
  assert.equal(locateLine(text, null), -1);
});
