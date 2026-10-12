// 附件（docs/FILE_TRANSFER.md §4.4）：粘贴 / 拖进终端的截图、“发给 AI”的文件，存在临时附件目录里，过期自动删除。
// - PastedTray：终端右下角的缩略图条，列出这次粘贴的图片；点开看大图，悬停出现删除。
// - ImageViewer：大图预览（名称、大小、路径、删除）。
// - AttachmentsCard：设置页里的管理：保留天数、全部附件（按时间倒序）、单个删除、清空。
import { File as FileIcon, ImageOff, Images, Trash2, X } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { useStatus } from '../App.jsx';
import { api } from './api.js';
import { errorText } from './i18n.js';
import { relativeTime } from './time.js';
import { Button, Card, CopyButton, Empty, ErrorNote, Field, Modal, Skeleton, useConfirm, useLoad, useToast } from './ui.jsx';
import { formatBytes } from './xfer.js';

export const RETENTION_DAYS = [1, 3, 7, 14, 30];
const IMAGE_RE = /\.(png|jpe?g|gif|webp|bmp|avif)$/i;

export const isImageName = (name) => IMAGE_RE.test(name ?? '');
// 放在终端路径下：远程打开、有多台电脑时，入口按终端把 <img> 请求转给它所在的电脑。
export const attachmentUrl = (a) => `/api/instances/${encodeURIComponent(a.instance)}/attachments/${encodeURIComponent(a.name)}`;
export const baseName = (p) => (p ?? '').split(/[\\/]/).pop();
const isoOf = (secs) => new Date(secs * 1000).toISOString();

function Thumb({ item, size }) {
  const [broken, setBroken] = useState(false);
  if (!item.image) return <FileIcon size={Math.round(size / 2.4)} aria-hidden="true" />;
  if (broken) return <ImageOff size={Math.round(size / 2.4)} aria-hidden="true" />;
  return <img src={attachmentUrl(item)} alt="" loading="lazy" decoding="async" onError={() => setBroken(true)} />;
}

/** 大图预览。item: { instance, name, path, size?, modified? } */
export function ImageViewer({ item, onClose, onDelete }) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  if (!item) return null;
  const del = async () => {
    setBusy(true);
    try {
      await onDelete(item);
      onClose();
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal open onClose={onClose} wide className="att-viewer" title={item.name}>
      <div className="att-stage">
        {item.image ? <img src={attachmentUrl(item)} alt={item.name} /> : <FileIcon size={48} aria-hidden="true" />}
      </div>
      <div className="att-meta">
        <span className="mono small ellipsis grow" title={item.path}>
          {item.path}
        </span>
        <CopyButton text={item.path} variant="plain" label={false} />
      </div>
      <p className="small muted" style={{ margin: 0 }}>
        {[item.size != null && formatBytes(item.size), item.modified && relativeTime(t, isoOf(item.modified))].filter(Boolean).join(' · ')}
      </p>
      <div className="modal-actions">
        <Button variant="danger" icon={Trash2} busy={busy} onClick={del}>
          {t('att.delete')}
        </Button>
        <Button variant="ghost" onClick={onClose}>
          {t('action.close')}
        </Button>
      </div>
    </Modal>
  );
}

/** 删除一个附件；已经不在了也算成功。 */
export async function deleteAttachment(item) {
  await api.del(attachmentUrl(item).replace(/^\/api/, ''));
}

/**
 * 终端右下角的“刚粘贴的图片”：items 是这个终端这次粘贴/拖进来的图片（新的在后），
 * onRemove(item) 从条里去掉（删除文件后也调用），onClear() 收起整条（不删文件）。
 */
