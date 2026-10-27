//! 常见服务商的快速配置：选一家、贴上 API Key 就能用。接口地址、Key 的发法、推荐的模型都填好了。
//!
//! - 地址 2026-10 逐个核对过：Claude Code 用的是各家的 Anthropic 兼容地址（`<地址>/v1/messages`），
//!   Codex 用的是 OpenAI Responses 兼容地址（`<地址>/responses`）。不带 Key 请求时回 401 / 403、
//!   而随便改一段路径就回 404 的，才算确认有这个接口。没有 Responses 接口的服务商（DeepSeek、硅基流动等）不给 Codex 用。
//! - 推荐的模型只是默认值，用户可以改。这家现在有哪些模型、什么价格由 models.dev 提供（[`crate::pricing`]），
//!   随它更新，不写死在这里。
//! - 这里的服务商没有合作关系，和平台下发的推荐（推广）分开显示。

use serde_json::{json, Value};

use crate::claude_profiles::Auth;

pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub name_en: &'static str,
    /// Claude Code 用的 Anthropic 兼容地址（`ANTHROPIC_BASE_URL`）
    pub claude: Option<&'static str>,
    /// Claude Code 的 Key 怎么发
    pub auth: Auth,
    /// 推荐的模型：主模型（`ANTHROPIC_MODEL` 和 Opus、Sonnet 档）和快的（Haiku 档，后台小任务用）；空表示用 Claude Code 自己的
    pub main: &'static str,
    pub fast: &'static str,
    /// Codex 用的 OpenAI Responses 兼容地址
    pub codex: Option<&'static str>,
    pub codex_model: &'static str,
    /// models.dev 里这家的 ID（模型列表、价格）
    pub catalog: &'static str,
    /// 去哪里创建 API Key
    pub key_url: &'static str,
}

const fn p(id: &'static str, name: &'static str, name_en: &'static str, catalog: &'static str, key_url: &'static str) -> Preset {
    Preset { id, name, name_en, claude: None, auth: Auth::Bearer, main: "", fast: "", codex: None, codex_model: "", catalog, key_url }
}

pub const PRESETS: &[Preset] = &[
    Preset { claude: Some("https://api.deepseek.com/anthropic"), main: "deepseek-v4-pro", fast: "deepseek-v4-flash", ..p("deepseek", "DeepSeek", "DeepSeek", "deepseek", "https://platform.deepseek.com/api_keys") },
    Preset {
        claude: Some("https://open.bigmodel.cn/api/anthropic"),
        main: "glm-5.3",
        fast: "glm-5.3-flash",
        codex: Some("https://open.bigmodel.cn/api/paas/v4"),
        codex_model: "glm-5.3",
        ..p("zhipu", "智谱 GLM", "Zhipu GLM", "zhipuai", "https://bigmodel.cn/usercenter/proj-mgmt/apikeys")
    },
    Preset {
        claude: Some("https://open.bigmodel.cn/api/anthropic"),
        main: "glm-5.3",
        fast: "glm-5.3-flash",
        codex: Some("https://open.bigmodel.cn/api/coding/paas/v4"),
        codex_model: "glm-5.3",
        ..p("zhipu-coding", "智谱 GLM Coding 套餐", "Zhipu GLM Coding Plan", "zhipuai-coding-plan", "https://bigmodel.cn/usercenter/proj-mgmt/apikeys")
    },
    Preset {
        claude: Some("https://api.moonshot.cn/anthropic"),
        main: "kimi-k3.1-pro",
        fast: "kimi-k2.6",
        codex: Some("https://api.moonshot.cn/v1"),
        codex_model: "kimi-k3.1-pro",
        ..p("kimi", "Kimi（月之暗面）", "Kimi (Moonshot)", "moonshotai-cn", "https://platform.moonshot.cn/console/api-keys")
    },
    Preset {
        claude: Some("https://api.kimi.com/coding"),
        codex: Some("https://api.kimi.com/coding/v1"),
        codex_model: "kimi-for-coding",
        ..p("kimi-coding", "Kimi Coding 套餐", "Kimi Coding Plan", "kimi-code-plan-cn", "https://www.kimi.com/code")
    },
    Preset { claude: Some("https://api.minimaxi.com/anthropic"), main: "MiniMax-M3", fast: "MiniMax-M3.1-Flash", ..p("minimax", "MiniMax", "MiniMax", "minimax-cn", "https://platform.minimaxi.com/user-center/basic-information/interface-key") },
    Preset {
        claude: Some("https://api.minimaxi.com/anthropic"),
        main: "MiniMax-M3",
        fast: "MiniMax-M3.1-Flash",
        ..p("minimax-coding", "MiniMax Coding 套餐", "MiniMax Coding Plan", "minimax-cn-coding-plan", "https://platform.minimaxi.com/user-center/basic-information/interface-key")
    },
    Preset {
        claude: Some("https://dashscope.aliyuncs.com/apps/anthropic"),
        main: "qwen3.8-max",
        fast: "qwen3.8-flash",
        codex: Some("https://dashscope.aliyuncs.com/compatible-mode/v1"),
        codex_model: "qwen3.8-max",
        ..p("bailian", "阿里云百炼", "Alibaba Cloud Model Studio", "alibaba-cn", "https://bailian.console.aliyun.com/?tab=model#/api-key")
    },
    Preset {
        claude: Some("https://coding.dashscope.aliyuncs.com/apps/anthropic"),
        main: "qwen3.7-plus",
        fast: "qwen3.7-plus",
        ..p("bailian-coding", "阿里云百炼 Coding Plan", "Model Studio Coding Plan", "alibaba-coding-plan-cn", "https://bailian.console.aliyun.com/?tab=model#/api-key")
    },
    Preset {
        claude: Some("https://ark.cn-beijing.volces.com/api/compatible"),
        main: "doubao-seed-2-1-pro-260628",
        fast: "doubao-seed-2-1-turbo-260628",
        codex: Some("https://ark.cn-beijing.volces.com/api/v3"),
        codex_model: "doubao-seed-2-1-pro-260628",
        ..p("ark", "火山方舟（豆包）", "Volcengine Ark", "volcengine", "https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey")
    },
    Preset { claude: Some("https://api.siliconflow.cn"), main: "zai-org/GLM-5.2", fast: "deepseek-ai/DeepSeek-V4-Flash", ..p("siliconflow", "硅基流动", "SiliconFlow", "siliconflow-cn", "https://cloud.siliconflow.cn/account/ak") },
    Preset { claude: Some("https://api.xiaomimimo.com/anthropic"), main: "mimo-v2.6-pro", fast: "mimo-v2.6-flash", ..p("mimo", "小米 MiMo", "Xiaomi MiMo", "xiaomi", "https://platform.xiaomimimo.com/#/console/api-keys") },
    Preset {
        claude: Some("https://openrouter.ai/api"),
        codex: Some("https://openrouter.ai/api/v1"),
        codex_model: "openai/gpt-6.1-sol",
        ..p("openrouter", "OpenRouter", "OpenRouter", "openrouter", "https://openrouter.ai/settings/keys")
    },
    Preset { claude: Some("https://api.anthropic.com"), auth: Auth::ApiKey, ..p("anthropic", "Anthropic API", "Anthropic API", "anthropic", "https://console.anthropic.com/settings/keys") },
    Preset { codex: Some("https://api.openai.com/v1"), ..p("openai", "OpenAI API", "OpenAI API", "openai", "https://platform.openai.com/api-keys") },
];

