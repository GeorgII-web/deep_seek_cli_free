use crate::auth::Auth;
use crate::config::Deepseek as Settings;
use anyhow::{Context, Result, anyhow};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

pub const BASE: &str = "https://chat.deepseek.com";
pub const COMPLETION_PATH: &str = "/api/v0/chat/completion";
/// Used when the login source did not give a wasm URL.
pub const DEFAULT_WASM_URL: &str =
    "https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm";
const CLIENT_VERSION: &str = "2.0.0";
const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// Failures the caller reacts to; everything else is a plain `anyhow` error.
#[derive(Debug)]
pub enum ApiError {
    /// HTTP 401/403 or an auth error code: the user must log in again
    TokenDead(String),
    /// HTTP 429, with `Retry-After` in seconds when the server sent it
    RateLimited(Option<u64>),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TokenDead(why) => write!(f, "token dead ({why}); run `dsc login`"),
            Self::RateLimited(Some(secs)) => write!(f, "rate limited, retry after {secs} s"),
            Self::RateLimited(None) => write!(f, "rate limited"),
        }
    }
}

impl std::error::Error for ApiError {}

#[derive(Debug, Clone, Deserialize)]
pub struct Challenge {
    pub algorithm: String,
    pub challenge: String,
    pub salt: String,
    pub difficulty: u64,
    pub expire_at: u64,
    pub signature: String,
}

/// Flags of one completion call.
#[derive(Debug, Clone)]
pub struct TurnOpts {
    pub model_type: String,
    pub thinking: bool,
    pub search: bool,
}

pub struct Client {
    http: reqwest::Client,
    wasm_url: String,
}

impl Client {
    pub fn new(auth: &Auth, settings: &Settings) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let mut set = |name: &'static str, value: &str, secret: bool| -> Result<()> {
            let mut value = HeaderValue::from_str(value).map_err(|_| {
                anyhow!("header {name}: value has characters not allowed in a header")
            })?;
            value.set_sensitive(secret);
            headers.insert(HeaderName::from_static(name), value);
            Ok(())
        };
        set("authorization", &format!("Bearer {}", auth.token), true)?;
        if !auth.cookie.is_empty() {
            set("cookie", &auth.cookie, true)?;
        }
        set("x-client-platform", "web", false)?;
        set("x-client-version", CLIENT_VERSION, false)?;
        set("x-app-version", CLIENT_VERSION, false)?;
        set("x-client-locale", &settings.locale, false)?;
        set("x-client-timezone-offset", "0", false)?;
        set("origin", BASE, false)?;
        set("referer", &format!("{BASE}/"), false)?;

        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(settings.timeout_secs))
            .build()?;
        Ok(Self {
            http,
            wasm_url: auth
                .wasm_url
                .clone()
                .unwrap_or_else(|| DEFAULT_WASM_URL.into()),
        })
    }

    pub fn wasm_url(&self) -> &str {
        &self.wasm_url
    }

    /// POST with a JSON body; returns the response after the HTTP status check.
    async fn post(&self, path: &str, body: &Value, pow: Option<&str>) -> Result<reqwest::Response> {
        let mut request = self.http.post(format!("{BASE}{path}")).json(body);
        if let Some(pow) = pow {
            request = request.header("x-ds-pow-response", pow);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        let status = response.status().as_u16();
        match status {
            200..=299 => Ok(response),
            401 | 403 => Err(ApiError::TokenDead(format!("HTTP {status}")).into()),
            429 => {
                let secs = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok());
                Err(ApiError::RateLimited(secs).into())
            }
            _ => Err(anyhow!("POST {path}: HTTP {status}")),
        }
    }

    /// POST that answers with the JSON envelope `{code, msg, data: {biz_code, biz_msg, biz_data}}`.
    async fn post_biz(&self, path: &str, body: &Value) -> Result<Value> {
        let text = self.post(path, body, None).await?.text().await?;
        let envelope: Value = serde_json::from_str(&text)
            .map_err(|_| anyhow!("POST {path}: the answer is not JSON (captcha or block page?)"))?;
        biz_data(path, envelope)
    }

    pub async fn create_session(&self) -> Result<String> {
        let data = self
            .post_biz("/api/v0/chat_session/create", &json!({}))
            .await?;
        data.pointer("/chat_session/id")
            .or_else(|| data.get("id"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("session create: no session id in the answer")
    }

    pub async fn create_challenge(&self) -> Result<Challenge> {
        let body = json!({ "target_path": COMPLETION_PATH });
        let data = self
            .post_biz("/api/v0/chat/create_pow_challenge", &body)
            .await?;
        let challenge = data
            .get("challenge")
            .cloned()
            .context("no challenge in the answer")?;
        serde_json::from_value(challenge).context("challenge has an unknown shape")
    }

    /// Starts a completion; the body of the returned response is the SSE stream.
    pub async fn completion(
        &self,
        session_id: &str,
        parent_message_id: Option<u64>,
        prompt: &str,
        opts: &TurnOpts,
        pow_header: &str,
    ) -> Result<reqwest::Response> {
        let body = json!({
            "chat_session_id": session_id,
            "parent_message_id": parent_message_id,
            "model_type": opts.model_type,
            "prompt": prompt,
            "ref_file_ids": [],
            "thinking_enabled": opts.thinking,
            "search_enabled": opts.search,
        });
        let response = self.post(COMPLETION_PATH, &body, Some(pow_header)).await?;
        let is_json = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("application/json"));
        if is_json {
            // An error comes as the JSON envelope instead of a stream.
            let envelope: Value = response
                .json()
                .await
                .context("completion: unreadable answer")?;
            biz_data(COMPLETION_PATH, envelope)?;
            return Err(anyhow!("completion: JSON answer instead of a stream"));
        }
        Ok(response)
    }

    pub async fn fetch_wasm(&self) -> Result<Vec<u8>> {
        // A plain client: the static host must not get the token.
        let response = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(60))
            .build()?
            .get(&self.wasm_url)
            .send()
            .await
            .context("download the proof-of-work module")?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "proof-of-work module: HTTP {}",
                response.status().as_u16()
            ));
        }
        Ok(response.bytes().await?.to_vec())
    }
}

