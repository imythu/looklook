import assert from 'node:assert/strict';
import { test } from 'node:test';

import { applyEvent, diffLines, diffStat, EMPTY, latestTodos, lineDiff, opensTui, questionAnswers, resultsById, toolSummary, toolUseIds } from './chat.js';

test('events merge into the tail and update the last item', () => {
  let s = applyEvent(EMPTY, 'snapshot', { status: { phase: 'idle' }, start: 10, total: 12, items: [{ key: 'a' }, { key: 'b' }] });
  s = applyEvent(s, 'items', { start: 11, items: [{ key: 'b', more: 1 }, { key: 'c' }] });
  assert.deepEqual(s.items.map((i) => i.key), ['a', 'b', 'c']);
  assert.equal(s.items[1].more, 1);
  assert.equal(s.total, 13);
  s = applyEvent(s, 'older', { start: 8, items: [{ key: 'y' }, { key: 'z' }] });
  assert.equal(s.start, 8);
  assert.deepEqual(s.items.map((i) => i.key), ['y', 'z', 'a', 'b', 'c']);
  s = applyEvent(s, 'status', { phase: 'running' });
  assert.equal(s.status.phase, 'running');
});

test('tool results pair with tool uses', () => {
  const items = [
    { k: 'assistant', blocks: [{ t: 'tool_use', id: 't1', name: 'Bash', input: { command: 'ls' } }] },
    { k: 'tool_result', tool_use_id: 't1', content: 'a' },
  ];
  assert.equal(resultsById(items).get('t1').content, 'a');
  assert.ok(toolUseIds(items).has('t1'));
});

test('summaries', () => {
  assert.equal(toolSummary('Bash', { command: 'ls  -la\n' }), 'ls -la');
  assert.equal(toolSummary('Edit', { file_path: '/home/a/proj/src/main.rs' }), 'src/main.rs');
  assert.equal(toolSummary('mcp__looklook__ensure_mapping', { port: 5173, name: 'web' }), 'web');
  assert.equal(toolSummary('TodoWrite', { todos: [{ status: 'completed' }, { status: 'pending' }] }), '1/2');
});

test('diffs', () => {
  const d = lineDiff('a\nb\nc', 'a\nB\nc');
  assert.deepEqual(d.map((l) => l.t + l.text), [' a', '-b', '+B', ' c']);
  assert.deepEqual(diffStat(d), { add: 1, del: 1 });
  const p = diffLines('Edit', {}, { structuredPatch: [{ oldStart: 1, oldLines: 1, newStart: 1, newLines: 1, lines: ['-a', '+b', ' c'] }] });
  assert.deepEqual(p.map((l) => l.t), ['@', '-', '+', ' ']);
  assert.equal(diffLines('Write', { content: 'x\ny' }).length, 2);
});

test('todos, tui commands, answers', () => {
  const items = [{ k: 'assistant', blocks: [{ t: 'tool_use', name: 'TodoWrite', input: { todos: [{ content: '1', status: 'pending' }] } }] }];
  assert.equal(latestTodos(items).length, 1);
  assert.equal(latestTodos([]), null);
  assert.ok(opensTui('/model') && opensTui(' /resume abc') && !opensTui('/compact') && !opensTui('model'));
  const qs = [{ question: '颜色？' }, { question: '水果？' }, { question: '空的' }];
  assert.deepEqual(questionAnswers(qs, { 0: { labels: ['红'] }, 1: { labels: ['苹果'], other: '梨' } }), { '颜色？': '红', '水果？': '苹果, 梨' });
});
