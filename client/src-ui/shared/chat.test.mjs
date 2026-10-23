import assert from 'node:assert/strict';
import { test } from 'node:test';

import { applyEvent, backgroundCounts, chatRows, conversationResources, diffLines, diffStat, EMPTY, fieldKind, latestTodos, lineDiff, baseModel, modelFamily, modelLabel, modelView, parseJson, questionAnswers, slashAction, splitLinks, tokens, resultsById, toolCounts, toolSummary, toolUseIds } from './chat.js';

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

test('todos, slash commands, answers', () => {
  const items = [{ k: 'assistant', blocks: [{ t: 'tool_use', name: 'TodoWrite', input: { todos: [{ content: '1', status: 'pending' }] } }] }];
  assert.equal(latestTodos(items).length, 1);
  assert.equal(latestTodos([]), null);
  assert.equal(slashAction('/model'), 'model');
  assert.equal(slashAction(' /resume '), 'history');
  assert.equal(slashAction('/model sonnet'), null, '带参数的照常送进去');
  assert.equal(slashAction('/compact'), null);
  assert.equal(slashAction('model'), null);
  const qs = [{ question: '颜色？' }, { question: '水果？' }, { question: '空的' }];
  assert.deepEqual(questionAnswers(qs, { 0: { labels: ['红'] }, 1: { labels: ['苹果'], other: '梨' } }), { '颜色？': '红', '水果？': '苹果, 梨' });
});

test('consecutive tool calls of the same kind are grouped', () => {
  const use = (id, name) => ({ t: 'tool_use', id, name, input: {} });
  const res = (id) => ({ k: 'tool_result', key: `r${id}`, tool_use_id: id });
  const items = [
    { k: 'user', key: 'u', text: 'hi' },
    { k: 'assistant', key: 'a1', blocks: [{ t: 'text', text: '先看看' }, use('1', 'Read')] },
    res('1'),
    { k: 'assistant', key: 'a2', blocks: [{ t: 'thinking', text: ' ' }, use('2', 'Grep'), use('3', 'Read')] },
    res('2'),
    res('3'),
    { k: 'assistant', key: 'a3', blocks: [use('4', 'Edit')] },
    res('4'),
    { k: 'assistant', key: 'a4', blocks: [use('5', 'TodoWrite'), use('6', 'Bash'), { t: 'text', text: '好了' }] },
    { k: 'tool_result', key: 'orphan', tool_use_id: 'gone' },
  ];
  const rows = chatRows(items);
  assert.deepEqual(rows.map((r) => r.k), ['item', 'text', 'group', 'tool', 'tool', 'tool', 'text', 'item']);
  assert.deepEqual(rows[2].calls.map((c) => c.id), ['1', '2', '3']);
  assert.equal(rows[2].key, 'g:1');
  assert.deepEqual(toolCounts(rows[2].calls), [['Read', 2], ['Grep', 1]]);
  // 一个单独的调用不成组
  assert.equal(rows[3].block.name, 'Edit');
  assert.equal(rows[4].block.name, 'TodoWrite');
});

test('model labels', () => {
  assert.equal(modelLabel('claude-opus-5-5'), 'Opus 5.5');
  assert.equal(modelLabel('claude-haiku-4-5-20251001'), 'Haiku 4.5');
  assert.equal(modelLabel('claude-sonnet-5-5[1m]'), 'Sonnet 5.5 1M');
  assert.equal(modelLabel('claude-fable-5'), 'Fable 5');
  assert.equal(modelLabel('sonnet'), 'Sonnet');
  assert.equal(modelLabel('opusplan'), 'opusplan');
  assert.equal(modelFamily('claude-opus-5-5'), 'opus');
  assert.equal(modelFamily('default'), '');
  assert.equal(baseModel('claude-haiku-4-5-20251001[1m]'), 'claude-haiku-4-5');
  assert.equal(tokens(950), '950');
  assert.equal(tokens(12345), '12k');
  assert.equal(tokens(1234), '1.2k');
  assert.equal(tokens(1_500_000), '1.5M');
});

