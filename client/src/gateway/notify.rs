//! 通知设置（docs/TASK_BOARD.md §6.1、§6.5）：设置页“通知”分区的接口，以及电脑之间转发飞书卡片操作。

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::require_allowed;
use super::{Access, App};
use crate::error::LocalError;
use crate::notify::{self, Feishu, Rules};
use crate::push::{self, Hook, Kind};

type R<T> = Result<T, LocalError>;

fn view(app: &App) -> Value {
    let f = notify::load_feishu(&app.store);
    let now = app.account.clock.now_ms();
    let st = app.notify.status();
    json!({
        "feishu": f.as_ref().map(|f| json!({
            "app_id": f.app_id,
            "domain": if f.domain.is_empty() { "feishu" } else { &f.domain },
            "bound": f.open_id.is_some(),
            "bound_at": f.bound_at,
            "callback_ok": f.callback_ok,
        })),
        "rules": notify::load_rules(&app.store),
        "status": st,
        // 地址和 key 只给遮住的样子
        "hooks": notify::load_hooks(&app.store).iter().map(|h| json!({
            "id": h.id,
            "kind": h.kind,
            "name": h.name,
            "target": h.masked(),
            "has_secret": !h.secret.is_empty(),
            "status": app.notify.hook_status(&h.id),
        })).collect::<Vec<_>>(),
        // 没绑定时才显示绑定码（绑定后换人也可以再发一次码）
        "pair": f.as_ref().map(|f| json!({ "code": notify::pair_code(f, now), "expires_at": notify::pair_expires(now) })),
        "sync_enabled": crate::settings_sync::enabled(&app.store),
        // 扫码建应用的进度（这台电脑上的）
        "register": app.notify.reg(),
        // AI 工具用你本人的身份调飞书：这台电脑上授权了没有（令牌不同步）
        "user": crate::feishu_tools::view(app),
    })
}

pub async fn get(State(app): State<App>) -> Json<Value> {
    Json(view(&app))
}

#[derive(Deserialize)]
pub struct FeishuReq {
    app_id: String,
    /// 不改 Secret 时不传
    #[serde(default)]
    app_secret: Option<String>,
    #[serde(default)]
    domain: String,
}

/// 填好 App ID / Secret：先换一次令牌，对了才存
pub async fn put_feishu(State(app): State<App>, Json(req): Json<FeishuReq>) -> R<Json<Value>> {
    require_allowed(&app)?;
    let old = notify::load_feishu(&app.store);
    let app_id = req.app_id.trim().to_string();
    let secret = req.app_secret.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).or_else(|| old.as_ref().map(|o| o.app_secret.clone())).unwrap_or_default();
    if app_id.is_empty() || app_id.len() > 64 || !app_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(LocalError::invalid("app_id", "invalid"));
    }
    if secret.is_empty() || secret.len() > 128 {
        return Err(LocalError::invalid("app_secret", "required"));
    }
    let domain = if req.domain == "lark" { "lark" } else { "feishu" }.to_string();
    let api = crate::feishu::Api::new(reqwest::Client::new(), &domain, &app_id, &secret);
    api.check().await.map_err(|e| notify::feishu_error(&e))?;
    // 换了应用：之前绑定的人在新应用里的 open_id 不一样，要重新绑定
    let same_app = old.as_ref().is_some_and(|o| o.app_id == app_id && o.domain == domain);
    let f = match old {
        Some(o) if same_app => Feishu { app_secret: secret, ..o },
        _ => Feishu { app_id, app_secret: secret, domain, ..Default::default() },
    };
    notify::save_feishu(&app, Some(&f))?;
    app.notify.reset();
    Ok(Json(view(&app)))
}

#[derive(Deserialize)]
pub struct RegisterReq {
    /// 给现在配的应用补权限，而不是建新的
    #[serde(default)]
    update: bool,
}

/// 扫码建应用：拿一个给飞书扫的链接，之后设置页轮询 `register.state`
pub async fn register(State(app): State<App>, Json(req): Json<RegisterReq>) -> R<Json<Value>> {
    require_allowed(&app)?;
    app.notify.reg_start(&app, req.update).await?;
    Ok(Json(view(&app)))
}

pub async fn register_cancel(State(app): State<App>) -> R<Json<Value>> {
    require_allowed(&app)?;
    app.notify.reg_cancel();
    Ok(Json(view(&app)))
}

/// 让 AI 工具用你的身份调飞书：开始授权（飞书私聊里也会收到一张授权卡片）
pub async fn user_auth(State(app): State<App>) -> R<Json<Value>> {
    require_allowed(&app)?;
    crate::feishu_tools::start(&app).await?;
    Ok(Json(view(&app)))
}

/// 撤销：删掉这台电脑上的用户令牌
pub async fn user_revoke(State(app): State<App>) -> R<Json<Value>> {
    require_allowed(&app)?;
    crate::feishu_tools::revoke(&app);
    Ok(Json(view(&app)))
}

