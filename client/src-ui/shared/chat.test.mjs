import assert from 'node:assert/strict';
import { test } from 'node:test';

import { applyEvent, insertMention, mentionAt, backgroundCounts, chatRows, conversationResources, diffLines, diffStat, EMPTY, fieldKind, knownDialog, latestTodos, localFilePath, lineDiff, baseModel, modelFamily, modelLabel, modelView, parseJson, questionAnswers, slashAction, splitLinks, tokens, resultsById, toolCounts, toolSummary, toolUseIds, workParts } from './chat.js';

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
  assert.equal(toolSummary('Bash', { command: 'ls -la', description: 'List files' }), 'List files');
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
  assert.equal(diffLines('Write', { content: 'x\ny\n' }).length, 2);
  assert.deepEqual(diffLines('Write', { content: 'x\ny' }).map((l) => l.no), [1, 2]);
  const patch = [{ oldStart: 3, oldLines: 2, newStart: 3, newLines: 2, lines: [' a', '-b', '+c'] }];
  assert.deepEqual(diffLines('Edit', {}, { structuredPatch: patch }).map((l) => `${l.t}${l.no ?? ''}`), ['@', ' 3', '-4', '+4']);
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
  assert.equal(slashAction('/worktree', 'codex'), 'worktree');
  assert.equal(slashAction('/worktree', 'claude'), null, '只有 Codex 有');
  const qs = [{ question: '颜色？' }, { question: '水果？' }, { question: '空的' }];
  assert.deepEqual(questionAnswers(qs, { 0: { labels: ['红'] }, 1: { labels: ['苹果'], other: '梨' } }), { '颜色？': '红', '水果？': '苹果, 梨' });
});