test('model button follows the real model', () => {
  const options = [
    { value: 'default', label: 'Default', description: 'Opus 5.5', model: 'claude-opus-5-5' },
    { value: 'claude-opus-5-5', label: 'Opus 5.5', model: 'claude-opus-5-5' },
    { value: 'claude-sonnet-5-5', label: 'Sonnet 5.5', model: 'claude-sonnet-5-5' },
    { value: 'sonnet', label: 'GLM 4.6', model: 'glm-4.6' },
  ];
  // 终端里切过模型：按最近一条回复显示
  assert.deepEqual(modelView({ model: 'claude-opus-5-5', actual_model: 'claude-sonnet-5-5' }, options), { checked: 'claude-sonnet-5-5', label: 'Sonnet 5.5', pending: false, mismatch: null });
  // 刚在界面上选了，还没有回复
  assert.deepEqual(modelView({ model: 'claude-sonnet-5-5', actual_model: 'claude-opus-5-5', model_pending: true, model_chosen: true }, options), { checked: 'claude-sonnet-5-5', label: 'Sonnet 5.5', pending: true, mismatch: null });
  // 默认
  assert.equal(modelView({ model: 'default', actual_model: 'claude-opus-5-5' }, options).checked, 'default');
  assert.equal(modelView({ model: 'default', actual_model: 'claude-opus-5-5' }, options).label, 'Opus 5.5');
  // 别名指到服务商的模型，回复也是它
  assert.deepEqual(modelView({ model: 'sonnet', actual_model: 'glm-4.6', model_chosen: true }, options), { checked: 'sonnet', label: 'GLM 4.6', pending: false, mismatch: null });
  // 选的和实际的对不上
  assert.equal(modelView({ model: 'claude-sonnet-5-5', actual_model: 'claude-haiku-5-5', model_chosen: true }, options).mismatch, 'Haiku 5.5');
  // 没有目录时的别名
  assert.equal(modelView({ model: 'opus', actual_model: 'claude-opus-5-5', model_chosen: true }, [{ value: 'opus', label: 'Opus', model: '' }]).mismatch, null);
  assert.equal(modelView(null, []).label, '');
});

test('background counts', () => {
  assert.deepEqual(backgroundCounts({ background: [{ n: 2, kind: 'shell' }], agents: 0 }), { shell: 2 });
  assert.deepEqual(backgroundCounts({ background: [{ n: 1, kind: 'subagent' }], agents: 3 }), { subagent: 3 });
  assert.deepEqual(backgroundCounts(null), {});
});

test('conversationResources: 链接、搜索、图片，最近的在前、去重、标出最近一轮', () => {
  const items = [
    { k: 'user', text: 'a' },
    { k: 'assistant', blocks: [{ t: 'text', text: '见 https://old.example/x。' }] },
    { k: 'user', text: 'b' },
    { k: 'assistant', blocks: [{ t: 'tool_use', id: 's', name: 'WebSearch', input: { query: 'rust tokio' } }] },
    { k: 'tool_result', tool_use_id: 's', content: 'Tokio docs https://tokio.rs/\nOld https://old.example/x' },
    { k: 'assistant', blocks: [{ t: 'tool_use', id: 'r', name: 'Read', input: { file_path: 'C:\pics\a.png' } }, { t: 'text', text: '参考 https://tokio.rs/' }] },
  ];
  const r = conversationResources(items);
  assert.deepEqual(
    r.map((x) => [x.kind, x.value, x.turn]),
    [
      ['link', 'https://tokio.rs/', true],
      ['image', 'C:\pics\a.png', true],
      ['link', 'https://old.example/x', true],
      ['search', 'rust tokio', true],
    ],
  );
  assert.equal(r[0].title, '');
});

test('工具参数与结果：认出 JSON、网址，长文字单独成块', () => {
  assert.deepEqual(parseJson(' {"url": "https://a.b/c", "ok": true} '), { url: 'https://a.b/c', ok: true });
  assert.deepEqual(parseJson('[1, 2]'), [1, 2]);
  assert.equal(parseJson('Published: {"a": 1}'), null);
  assert.equal(parseJson('{bad json}'), null);
  assert.equal(parseJson('42'), null);
  assert.deepEqual(splitLinks('已发布到 https://claude.ai/artifact/abc。完成'), [{ text: '已发布到 ' }, { text: 'https://claude.ai/artifact/abc', url: 'https://claude.ai/artifact/abc' }, { text: '。完成' }]);
  assert.deepEqual(splitLinks('see https://x.y/z.'), [{ text: 'see ' }, { text: 'https://x.y/z', url: 'https://x.y/z' }, { text: '.' }]);
  assert.deepEqual(splitLinks('no links'), [{ text: 'no links' }]);
  assert.equal(fieldKind({ a: 1 }), 'json');
  assert.equal(fieldKind('{"a": 1}'), 'json');
  assert.equal(fieldKind('https://claude.ai/x'), 'link');
  assert.equal(fieldKind('line 1\nline 2'), 'block');
  assert.equal(fieldKind('x'.repeat(200)), 'block');
  assert.equal(fieldKind('publish'), 'inline');
  assert.equal(fieldKind(3), 'inline');
});

test('认不出的工具：标题挑说明性的参数', () => {
  assert.equal(toolSummary('Artifact', { icon: 'chart', file_path: '/tmp/a/report.html' }), 'a/report.html');
  assert.equal(toolSummary('mcp__docs__create', { kind: 'doc', title: '周报' }), '周报');
  assert.equal(toolSummary('mcp__x__y', { foo: 'bar' }), 'bar');
});

test('参数被截断的工具调用记下所在的条目', () => {
  const items = [{ key: 'k1', k: 'assistant', blocks: [{ t: 'tool_use', id: 't1', name: 'Artifact', input: {}, truncated: true }] }];
  const rows = chatRows(items);
  assert.equal(rows[0].block.item, 'k1');
});