/// 断开飞书（所有电脑上都清掉）
pub async fn delete_feishu(State(app): State<App>) -> R<Json<Value>> {
    require_allowed(&app)?;
    notify::save_feishu(&app, None)?;
    app.notify.reset();
    Ok(Json(view(&app)))
}

pub async fn unbind(State(app): State<App>) -> R<Json<Value>> {
    require_allowed(&app)?;
    if let Some(f) = notify::load_feishu(&app.store) {
        notify::save_feishu(&app, Some(&Feishu { open_id: None, bound_at: None, callback_ok: false, ..f }))?;
    }
    Ok(Json(view(&app)))
}

pub async fn put_rules(State(app): State<App>, Json(mut r): Json<Rules>) -> R<Json<Value>> {
    require_allowed(&app)?;
    r.muted.retain(|m| !m.is_empty() && m.len() <= 1024);
    r.muted.sort();
    r.muted.dedup();
    r.muted.truncate(200);
    if !matches!(r.lang.as_str(), "zh-CN" | "en-US") {
        r.lang = "zh-CN".into();
    }
    if r.quiet.as_ref().is_some_and(|q| !q.valid()) {
        return Err(LocalError::invalid("quiet", "invalid"));
    }
    if !matches!(r.done_batch, 0 | 10 | 30 | 60) {
        return Err(LocalError::invalid("done_batch", "invalid"));
    }
    r.stall_min = r.stall_min.min(240);
    notify::save_rules(&app, &r)?;
    Ok(Json(view(&app)))
}

#[derive(Deserialize)]
pub struct HookReq {
    /// 改已有的一项时带上；不带 = 新加
    #[serde(default)]
    id: Option<String>,
    kind: Kind,
    /// 改的时候不传 = 不变
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    name: String,
}

/// 加一个只通知的渠道，或者改名字、换地址
pub async fn put_hook(State(app): State<App>, Json(req): Json<HookReq>) -> R<Json<Value>> {
    require_allowed(&app)?;
    let mut hooks = notify::load_hooks(&app.store);
    let old = req.id.as_ref().and_then(|id| hooks.iter().find(|h| &h.id == id).cloned());
    if req.id.is_some() && old.is_none() {
        return Err(LocalError::new("NOT_FOUND"));
    }
    let trim = |s: Option<String>| s.map(|s| s.trim().to_string());
    let same_kind = old.as_ref().is_some_and(|o| o.kind == req.kind);
    let target = trim(req.target).filter(|s| !s.is_empty()).or_else(|| old.as_ref().filter(|_| same_kind).map(|o| o.target.clone())).unwrap_or_default();
    let secret = match trim(req.secret) {
        Some(s) => s,
        None => old.as_ref().filter(|_| same_kind).map(|o| o.secret.clone()).unwrap_or_default(),
    };
    let id = old.as_ref().map(|o| o.id.clone()).unwrap_or_else(|| format!("h{:x}", rand::random::<u32>()));
    let h = Hook { id, kind: req.kind, target, secret, name: req.name.trim().chars().take(40).collect() };
    h.validate().map_err(|f| LocalError::invalid(f, "invalid"))?;
    match hooks.iter().position(|x| x.id == h.id) {
        Some(i) => hooks[i] = h,
        None if hooks.len() >= push::MAX_HOOKS => return Err(LocalError::invalid("hooks", "max")),
        None => hooks.push(h),
    }
    notify::save_hooks(&app, &hooks)?;
    Ok(Json(view(&app)))
}

pub async fn delete_hook(State(app): State<App>, Path(id): Path<String>) -> R<Json<Value>> {
    require_allowed(&app)?;
    let mut hooks = notify::load_hooks(&app.store);
    hooks.retain(|h| h.id != id);
    notify::save_hooks(&app, &hooks)?;
    Ok(Json(view(&app)))
}

pub async fn test_hook(State(app): State<App>, Path(id): Path<String>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.notify.test_hook(&app, &id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn test(State(app): State<App>) -> R<StatusCode> {
    require_allowed(&app)?;
    app.notify.test(&app).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// 另一台电脑转来的卡片操作：只接受经 WebRTC 的请求，还要带那台电脑的设备签名
pub async fn action(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<notify::ActionReq>) -> R<Json<Value>> {
    if !matches!(access, Access::Rtc { .. }) {
        return Err(LocalError::new("SYNC_NEED_RTC"));
    }
    require_allowed(&app)?;
    Ok(Json(notify::receive(&app, req).await?))
}

/// 另一台电脑转来的“在飞书里和 AI 对话”请求（docs/FEISHU_AGENT.md）：同样只接受经 WebRTC、带设备签名的
pub async fn agent(State(app): State<App>, Extension(access): Extension<Access>, Json(req): Json<notify::ActionReq>) -> R<Json<Value>> {
    if !matches!(access, Access::Rtc { .. }) {
        return Err(LocalError::new("SYNC_NEED_RTC"));
    }
    require_allowed(&app)?;
    Ok(Json(crate::feishu_agent::receive(&app, req).await?))
}