test('consecutive tool calls are grouped', () => {
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
  assert.deepEqual(rows.map((r) => r.k), ['item', 'text', 'group', 'tool', 'tool', 'text', 'item']);
  assert.deepEqual(rows[2].calls.map((c) => c.id), ['1', '2', '3', '4']);
  assert.equal(rows[2].key, 'g:1');
  assert.deepEqual(toolCounts(rows[2].calls), [['Read', 2], ['Grep', 1], ['Edit', 1]]);
  // 待办不合并，打断了分组；后面单独一个调用不成组
  assert.equal(rows[3].block.name, 'TodoWrite');
  assert.equal(rows[4].block.name, 'Bash');
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

test('运行中的状态行：中文界面换成中文动词，去掉时间；表里没有的原样显示', async () => {
  const { spinnerText, SPINNER_ZH } = await import('./spinner.js');
  assert.deepEqual(spinnerText('Unraveling… (8s · ↓ 22 tokens · thinking)', 'zh-CN'), { text: '抽丝剥茧…', detail: '↓ 22 tokens · 思考中', orig: 'Unraveling' });
  assert.deepEqual(spinnerText('Unraveling… (1m 3s)', 'en-US'), { text: 'Unraveling…', detail: '', orig: '' });
  assert.deepEqual(spinnerText('Manifesting… (thought for 1s)', 'zh-CN'), { text: '心想事成…', detail: '想了 1s', orig: 'Manifesting' });
  assert.deepEqual(spinnerText('Running tests…', 'zh-CN'), { text: 'Running tests…', detail: '', orig: '' });
  assert.equal(spinnerText("Beboppin'…", 'zh-CN').text, '踏歌而行…');
  assert.ok(Object.keys(SPINNER_ZH).length >= 187);
});

test('时间点解析：微秒、没写时区、秒数', async () => {
  const { parseInstant } = await import('./time.js');
  assert.equal(parseInstant('2026-10-24T17:59:59.943648+00:00'), Date.UTC(2026, 9, 24, 17, 59, 59, 943));
  assert.equal(parseInstant('2026-10-24T17:59:59'), Date.UTC(2026, 9, 24, 17, 59, 59));
  assert.equal(parseInstant('2026-10-24T18:00:00+08:00'), Date.UTC(2026, 9, 24, 10));
  assert.equal(parseInstant(1792583484), 1792583484000);
  assert.ok(Number.isNaN(parseInstant('junk')));
});

test('trust and bypass dialogs are recognized by their options', () => {
  // 60 列时目录折成两行（2.1.310 实测）
  const trust = { title: 'Accessing workspace:', body: ['/tmp/claude-0/-workspace-', '29ccd47f9839/proj', '', 'Quick safety check: …'], options: [{ label: 'No, exit' }, { label: 'Yes, I trust this folder' }] };
  assert.deepEqual(knownDialog(trust), { kind: 'trust', path: '/tmp/claude-0/-workspace-29ccd47f9839/proj' });
  const bypass = { title: 'WARNING: Claude Code running in Bypass Permissions mode', body: ['In Bypass Permissions mode, …'], options: [{ label: 'No, exit' }, { label: 'Yes, I accept' }] };
  assert.deepEqual(knownDialog(bypass), { kind: 'bypass', path: '' });
  assert.equal(knownDialog({ title: 'Bash command', body: [], options: [{ label: 'Yes' }, { label: 'No' }] }), null);
  assert.equal(knownDialog(null), null);
});

test('links to files on this computer', () => {
  assert.equal(localFilePath('/Users/a/.codex/sessions/2026/10/23/rollout-x.jsonl'), '/Users/a/.codex/sessions/2026/10/23/rollout-x.jsonl');
  assert.equal(localFilePath('/Users/a/my%20proj/src/main.rs:12'), '/Users/a/my proj/src/main.rs');
  assert.equal(localFilePath('/p/a.rs:12:3'), '/p/a.rs');
  assert.equal(localFilePath('/p/a.rs#L12-L20'), '/p/a.rs');
  assert.equal(localFilePath('file:///C:/out/a.pdf'), 'C:/out/a.pdf');
  assert.equal(localFilePath('F:\\out\\a.pdf'), 'F:\\out\\a.pdf');
  assert.equal(localFilePath('file:///home/a/x.txt'), '/home/a/x.txt');
  for (const h of ['https://x.com/a', '//cdn/x.js', 'docs/a.md', '#top', 'mailto:a@b.c', '', null]) assert.equal(localFilePath(h), null, String(h));
});

test('emails are masked', async () => {
  const { maskEmail } = await import('./chat.js');
  assert.equal(maskEmail('john@example.com'), 'jo***@ex***.com');
  assert.equal(maskEmail('a@b.co'), 'a***@b***.co');
  assert.equal(maskEmail('ab@host'), 'a***@ho***');
});

test('sub-agents from the conversation', async () => {
  const { conversationAgents } = await import('./chat.js');
  const items = [
    { k: 'assistant', ts: 't0', blocks: [{ t: 'tool_use', id: 'a1', name: 'Agent', input: { description: 'helper', agent_thread_id: 'th1' } }] },
    { k: 'tool_result', tool_use_id: 'a1', content: 'ok', extra: { agentId: 'th1' } },
    { k: 'assistant', blocks: [{ t: 'tool_use', id: 'a1#9', name: 'Agent', input: { description: 'helper', agent_thread_id: 'th1' } }] },
    { k: 'assistant', blocks: [{ t: 'tool_use', id: 't2', name: 'Task', input: { description: 'search', subagent_type: 'Explore', prompt: 'find x' } }] },
  ];
  const a = conversationAgents(items, true);
  assert.equal(a.length, 2);
  assert.deepEqual([a[0].id, a[0].tool_use_id, a[0].runs, a[0].state, a[0].started], ['th1', 'a1', 2, 'running', 't0']);
  assert.deepEqual([a[1].type, a[1].prompt, a[1].state], ['Explore', 'find x', 'running']);
  assert.equal(conversationAgents(items, false)[1].state, 'ended');
});

test('work parts: process collapses into one block, trailing text is the answer', () => {
  const text = (key) => ({ k: 'text', key, block: { t: 'text', text: key } });
  const tool = (key) => ({ k: 'tool', key, block: { t: 'tool_use', id: key } });
  const item = (key, k, extra) => ({ k: 'item', key, item: { k, key, ...extra } });
  const rows = [item('u1', 'user'), text('a'), tool('b'), text('c'), text('d'), item('t1', 'turn', { ms: 9000 }), item('u2', 'user'), text('e'), item('u3', 'user'), tool('f')];
  const parts = workParts(rows, false);
  assert.deepEqual(parts.map((p) => (p.k === 'work' ? `W${p.rows.join('')}${p.answer ? '+' : ''}` : p.n)), [0, 'W12+', 3, 4, 5, 6, 7, 8, 'W9']);
  assert.equal(parts[1].turn.ms, 9000);
  assert.equal(parts[1].running, false);
  // 还在跑：最后一段连文字一起都在处理过程里
  const live = workParts([item('u1', 'user'), tool('a'), text('b')], true);
  assert.deepEqual(live[1], { k: 'work', key: 'w:a', rows: [1, 2], running: true, answer: false, turn: null });
  // 没配上的工具结果不打断处理过程
  assert.deepEqual(workParts([tool('a'), item('r', 'tool_result'), text('b')], false).map((p) => p.k), ['work', 'row']);
});

test('mentionAt / insertMention：输入框里的 @ 提示', () => {
  assert.deepEqual(mentionAt('看看 @src/ch', 10), { start: 3, q: 'src/ch' });
  assert.deepEqual(mentionAt('@', 1), { start: 0, q: '' });
  assert.equal(mentionAt('a@b.com', 7), null, '邮箱不算');
  assert.equal(mentionAt('@a b', 4), null, '打了空格就结束');
  assert.deepEqual(insertMention('看 @ch 吧', 2, 5, 'src/chat.js'), { text: '看 @src/chat.js  吧', caret: 15 });
  assert.deepEqual(insertMention('@s', 0, 2, 'src/'), { text: '@src/', caret: 5 }, '目录不加空格，接着找');
  assert.equal(insertMention('@m', 0, 2, 'my file.txt').text, '@"my file.txt" ');
});
