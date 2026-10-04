// 问题与日志：客户端把 WARN / ERROR 日志和页面脚本错误记下来，这里查看；“报告问题”把描述连同（可选的）
// 运行信息、错误记录和运行日志发给看看服务端，发送前可以预览全部内容。没登录时可以下载成文件。
import { Bug, Check, Download, Send, Trash2 } from 'lucide-react';
import { createContext, useContext, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api } from './api.js';
import { relativeTime } from './time.js';
import { Button, Card, ErrorNote, Field, Modal, Note, Skeleton, useAction, useConfirm, useLoad } from './ui.jsx';

const Ctx = createContext(() => {});

/** 打开“报告问题”弹窗。 */
export function useReportDialog() {
  return useContext(Ctx);
}

export function ReportProvider({ children }) {
  const [open, setOpen] = useState(false);
  return (
    <Ctx.Provider value={() => setOpen(true)}>
      {children}
      <ReportDialog open={open} onClose={() => setOpen(false)} />
    </Ctx.Provider>
  );
}

function ReportDialog({ open, onClose }) {
  const { t } = useTranslation();
  const [description, setDescription] = useState('');
  const [contact, setContact] = useState('');
  const [include, setInclude] = useState(true);
  const [preview, setPreview] = useState(false);
  const [sent, setSent] = useState(null);
  const { busy, error, setError, run } = useAction();
  const [diag] = useLoad(() => (open ? api.get('/diag') : Promise.resolve(null)), [open]);
  const [bundle] = useLoad(() => (open && preview ? api.get('/diag/bundle') : Promise.resolve(null)), [open, preview]);
  useEffect(() => {
    if (!open) return;
    setSent(null);
    setError(null);
    setPreview(false);
  }, [open]);
  const submit = async (e) => {
    e.preventDefault();
    const r = await run(() => api.post('/diag/report', { description, contact, include }));
    if (r) {
      setSent(r);
      setDescription('');
    }
  };
  const canSend = diag?.can_report !== false;
  return (
    <Modal open={open} onClose={onClose} title={t('report.title')} wide>
      {sent ? (
        <>
          <Note kind="ok">
            <b>{t('report.sent', { id: sent.id })}</b>
            <span style={{ display: 'block' }}>{t('report.sent_body')}</span>
          </Note>
          <div className="modal-actions">
            <Button onClick={onClose}>{t('action.close')}</Button>
          </div>
        </>
      ) : (
        <form onSubmit={submit}>
          <p className="muted small" style={{ marginTop: 0 }}>
            {t('report.intro')}
          </p>
          {!canSend && <Note kind="warn">{t('report.need_login')}</Note>}
          <Field label={t('report.description')}>
            <textarea rows={5} value={description} onChange={(e) => setDescription(e.target.value)} maxLength={4000} required placeholder={t('report.description_placeholder')} />
          </Field>
          <Field label={t('report.contact')} hint={t('report.contact_hint')}>
            <input value={contact} onChange={(e) => setContact(e.target.value)} maxLength={200} autoComplete="email" />
          </Field>
          <label className="check-row">
            <input type="checkbox" checked={include} onChange={(e) => setInclude(e.target.checked)} />
            <span>
              <b>{t('report.include')}</b>
              <span className="field-hint" style={{ display: 'block' }}>
                {t('report.include_hint')}
              </span>
            </span>
          </label>
          {include && (
            <details className="report-preview" onToggle={(e) => setPreview(e.currentTarget.open)}>
              <summary>{t('report.preview')}</summary>
              {!bundle ? (
                <Skeleton height={120} />
              ) : (
                <pre>
                  {JSON.stringify(bundle.meta, null, 2)}
                  {'\n\n'}
                  {bundle.errors || t('report.no_errors')}
                  {'\n\n'}
                  {bundle.logs}
                </pre>
              )}
            </details>
          )}
          <ErrorNote error={error} />
          <div className="modal-actions">
            <a className="btn btn-ghost" href="/api/diag/bundle.txt" download>
              <Download size={18} aria-hidden="true" />
              {t('report.download')}
            </a>
            <Button type="submit" icon={Send} busy={busy} disabled={!canSend || !description.trim()}>
              {t('report.send')}
            </Button>
          </div>
        </form>
      )}
    </Modal>
  );
}

const LEVEL = { ERROR: 'badge-danger', WARN: 'badge-warn', UI: 'badge-danger' };

/** 设置页：最近的错误记录 + 报告问题。 */
export function DiagCard() {
  const { t } = useTranslation();
  const openReport = useReportDialog();
  const confirm = useConfirm();
  const [data, error, reload] = useLoad(() => api.get('/diag'));
  const [all, setAll] = useState(false);
  const items = data?.errors ?? [];
  const day = Date.now() - 24 * 3600 * 1000;
  const recent = items.filter((e) => Date.parse(e.time) > day).length;
  const shown = all ? items : items.slice(0, 8);
  const clear = async () => {
    if (await confirm({ title: t('diag.clear_title'), body: t('diag.clear_body'), confirm: t('diag.clear') })) {
      await api.post('/diag/clear').catch(() => {});
      reload();
    }
  };
  return (
    <Card title={t('diag.title')}>
      <p className="muted small" style={{ marginTop: 0 }}>
        {t('diag.intro')}
      </p>
      <ErrorNote error={error} />
      {!data ? (
        <Skeleton height={60} />
      ) : items.length === 0 ? (
        <Note kind="ok" icon={Check}>
          {t('diag.none')}
        </Note>
      ) : (
        <>
          <p className="small" style={{ margin: '0 0 6px' }}>
            {t('diag.summary', { total: items.length, recent })}
          </p>
          <ul className="diag-list">
            {shown.map((e, i) => (
              <li key={`${e.time}-${i}`}>
                <span className={`badge ${LEVEL[e.level] ?? ''}`}>{e.level === 'UI' ? t('diag.level_ui') : e.level === 'ERROR' ? t('diag.level_error') : t('diag.level_warn')}</span>
                <span className="grow">
                  <span className="diag-msg">
                    {e.message}
                    {e.count > 1 && <span className="badge diag-count">×{e.count}</span>}
                  </span>
                  {(e.fields || e.target) && (
                    <span className="mono small muted diag-fields">
                      {e.target && `[${e.target}] `}
                      {e.fields}
                    </span>
                  )}
                </span>
                <span className="small muted diag-time" title={e.time}>
                  {relativeTime(t, e.time)}
                </span>
              </li>
            ))}
          </ul>
          {items.length > shown.length && (
            <button type="button" className="btn btn-plain" onClick={() => setAll(true)}>
              {t('diag.show_all', { n: items.length })}
            </button>
          )}
        </>
      )}
      <div className="row" style={{ flexWrap: 'wrap', marginTop: 12 }}>
        <Button icon={Bug} onClick={openReport}>
          {t('report.title')}
        </Button>
        <a className="btn btn-secondary" href="/api/diag/bundle.txt" download>
          <Download size={18} aria-hidden="true" />
          {t('report.download')}
        </a>
        {items.length > 0 && (
          <Button variant="ghost" icon={Trash2} onClick={clear}>
            {t('diag.clear')}
          </Button>
        )}
      </div>
    </Card>
  );
}
