// 新会话输入框旁边的“模板”（docs/PLAN_2026-10-27.md §6.2）：常用任务点一下填进输入框（不直接发送，还能改），
// 可以把当前输入存成模板、改、删。模板在同步集合 prompts 里（所有电脑一份，客户端 prompts.rs）；
// 第一次打开时按界面语言写入几条示例，以后删掉就不再出现。
import { Pencil, Plus, ScrollText, Settings2, Trash2 } from 'lucide-react';
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import PopMenu from '../shared/PopMenu.jsx';
import { Button, ErrorNote, Field, Modal, useAction, useConfirm } from '../shared/ui.jsx';

const EXAMPLES = 5;
const SNIPPET = 48;

function snippet(text) {
  const s = text.replace(/\s+/g, ' ').trim();
  return s.length > SNIPPET ? `${s.slice(0, SNIPPET)}…` : s;
}

/** 新建 / 修改一个模板（prompt：已有的模板；没有 id 是新建，text 预先填好） */
function EditModal({ client, prompt, onClose, onSaved }) {
  const { t } = useTranslation();
  const [name, setName] = useState(prompt.name ?? '');
  const [text, setText] = useState(prompt.text ?? '');
  const { busy, error, run } = useAction();
  const submit = async (e) => {
    e.preventDefault();
    const body = { name: name.trim(), text: text.trim() };
    const v = await run(() => (prompt.id ? client.put(`/prompts/${encodeURIComponent(prompt.id)}`, body) : client.post('/prompts', body)));
    if (v) onSaved(v);
  };
  return (
    <Modal open onClose={onClose} title={t(prompt.id ? 'prompts.edit_title' : 'prompts.new_title')}>
      <form onSubmit={submit} className="stack">
        <Field label={t('prompts.name')}>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={40} autoFocus placeholder={t('prompts.name_ph')} />
        </Field>
        <Field label={t('prompts.text')} hint={t('prompts.text_hint')}>
          <textarea value={text} onChange={(e) => setText(e.target.value)} maxLength={4000} rows={6} />
        </Field>
        <ErrorNote error={error} />
        <div className="modal-actions">
          <Button variant="ghost" onClick={onClose}>
            {t('action.cancel')}
          </Button>
          <Button type="submit" busy={busy} disabled={!name.trim() || !text.trim()}>
            {t('action.save')}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

/** 管理：改名、改内容、删除、新建 */
function ManageModal({ client, items, onClose, onChange, onEdit }) {
  const { t } = useTranslation();
  const confirm = useConfirm();
  const { error, run } = useAction();
  const remove = async (p) => {
    if (!(await confirm({ title: t('prompts.delete_title', { name: p.name }), confirm: t('prompts.delete'), danger: true }))) return;
    const v = await run(() => client.del(`/prompts/${encodeURIComponent(p.id)}`));
    if (v) onChange(v);
  };
  return (
    <Modal open onClose={onClose} title={t('prompts.manage_title')}>
      <p className="muted small">{t('prompts.manage_lead')}</p>
      <ErrorNote error={error} />
      {items.length === 0 ? (
        <p className="muted">{t('prompts.none')}</p>
      ) : (
        <ul className="list">
          {items.map((p) => (
            <li key={p.id} style={{ flexWrap: 'nowrap' }}>
              <span style={{ minWidth: 0 }}>
                <b className="ellipsis" style={{ display: 'block' }}>
                  {p.name}
                </b>
                <span className="muted small ellipsis" style={{ display: 'block' }}>
                  {snippet(p.text)}
                </span>
              </span>
              <span className="inline" style={{ flexWrap: 'nowrap', gap: 2 }}>
                <Button variant="ghost" icon={Pencil} aria-label={t('prompts.edit', { name: p.name })} title={t('prompts.edit', { name: p.name })} onClick={() => onEdit(p)} />
                <Button variant="ghost" icon={Trash2} aria-label={t('prompts.delete_title', { name: p.name })} title={t('prompts.delete')} onClick={() => remove(p)} />
              </span>
            </li>
          ))}
        </ul>
      )}
      <div className="modal-actions">
        <Button variant="secondary" icon={Plus} onClick={() => onEdit({})}>
          {t('prompts.new')}
        </Button>
        <Button onClick={onClose}>{t('action.close')}</Button>
      </div>
    </Modal>
  );
}

/** 输入框上的“模板”按钮。text：输入框现在的内容（存成模板用）；onPick：选中的模板内容 */
export default function PromptsMenu({ client, text, onPick, className }) {
  const { t } = useTranslation();
  const [data, setData] = useState(null);
  const [editing, setEditing] = useState(null);
  const [managing, setManaging] = useState(false);
  useEffect(() => {
    let alive = true;
    (async () => {
      let v = await client.get('/prompts');
      // 第一次：写入这个语言的示例（只写一次，删掉就不再出现）
      if (!v.seeded) {
        const items = Array.from({ length: EXAMPLES }, (_, i) => ({ name: t(`prompts.ex${i + 1}_name`), text: t(`prompts.ex${i + 1}_text`) }));
        v = await client.post('/prompts/seed', { items });
      }
      if (alive) setData(v);
    })().catch(() => {});
    return () => {
      alive = false;
    };
    // client 每次渲染都是新对象；换电脑时 Draft 会重新挂载
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const items = data?.items ?? [];
  const menu = [
    { head: t('prompts.title') },
    ...items.map((p) => ({ label: p.name, hint: snippet(p.text), onClick: () => onPick(p.text) })),
    items.length === 0 && { label: t('prompts.none'), disabled: true, onClick: () => {} },
    'sep',
    { icon: Plus, label: t('prompts.save_current'), disabled: !text.trim(), onClick: () => setEditing({ text: text.trim() }) },
    { icon: Settings2, label: t('prompts.manage'), onClick: () => setManaging(true) },
  ];
  return (
    <>
      <PopMenu sheet className={className} icon={ScrollText} label={t('prompts.title')} items={menu} disabled={!data} />
      {managing && !editing && <ManageModal client={client} items={items} onClose={() => setManaging(false)} onChange={setData} onEdit={setEditing} />}
      {editing && (
        <EditModal
          client={client}
          prompt={editing}
          onClose={() => setEditing(null)}
          onSaved={(v) => {
            setData(v);
            setEditing(null);
          }}
        />
      )}
    </>
  );
}
