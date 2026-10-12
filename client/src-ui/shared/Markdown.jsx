// Markdown 预览（“文件”页、终端的“文件”标签）。文件来自用户的电脑，内容不可信：
// marked 转成 HTML 后用 DOMPurify 清洗（去掉脚本、事件属性、iframe、表单等），再放进管理台。
// 两个库按需加载，不进首屏的包。外部链接在新标签页打开；相对路径的图片交给调用方换成这台电脑上的文件地址。
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

export const isMarkdown = (name) => /\.(md|markdown|mdx|mdown|mkd)$/i.test(name ?? '');

let libs = null;
const load = () => {
  libs ??= Promise.all([import('marked'), import('dompurify')]).then(([m, d]) => ({ marked: m.marked, purify: d.default }));
  return libs;
};

const EXTERNAL = /^(https?:|mailto:)/i;
const SAFE_IMAGE = /^(https?:|data:image\/(png|jpe?g|gif|webp);)/i;

/** text：Markdown 原文；imageUrl(相对路径) → 图片地址（没有时相对路径的图片不显示）。 */
export default function Markdown({ text, imageUrl }) {
  const { t } = useTranslation();
  const [html, setHtml] = useState(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    let alive = true;
    load()
      .then(({ marked, purify }) => {
        const raw = marked.parse(text, { gfm: true, breaks: false, async: false });
        purify.addHook('afterSanitizeAttributes', (node) => {
          if (node.tagName === 'A') {
            const href = node.getAttribute('href') ?? '';
            if (EXTERNAL.test(href)) {
              node.setAttribute('target', '_blank');
              node.setAttribute('rel', 'noopener noreferrer');
            } else if (!href.startsWith('#')) {
              // 指向这台电脑上其他文件的相对链接：在管理台里打不开，只保留文字
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
        // 只留任务列表的勾选框（- [x]），而且只读
        for (const el of doc.querySelectorAll('input')) {
          if (el.getAttribute('type') === 'checkbox') el.setAttribute('disabled', '');
          else el.remove();
        }
        const box = document.createElement('div');
        box.appendChild(doc);
        if (alive) setHtml(box.innerHTML);
      })
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [text]);
  if (failed) return <pre className="fb-text fb-wrap">{text}</pre>;
  if (html == null) return <p className="muted small fb-pad">{t('fb.loading')}</p>;
  // eslint-disable-next-line react/no-danger -- 已用 DOMPurify 清洗
  return <article className="md" dangerouslySetInnerHTML={{ __html: html }} />;
}