/// Unwraps the envelope; error codes become `ApiError` or a message.
fn biz_data(path: &str, mut envelope: Value) -> Result<Value> {
    let code = envelope.get("code").and_then(Value::as_i64).unwrap_or(-1);
    let msg = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    if code != 0 {
        let text = msg(&envelope, "msg");
        // 40002 missing token, 40003 invalid token
        if matches!(code, 40002 | 40003) {
            return Err(ApiError::TokenDead(format!("code {code} {text}")).into());
        }
        return Err(anyhow!("POST {path}: code {code} {text}"));
    }
    let data = envelope
        .get_mut("data")
        .map(Value::take)
        .unwrap_or(Value::Null);
    let biz_code = data.get("biz_code").and_then(Value::as_i64).unwrap_or(0);
    if biz_code != 0 {
        return Err(anyhow!(
            "POST {path}: biz_code {biz_code} {}",
            msg(&data, "biz_msg")
        ));
    }
    Ok(data.get("biz_data").cloned().unwrap_or(Value::Null))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_ok_gives_biz_data() {
        let v =
            json!({"code":0,"msg":"","data":{"biz_code":0,"biz_msg":"","biz_data":{"id":"s1"}}});
        assert_eq!(biz_data("/x", v).unwrap(), json!({"id":"s1"}));
    }

    #[test]
    fn invalid_token_code_is_token_dead() {
        let v = json!({"code":40003,"msg":"Invalid token","data":null});
        let err = biz_data("/x", v).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<ApiError>(),
            Some(ApiError::TokenDead(_))
        ));
    }

    #[test]
    fn biz_error_keeps_the_message() {
        let v = json!({"code":0,"data":{"biz_code":5,"biz_msg":"too fast","biz_data":null}});
        assert!(
            biz_data("/x", v)
                .unwrap_err()
                .to_string()
                .contains("too fast")
        );
    }
}