pub fn get(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// 给某个助手（`claude` / `codex`）用的预设，带上 models.dev 里这家现在的模型（按发布日期新的在前，带价格）
pub fn list(agent: &str, en: bool) -> Vec<Value> {
    let cat = crate::pricing::catalog();
    PRESETS
        .iter()
        .filter_map(|p| {
            let base_url = if agent == "codex" { p.codex? } else { p.claude? };
            let mut models: Vec<Value> = cat
                .as_ref()
                .and_then(|c| c.providers.get(p.catalog))
                .map(|prov| {
                    let mut v: Vec<_> = prov.models.iter().collect();
                    v.sort_by(|a, b| b.1.release_date.cmp(&a.1.release_date));
                    v.into_iter().take(40).map(|(id, m)| json!({ "id": id, "name": m.name, "cost": m.cost, "release_date": m.release_date })).collect()
                })
                .unwrap_or_default();
            models.retain(|m| crate::model::clean_model(m["id"].as_str()).is_ok());
            Some(json!({
                "id": p.id,
                "name": if en { p.name_en } else { p.name },
                "base_url": base_url,
                "auth": if agent == "claude" { json!(p.auth) } else { Value::Null },
                "models": if agent == "codex" { json!({ "main": p.codex_model }) } else { json!({ "main": p.main, "fast": p.fast }) },
                "catalog": p.catalog,
                "key_url": p.key_url,
                "available": models,
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_valid() {
        let mut ids = std::collections::HashSet::new();
        for p in PRESETS {
            assert!(ids.insert(p.id), "{} 重复", p.id);
            assert!(p.claude.is_some() || p.codex.is_some(), "{}", p.id);
            for u in [p.claude, p.codex].into_iter().flatten() {
                crate::model::check_base_url(u).unwrap_or_else(|_| panic!("{u}"));
            }
            for m in [p.main, p.fast, p.codex_model] {
                assert_eq!(crate::model::clean_model(Some(m)).unwrap().as_deref().unwrap_or(""), m, "{}", p.id);
            }
            assert!(p.key_url.starts_with("https://"));
        }
        assert_eq!(list("codex", false).len(), PRESETS.iter().filter(|p| p.codex.is_some()).count());
        assert!(list("claude", true).iter().any(|p| p["name"] == "Zhipu GLM"));
    }
}
