// Markdown 预览（“文件”页、终端的“文件”标签）。文件来自用户的电脑，内容不可信：
// marked 转成 HTML 后用 DOMPurify 清洗（去掉脚本、事件属性、iframe、表单等），再放进管理台。
// 两个库按需加载，不进首屏的包。外部链接在新标签页打开；相对路径的图片交给调用方换成这台电脑上的文件地址。
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { copyText } from './chatCopy.js';

export const isMarkdown = (name) => /\.(md|markdown|mdx|mdown|mkd)$/i.test(name ?? '');

let libs = null;
const load = () => {
  libs ??= Promise.all([import('marked'), import('dompurify')]).then(([m, d]) => ({ marked: m.marked, purify: d.default }));
  return libs;
};

const EXTERNAL = /^(https?:|mailto:)/i;

// 代码块右上角的复制按钮（清洗之后才加上，按钮本身不来自文件）。图标是 lucide 的 copy / check
const COPY_SVG = '<svg class="md-copy-i" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="8" y="8" width="14" height="14" rx="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg><svg class="md-copy-ok" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';

function addCopyButtons(root, label) {
  for (const pre of root.querySelectorAll('pre')) {
    const box = document.createElement('div');
    box.className = 'md-code';
    const lang = /language-([\w+#.-]+)/.exec(pre.querySelector('code')?.className ?? '')?.[1];
    pre.replaceWith(box);
    box.append(pre);
    const bar = document.createElement('div');
    bar.className = 'md-code-bar';
    if (lang) {
      const tag = document.createElement('span');
      tag.className = 'md-code-lang';
      tag.textContent = lang;
      bar.append(tag);
    }
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'md-copy';
    btn.setAttribute('aria-label', label);
    btn.setAttribute('title', label);
    btn.innerHTML = COPY_SVG;
    bar.append(btn);
    box.prepend(bar);
  }
}

/** 点了代码块的复制：复制这个代码块的文字，按钮变成对勾 1.5 秒 */
async function onCopyClick(e) {
  const btn = e.target.closest?.('.md-copy');
  if (!btn) return;
  e.preventDefault();
  const pre = btn.closest('.md-code')?.querySelector('pre');
  if (!pre) return;
  const text = pre.textContent.replace(/\n$/, '');
  if (await copyText(text)) {
    btn.dataset.done = '';
    setTimeout(() => delete btn.dataset.done, 1500);
  } else {
    window.prompt('', text);
  }
}

const SAFE_IMAGE = /^(https?:|data:image\/(png|jpe?g|gif|webp);)/i;

/** 文字里出现 links 里的写法（本机文件路径）就换成链接，在新标签页打开；已经在链接里的不动 */
function linkify(root, links) {
  const keys = Object.keys(links).sort((a, b) => b.length - a.length);
  if (!keys.length) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    if (!n.parentElement?.closest('a') && keys.some((k) => n.data.includes(k))) nodes.push(n);
  }
  for (const node of nodes) {
    const frag = document.createDocumentFragment();
    let rest = node.data;
    while (rest) {
      let at = -1;
      let key = null;
      for (const k of keys) {
        const i = rest.indexOf(k);
        if (i >= 0 && (at < 0 || i < at)) [at, key] = [i, k];
      }
      if (at < 0) {
        frag.append(rest);
        break;
      }
      if (at > 0) frag.append(rest.slice(0, at));
      const a = document.createElement('a');
      a.setAttribute('href', links[key]);
      a.setAttribute('target', '_blank');
      a.setAttribute('rel', 'noopener noreferrer');
      a.className = 'md-file';
      a.textContent = key;
      frag.append(a);
      rest = rest.slice(at + key.length);
    }
    node.replaceWith(frag);
  }
}

/**
 * text：Markdown 原文；imageUrl(相对路径) → 图片地址（没有时相对路径的图片不显示）；
 * links：{ 文字里的写法: 地址 }，对话里提到的本机文件变成可以点开的链接。
 */
export default function Markdown({ text, imageUrl, links }) {
  const { t } = useTranslation();
  const [html, setHtml] = useState(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    load()
      .then(({ marked, purify }) => {
        const raw = marked.parse(text, { gfm: true, breaks: false, async: false });
        // 指向这台电脑上文件的链接（例如 Codex 写的 [下载 PDF](F:/out/a.pdf)）：是这段对话产出的文件就换成它的地址。
        // 要在清洗之前换：`F:/…` 会被当成不认识的协议去掉
        purify.addHook('beforeSanitizeAttributes', (node) => {
          if (node.tagName !== 'A' || !links) return;
          const href = node.getAttribute('href') ?? '';
          let raw = href;
          try {
            raw = decodeURI(href);
          } catch {
            /* 不是合法的转义，按原样比 */
          }
          const to = links[raw] ?? links[raw.replace(/^file:\/\/\/?/i, '')];
          if (to) {
            node.setAttribute('href', to);
            node.setAttribute('data-file', '1');
          }
        });
        purify.addHook('afterSanitizeAttributes', (node) => {
          if (node.tagName === 'A') {
            const href = node.getAttribute('href') ?? '';
            if (node.getAttribute('data-file') === '1' && href.startsWith('/api/')) {
              node.removeAttribute('data-file');
              node.setAttribute('class', 'md-file');
              // 要下载的（PDF 等）直接下载，不开新标签页；能看的在新标签页里打开
              if (href.includes('&download=1')) {
                node.setAttribute('download', '');
              } else {
                node.setAttribute('target', '_blank');
                node.setAttribute('rel', 'noopener noreferrer');
              }
            } else if (EXTERNAL.test(href)) {
              node.setAttribute('target', '_blank');
              node.setAttribute('rel', 'noopener noreferrer');
            } else if (!href.startsWith('#')) {
              // 指向这台电脑上其他文件的相对链接：在管理台里打不开，只保留文字（对话产出的文件已在清洗前换成了地址）
              node.removeAttribute('href');
            }
          } else if (node.tagName === 'IMG') {
            const src = node.getAttribute('src') ?? '';
            if (SAFE_IMAGE.test(src)) return;
            const local = src && !/^[a-z][a-z0-9+.-]*:/i.test(src) && imageUrl ? imageUrl(decodeURI(src.split(/[?#]/)[0])) : null;
            if (local) node.setAttribute('src', local);
            else node.removeAttribute('src');
            node.setAttribute('loading', 'lazy');
          }
        });
        const doc = purify.sanitize(raw, { FORBID_TAGS: ['style', 'form', 'button', 'textarea', 'select', 'iframe', 'object', 'embed'], FORBID_ATTR: ['style'], RETURN_DOM_FRAGMENT: true });
        purify.removeHook('afterSanitizeAttributes');
        purify.removeHook('beforeSanitizeAttributes');
        // 只留任务列表的勾选框（- [x]），而且只读
        for (const el of doc.querySelectorAll('input')) {
          if (el.getAttribute('type') === 'checkbox') el.setAttribute('disabled', '');
          else el.remove();
        }
        if (links) linkify(doc, links);
        addCopyButtons(doc, t('action.copy'));
        const box = document.createElement('div');
        box.appendChild(doc);
        if (alive) setHtml(box.innerHTML);
      })
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- t 只用于按钮的文字
  }, [text, links]);
  if (failed) return <pre className="fb-text fb-wrap">{text}</pre>;
  if (html == null) return <p className="muted small fb-pad">{t('fb.loading')}</p>;
  // eslint-disable-next-line react/no-danger -- 已用 DOMPurify 清洗
  return <article className="md" onClick={onCopyClick} dangerouslySetInnerHTML={{ __html: html }} />;
}
