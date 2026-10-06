//! 管理台页面（构建产物编进可执行文件）、终端字体（随包资源目录）、终端辅助脚本与提示页。

use axum::extract::{Path, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

use super::App;

#[derive(RustEmbed)]
#[folder = "static/"]
struct Ui;

const TERM_JS: &str = include_str!("../term-inject.js");

pub async fn ui(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    if let Some(f) = (!path.is_empty()).then(|| Ui::get(path)).flatten() {
        let mime = mime_guess::from_path(path).first_or_octet_stream();
        let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
        return ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], f.data.into_owned()).into_response();
    }
    // 单页应用：其余路径都回到入口页面。
    match Ui::get("index.html") {
        Some(f) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], f.data.into_owned()).into_response(),
        None => (StatusCode::NOT_FOUND, "管理台页面缺失：请先执行 npm run build").into_response(),
    }
}

pub async fn term_js() -> Response {
    ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], TERM_JS).into_response()
}

pub async fn font(State(app): State<App>, Path(path): Path<String>) -> Response {
    let Some(dir) = &app.paths.fonts else { return StatusCode::NOT_FOUND.into_response() };
    // 只允许简单的相对路径，不能跳出字体目录。
    if path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == ".." || seg.contains('\\')) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match tokio::fs::read(dir.join(&path)).await {
        Ok(bytes) => {
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            let cache = if path == "fonts.css" { "no-cache" } else { "public, max-age=2592000" };
            ([(header::CONTENT_TYPE, mime.as_ref().to_string()), (header::CACHE_CONTROL, cache.to_string())], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

fn text(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "locked" => ("暂时不能打开终端", "请回到看看客户端首页查看原因（例如会员到期或长时间连不上看看服务器）。正在运行的任务不受影响。"),
        "not_running" => ("这个终端还没有启动", "请回到看看客户端，在终端列表里点“启动”。"),
        "upstream_down" => ("终端暂时没有响应", "请稍后刷新；如果一直这样，可以在看看客户端里重启这个终端。"),
        "port_reserved" => ("这个地址不能用作本机网页", "它是看看自己在使用的端口，请换一个网页地址。"),
        "page_down" => ("电脑上的这个网页没有在运行", "请先在电脑上启动它（比如运行 npm run dev），再刷新本页。"),
        "lan_expired" => (
            "局域网链接已失效 / Link expired",
            "这个链接只能用一次、1 分钟内有效。请回到远程页面，再点一次“改用局域网”。This one-time link has expired; go back to the remote page and switch again.",
        ),
        _ => ("出了点问题", "请稍后再试。"),
    }
}

pub fn message_page(status: StatusCode, kind: &str) -> Response {
    message_page_with(status, kind, &[])
}

pub fn message_page_with(status: StatusCode, kind: &str, extra: &[(&str, &str)]) -> Response {
    let (title, body) = text(kind);
    let detail: String = extra.iter().map(|(k, v)| format!("<p class=\"d\">{}: {}</p>", esc(k), esc(v))).collect();
    page(status, title, &format!("<h1>{title}</h1><p>{body}</p>{detail}"))
}

fn page(status: StatusCode, title: &str, inner: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title}</title><style>\
         :root{{color-scheme:light dark;--bg:#f7f8fc;--fg:#12141c;--mu:#5e6678;--card:#fff;--line:#e2e5ee;--in:#f0f2f7;--br:#4f46e5;--er:#d9364f}}\
         @media (prefers-color-scheme:dark){{:root{{--bg:#0e1016;--fg:#eceef3;--mu:#98a0b0;--card:#171a22;--line:#2a2f3b;--in:#1f232d;--br:#818cf8;--er:#f06a7e}}}}\
         *{{box-sizing:border-box}}\
         body{{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--fg);\
         font:16px/1.6 -apple-system,BlinkMacSystemFont,\"Segoe UI\",\"PingFang SC\",\"Microsoft YaHei\",sans-serif}}\
         .c{{width:calc(100% - 32px);max-width:420px;margin:16px;padding:28px;border-radius:16px;background:var(--card);border:1px solid var(--line)}}\
         h1{{font-size:19px;margin:0 0 8px}}p{{margin:0 0 10px;color:var(--mu)}}.d{{margin-top:10px;font-size:13px}}.e{{color:var(--er)}}\
         code{{font-size:14px;padding:1px 6px;border-radius:6px;background:var(--in);color:var(--fg)}}\
         input{{display:block;width:100%;margin:6px 0 14px;font:600 20px/1 ui-monospace,Menlo,Consolas,monospace;letter-spacing:.08em;\
         padding:12px 14px;min-height:52px;border-radius:12px;border:1px solid var(--line);background:var(--in);color:var(--fg);text-transform:uppercase}}\
         input:focus{{outline:2px solid var(--br);outline-offset:1px}}\
         button{{width:100%;min-height:48px;border:0;border-radius:12px;background:var(--br);color:#fff;font-family:inherit;font-size:16px;font-weight:600;cursor:pointer}}\
         </style></head><body><div class=\"c\">{inner}</div></body></html>"
    );
    let mut r = (status, html).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// 这个来源不允许直接打开管理台：告诉对方自己的 IP，方便在设置里允许。
pub fn denied_page(ip: &str) -> Response {
    let ip = esc(ip);
    page(
        StatusCode::FORBIDDEN,
        "不能从这台设备打开 / Not allowed",
        &format!(
            "<h1>不能从这台设备打开</h1>\
             <p>看看默认只允许装着它的那台电脑自己打开管理台。这台设备的 IP 是 <code>{ip}</code>。</p>\
             <p>如果这是你自己的设备：在装看看的电脑上打开“设置 → 访问控制”，开启“允许局域网访问”，或把这个 IP 加入白名单；\
             没有屏幕的机器（如 NAS）可以运行 <code>looklook access lan on</code>。</p>\
             <p>不在家时，请用你的专属地址打开。</p>\
             <p class=\"d\">This device ({ip}) is not allowed to open the Looklook console directly. \
             Allow LAN access or add this IP in Settings → Access on the computer running Looklook.</p>"
        ),
    )
}

/// 输入访问码（局域网 / 白名单访问且启用了访问码时）。
pub fn code_page(next: &str, error: Option<&str>) -> Response {
    let err = match error {
        Some("too_many") => "<p class=\"e\">输错次数太多，请 15 分钟后再试。</p>",
        Some(_) => "<p class=\"e\">访问码不对，请再看一下。</p>",
        None => "",
    };
    let status = if error.is_some() { StatusCode::UNAUTHORIZED } else { StatusCode::OK };
    page(
        status,
        "输入访问码 / Access code",
        &format!(
            "<h1>输入访问码</h1>\
             <p>这台电脑上的看看开启了访问码。访问码在那台电脑的“设置 → 访问控制”里，或运行 <code>looklook access</code> 查看。不区分大小写。</p>\
             {err}<form method=\"post\" action=\"{action}\"><input type=\"hidden\" name=\"next\" value=\"{next}\">\
             <input name=\"code\" autocomplete=\"off\" autocapitalize=\"characters\" spellcheck=\"false\" autofocus required aria-label=\"访问码\" placeholder=\"XXXX-XXXX-XXXX\">\
             <button type=\"submit\">进入</button></form>",
            action = super::access::FORM_PATH,
            next = esc(next),
        ),
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
