// 对话里的复制和导出（docs/UI_REDESIGN_2026-10.md §4）。不依赖 React，单元测试见 chatCopy.test.mjs。

/**
 * 复制文字。剪贴板接口只在 https / localhost 下能用；局域网的 http 地址上退回 `execCommand('copy')`。
 * 都不行时返回 false，由调用方让用户自己选（例如打开“选择文字”）。
 */
export async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    /* 退回旧办法 */
  }
  try {
    const ta = document.createElement('textarea');
    ta.value = text;
    ta.setAttribute('readonly', '');
    ta.style.cssText = 'position:fixed;top:0;left:0;opacity:0;pointer-events:none';
    document.body.appendChild(ta);
    ta.select();
    ta.setSelectionRange(0, text.length);
    const ok = document.execCommand('copy');
    ta.remove();
    return ok;
  } catch {
    return false;
  }
}

/** 一次工具调用在导出里写成一行：工具名 + 最能说明它的参数 */
function toolLine(b) {
  const i = b.input ?? {};
  const arg = i.command ?? i.file_path ?? i.path ?? i.pattern ?? i.url ?? i.query ?? i.description ?? '';
  const one = String(arg).replace(/\s+/g, ' ').trim();
  return `- \`${b.name}\`${one ? ` ${one.length > 160 ? `${one.slice(0, 159)}…` : one}` : ''}`;
}

/**
 * 整段对话导出成 Markdown：你说的、AI 回的（正文原样），工具调用每次一行；工具结果、思考不导出。
 * `names`：{ user, agent, title }，标题和每段前面的名字。
 */
export function chatToMarkdown(items, { user = 'You', agent = 'Assistant', title = '' } = {}) {
  const out = [];
  if (title) out.push(`# ${title}`, '');
  let who = null;
  let tools = [];
  const flush = () => {
    if (tools.length) out.push(...tools, '');
    tools = [];
  };
  const head = (w) => {
    if (who === w) return;
    flush();
    who = w;
    out.push(`## ${w === 'user' ? user : agent}`, '');
  };
  for (const it of items) {
    if (it.k === 'user' && (it.text ?? '').trim()) {
      head('user');
      out.push(it.text.trim(), '');
    } else if (it.k === 'assistant') {
      for (const b of it.blocks ?? []) {
        if (b.t === 'text' && b.text?.trim()) {
          head('agent');
          flush();
          out.push(b.text.trim(), '');
        } else if (b.t === 'tool_use') {
          head('agent');
          tools.push(toolLine(b));
        }
      }
    }
  }
  flush();
  return `${out.join('\n').replace(/\n{3,}/g, '\n\n').trim()}\n`;
}

/** 导出文件名：标题里不能放进文件名的字换成短横线 */
export function exportName(title, date = new Date()) {
  const base = (title || 'chat').replace(/[\\/:*?"<>|\s]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60) || 'chat';
  const d = date.toISOString().slice(0, 10);
  return `${base}-${d}.md`;
}
