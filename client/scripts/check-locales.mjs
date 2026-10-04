// CI 检查：
// 1. zh-CN 与 en-US 的 key 集合一致（en-US 值为空，运行时回退中文）；
// 2. protocol 中客户端可能收到的每个错误码都有 `error.<CODE>` 文案；
// 3. 源码中字面量 t('…') 使用的 key 都存在于 zh-CN。
// 加 `--write-en` 时按 zh-CN 结构重写 en-US（保留已有英文）。
import { readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..');
const loc = join(root, 'src-ui/locales');
const namespaces = readdirSync(join(loc, 'zh-CN')).map((f) => f.replace(/\.json$/, ''));

function flat(obj, prefix = '') {
  return Object.entries(obj).flatMap(([k, v]) => (v && typeof v === 'object' ? flat(v, `${prefix}${k}.`) : [[`${prefix}${k}`, v]]));
}

function blank(zh, en) {
  const out = {};
  for (const [k, v] of Object.entries(zh)) {
    out[k] = v && typeof v === 'object' ? blank(v, en?.[k] ?? {}) : typeof en?.[k] === 'string' ? en[k] : '';
  }
  return out;
}

let failed = false;
const fail = (msg) => {
  failed = true;
  console.error(`✗ ${msg}`);
};

const zh = {};
for (const ns of namespaces) {
  zh[ns] = JSON.parse(readFileSync(join(loc, 'zh-CN', `${ns}.json`), 'utf8'));
  const enPath = join(loc, 'en-US', `${ns}.json`);
  let en = {};
  try {
    en = JSON.parse(readFileSync(enPath, 'utf8'));
  } catch {
    /* 缺文件时由 --write-en 生成 */
  }
  if (process.argv.includes('--write-en')) {
    en = blank(zh[ns], en);
    writeFileSync(enPath, `${JSON.stringify(en, null, 2)}\n`);
  }
  const a = new Set(flat(zh[ns]).map(([k]) => k));
  const b = new Set(flat(en).map(([k]) => k));
  for (const k of a) if (!b.has(k)) fail(`en-US/${ns}.json 缺少 ${k}`);
  for (const k of b) if (!a.has(k)) fail(`en-US/${ns}.json 多出 ${k}`);
}

// 错误码
const rs = readFileSync(join(root, '../protocol/src/error_code.rs'), 'utf8');
// 只在服务端引导、管理后台出现的错误码，客户端收不到。
const SERVER_ONLY = new Set(['SETUP_TOKEN_INVALID', 'SETUP_ALREADY_COMPLETED', 'SETUP_STEP_FAILED', 'CERT_OPERATION_IN_PROGRESS']);
const block = rs.slice(rs.indexOf('error_codes! {'), rs.indexOf('}', rs.indexOf('error_codes! {')));
const codes = [...block.matchAll(/^\s+([A-Z][A-Za-z]+) = \d+,/gm)].map((m) => m[1].replace(/([a-z])([A-Z])/g, '$1_$2').toUpperCase());
if (codes.length < 10) fail('未能从 protocol 解析错误码');
for (const c of codes) if (!SERVER_ONLY.has(c) && !(c in zh.error)) fail(`error.json 缺少错误码 ${c}`);

// 源码中的 key
function walk(dir) {
  return readdirSync(dir).flatMap((f) => {
    const p = join(dir, f);
    return statSync(p).isDirectory() ? walk(p) : /\.(jsx?|tsx?)$/.test(f) ? [p] : [];
  });
}
const has = (ns, key) => flat(zh[ns] ?? {}).some(([k]) => k === key || k.startsWith(`${key}_`));
for (const file of walk(join(root, 'src-ui'))) {
  const src = readFileSync(file, 'utf8');
  const defNs = 'common';
  for (const m of src.matchAll(/\bt\(\s*'([^'`$]+)'/g)) {
    const [ns, key] = m[1].includes(':') ? m[1].split(':') : [defNs, m[1]];
    if (!has(ns, key)) fail(`${file.slice(root.length + 1)} 使用了不存在的 key ${ns}:${key}`);
  }
}

if (failed) process.exit(1);
console.log(`✓ locales ok（${namespaces.join(', ')}；${codes.length} 个错误码）`);
