import assert from 'node:assert/strict';
import { test } from 'node:test';

import { chatToMarkdown, exportName } from './chatCopy.js';

test('导出成 Markdown：你说的、回复原样，工具一次一行，结果和思考不要', () => {
  const items = [
    { k: 'user', text: '修一下登录页' },
    { k: 'assistant', blocks: [{ t: 'thinking', text: '想想' }, { t: 'tool_use', name: 'Read', input: { file_path: 'src/login.js' } }] },
    { k: 'tool_result', content: '文件内容' },
    { k: 'assistant', blocks: [{ t: 'tool_use', name: 'Bash', input: { command: 'npm   test' } }, { t: 'text', text: '改好了：\n\n```js\nok()\n```' }] },
    { k: 'user', text: '  ' },
    { k: 'user', text: '谢谢' },
  ];
  const md = chatToMarkdown(items, { user: '我', agent: 'Claude', title: '登录页' });
  assert.equal(md, '# 登录页\n\n## 我\n\n修一下登录页\n\n## Claude\n\n- `Read` src/login.js\n- `Bash` npm test\n\n改好了：\n\n```js\nok()\n```\n\n## 我\n\n谢谢\n');
});

test('导出的文件名', () => {
  assert.equal(exportName('修 bug: a/b', new Date('2026-10-26T10:00:00Z')), '修-bug-a-b-2026-10-26.md');
  assert.equal(exportName('', new Date('2026-10-26T10:00:00Z')), 'chat-2026-10-26.md');
});