export function PastedTray({ items, onRemove, onClear, touch }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [view, setView] = useState(null);
  if (!items.length) return null;
  const remove = async (item) => {
    try {
      await deleteAttachment(item);
      onRemove(item);
      toast(t('att.deleted'));
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
  };
  const shown = items.slice(-6);
  return (
    <>
      <div className={`att-tray ${touch ? 'att-tray-touch' : ''}`} role="group" aria-label={t('att.tray')}>
        <span className="att-tray-label" title={t('att.tray_hint')}>
          <Images size={14} aria-hidden="true" />
          {items.length}
        </span>
        {shown.map((item) => (
          <span key={item.path} className="att-tray-item">
            <button type="button" className="att-thumb" onClick={() => setView(item)} title={`${item.name} · ${t('att.view')}`} aria-label={`${t('att.view')} ${item.name}`}>
              <Thumb item={item} size={44} />
            </button>
            <button type="button" className="att-del" onClick={() => remove(item)} title={t('att.delete')} aria-label={`${t('att.delete')} ${item.name}`}>
              <X size={12} />
            </button>
          </span>
        ))}
        <button type="button" className="btn btn-plain btn-icon att-tray-close" onClick={onClear} title={t('att.tray_hide')} aria-label={t('att.tray_hide')}>
          <X size={15} />
        </button>
      </div>
      <ImageViewer item={view} onClose={() => setView(null)} onDelete={remove} />
    </>
  );
}

/** 设置页：附件保留天数与管理。 */
export function AttachmentsCard() {
  const { t } = useTranslation();
  const { status, reload: reloadStatus } = useStatus();
  const toast = useToast();
  const confirm = useConfirm();
  const [data, error, reload] = useLoad(() => api.get('/attachments'));
  const [insts] = useLoad(() => api.get('/instances'));
  const [view, setView] = useState(null);
  const [filter, setFilter] = useState('image');
  const [saving, setSaving] = useState(false);
  const names = useMemo(() => Object.fromEntries((insts?.items ?? []).map((x) => [x.id, x.name])), [insts]);
  const items = data?.items ?? [];
  const shown = filter === 'image' ? items.filter((x) => x.image) : items;
  const total = items.reduce((n, x) => n + x.size, 0);
  const days = data?.days ?? status.settings.attach_days ?? 7;

  useEffect(() => {
    const iv = setInterval(reload, 30000);
    return () => clearInterval(iv);
  }, []);

  const setDays = async (v) => {
    setSaving(true);
    try {
      await api.put('/settings', { ...status.settings, attach_days: v });
      reload();
      reloadStatus();
      toast(t('common.saved'));
    } catch (e) {
      toast(errorText(t, e), 'error');
    } finally {
      setSaving(false);
    }
  };
  const remove = async (item) => {
    try {
      await deleteAttachment(item);
      toast(t('att.deleted'));
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
    reload();
  };
  const clear = async () => {
    if (!(await confirm({ title: t('att.clear_title'), body: t('att.clear_body', { n: items.length, size: formatBytes(total) }), confirm: t('att.clear'), danger: true }))) return;
    try {
      const r = await api.del('/attachments');
      toast(t('att.cleared', { n: r?.removed ?? 0 }));
    } catch (e) {
      toast(errorText(t, e), 'error');
    }
    reload();
  };

  return (
    <Card
      title={t('att.title')}
      actions={
        items.length > 0 && (
          <Button variant="ghost" icon={Trash2} onClick={clear}>
            {t('att.clear')}
          </Button>
        )
      }
    >
      <p className="muted small" style={{ marginTop: 0 }}>
        {t('att.lead')}
      </p>
      <Field label={t('att.keep')} hint={data?.dir && <span className="mono">{data.dir}</span>}>
        <div className="seg" role="group" aria-label={t('att.keep')} aria-busy={saving}>
          {RETENTION_DAYS.map((d) => (
            <button key={d} type="button" aria-pressed={days === d} disabled={saving} onClick={() => days !== d && setDays(d)}>
              {t(d === 1 ? 'att.day' : 'att.days', { count: d })}
            </button>
          ))}
        </div>
      </Field>
      <ErrorNote error={error} />
      {!data && !error && <Skeleton height={88} />}
      {data && (
        <>
          <div className="att-bar">
            <span className="small muted grow">{t('att.summary', { n: items.length, size: formatBytes(total) })}</span>
            <div className="seg seg-sm" role="group" aria-label={t('att.filter')}>
              <button type="button" aria-pressed={filter === 'image'} onClick={() => setFilter('image')}>
                {t('att.only_images')}
              </button>
              <button type="button" aria-pressed={filter === 'all'} onClick={() => setFilter('all')}>
                {t('att.all')}
              </button>
            </div>
          </div>
          {shown.length === 0 ? (
            <Empty icon={Images} title={t('att.empty')}>
              <p className="muted small">{t('att.empty_hint')}</p>
            </Empty>
          ) : (
            <ul className="att-grid">
              {shown.map((item) => (
                <li key={`${item.instance}/${item.name}`} className="att-cell">
                  <button type="button" className="att-cell-img" onClick={() => setView(item)} aria-label={`${t('att.view')} ${item.name}`}>
                    <Thumb item={item} size={120} />
                  </button>
                  <div className="att-cell-meta">
                    <span className="ellipsis small" title={item.name}>
                      {item.name}
                    </span>
                    <span className="ellipsis small muted" title={names[item.instance] ?? item.instance}>
                      {names[item.instance] ?? t('att.deleted_term')} · {relativeTime(t, isoOf(item.modified))}
                    </span>
                  </div>
                  <button type="button" className="btn btn-plain btn-icon att-cell-del" onClick={() => remove(item)} title={t('att.delete')} aria-label={`${t('att.delete')} ${item.name}`}>
                    <Trash2 size={15} />
                  </button>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      <ImageViewer item={view} onClose={() => setView(null)} onDelete={remove} />
    </Card>
  );
}
