//! 通知设置（docs/TASK_BOARD.md §6.1、§6.5）：设置页“通知”分区的接口，以及电脑之间转发飞书卡片操作。

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use super::api::require_allowed;
use super::{Access, App};
use crate::error::LocalError;
use crate::notify::{self, Feishu, Rules};

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
        // 没绑定时才显示绑定码（绑定后换人也可以再发一次码）
        "pair": f.as_ref().map(|f| json!({ "code": notify::pair_code(f, now), "expires_at": notify::pair_expires(now) })),
        "sync_enabled": crate::settings_sync::enabled(&app.store),
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
    notify::save_rules(&app, &r)?;
    Ok(Json(view(&app)))
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
