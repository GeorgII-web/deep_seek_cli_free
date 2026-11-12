use crate::auth::Auth;
use crate::config::Deepseek as Settings;
use anyhow::{Context, Result, anyhow};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const BASE: &str = "https://chat.deepseek.com";
/// Environment variable that points the client at another server: the local
/// mock of `scripts/mock-deepseek.py`, so tests never reach DeepSeek.
pub const BASE_ENV: &str = "DSC_API_BASE";
/// A challenge with this algorithm needs no proof of work; only the mock sends it.
pub const NO_POW: &str = "none";

/// Set to `1` to allow the real API in a run that looks like a test (see `base`).
pub const LIVE_ENV: &str = "DSC_LIVE";

/// The server all calls go to: `DSC_API_BASE` when set (the mock), else DeepSeek.
///
/// The real API is refused wherever a run is a test, so that no test can reach
/// the user's account by mistake:
/// - in the unit tests, always;
/// - with a custom home (`DSC_HOME`, which only tests set), unless `DSC_LIVE=1` says it is meant.
pub fn base() -> Result<String> {
    choose_base(
        std::env::var(BASE_ENV).ok().as_deref(),
        std::env::var_os("DSC_HOME").is_some(),
        std::env::var(LIVE_ENV).is_ok_and(|live| live == "1"),
        cfg!(test),
    )
}

/// The rule of `base`, without the environment.
fn choose_base(
    mock: Option<&str>,
    custom_home: bool,
    live: bool,
    unit_test: bool,
) -> Result<String> {
    if let Some(url) = mock.filter(|url| !url.is_empty()) {
        return Ok(url.trim_end_matches('/').to_string());
    }
    if unit_test {
        return Err(anyhow!("the unit tests never use the real DeepSeek API"));
    }
    if custom_home && !live {
        return Err(anyhow!(
            "DSC_HOME is set, which marks a test run: the real DeepSeek API is off. \
             Set {BASE_ENV} to a mock server, or {LIVE_ENV}=1 to use the real API on purpose"
        ));
    }
    Ok(BASE.into())
}

/// Prompts sent to the model by this process: one per answer asked for.
static REQUESTS: AtomicU64 = AtomicU64::new(0);

/// How many prompts this process has sent to the model so far: one request and
/// one answer each, which is what the user sees and what the throttle counts.
/// The difference around a task is what the task cost the account. The helper
/// calls behind a prompt (the proof-of-work challenge before each one, the
/// session at the start of a conversation) are not counted.
pub fn requests() -> u64 {
    REQUESTS.load(Ordering::Relaxed)
}

pub const COMPLETION_PATH: &str = "/api/v0/chat/completion";
/// Upload of an attached file; it needs a proof of work of its own, made for this path.
pub const UPLOAD_PATH: &str = "/api/v0/file/upload_file";
/// Status of uploaded files: DeepSeek reads (parses) a file before a prompt can use it.
pub const FETCH_FILES_PATH: &str = "/api/v0/file/fetch_files";
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

/// An uploaded file as DeepSeek reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteFile {
    pub id: String,
    /// Upper case: `PENDING`, `PARSING`, `SUCCESS`, or a failure (`PARSE_FAILED`, `FAILED`, …)
    pub status: String,
    /// Error code or reason DeepSeek gave, when any
    pub error: Option<String>,
}

impl RemoteFile {
    pub fn ready(&self) -> bool {
        self.status == "SUCCESS"
    }

    pub fn failed(&self) -> bool {
        self.status.contains("FAIL") || self.status.contains("ERROR")
    }
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
    /// See `base()`
    base: String,
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
        let base = base()?;
        set("origin", &base, false)?;
        set("referer", &format!("{base}/"), false)?;

        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(settings.timeout_secs))
            .build()?;
        Ok(Self {
            http,
            base,
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
        let mut request = self.http.post(format!("{}{path}", self.base)).json(body);
        if let Some(pow) = pow {
            request = request.header("x-ds-pow-response", pow);
        }
        let response = request
            .send()
            .await
            .with_context(|| format!("POST {path}"))?;
        checked("POST", path, response)
    }

    /// The JSON envelope of a response, unwrapped.
    async fn biz(path: &str, response: reqwest::Response) -> Result<Value> {
        let text = response.text().await?;
        let envelope: Value = serde_json::from_str(&text)
            .map_err(|_| anyhow!("{path}: the answer is not JSON (captcha or block page?)"))?;
        biz_data(path, envelope)
    }

    /// POST that answers with the JSON envelope `{code, msg, data: {biz_code, biz_msg, biz_data}}`.
    async fn post_biz(&self, path: &str, body: &Value) -> Result<Value> {
        Self::biz(path, self.post(path, body, None).await?).await
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

    /// A proof-of-work challenge for the call to `target_path` (`COMPLETION_PATH`, `UPLOAD_PATH`).
    pub async fn create_challenge(&self, target_path: &str) -> Result<Challenge> {
        let body = json!({ "target_path": target_path });
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
        ref_file_ids: &[String],
        opts: &TurnOpts,
        pow_header: &str,
    ) -> Result<reqwest::Response> {
        let body = json!({
            "chat_session_id": session_id,
            "parent_message_id": parent_message_id,
            "model_type": opts.model_type,
            "prompt": prompt,
            "ref_file_ids": ref_file_ids,
            "thinking_enabled": opts.thinking,
            "search_enabled": opts.search,
        });
        REQUESTS.fetch_add(1, Ordering::Relaxed);
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

    /// Uploads one file (multipart field `file`); `pow_header` is solved for `UPLOAD_PATH`.
    /// Returns the file id; DeepSeek still has to read the file (`fetch_files`).
    pub async fn upload_file(
        &self,
        name: &str,
        bytes: Vec<u8>,
        mime: &str,
        pow_header: &str,
    ) -> Result<String> {
        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(name.to_string())
            .mime_str(mime)?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let response = self
            .http
            .post(format!("{}{UPLOAD_PATH}", self.base))
            .header("x-ds-pow-response", pow_header)
            .multipart(form)
            .send()
            .await
            .with_context(|| format!("upload {name}"))?;
        let data = Self::biz(UPLOAD_PATH, checked("POST", UPLOAD_PATH, response)?).await?;
        uploaded_id(&data).with_context(|| format!("upload {name}: no file id in the answer"))
    }

    /// The status of uploaded files; files DeepSeek does not know are left out.
    pub async fn fetch_files(&self, ids: &[String]) -> Result<Vec<RemoteFile>> {
        let query: Vec<String> = ids
            .iter()
            .map(|id| format!("file_ids={}", url_encode(id)))
            .collect();
        let url = format!("{}{FETCH_FILES_PATH}?{}", self.base, query.join("&"));
        let response = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {FETCH_FILES_PATH}"))?;
        let data = Self::biz(
            FETCH_FILES_PATH,
            checked("GET", FETCH_FILES_PATH, response)?,
        )
        .await?;
        Ok(remote_files(&data))
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

/// The HTTP status check of every call: 401/403 = dead token, 429 = rate limited.
fn checked(method: &str, path: &str, response: reqwest::Response) -> Result<reqwest::Response> {
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
        _ => Err(anyhow!("{method} {path}: HTTP {status}")),
    }
}

/// The id in the answer to an upload: `id`, `file_id` or `file.id` (the shape has changed before).
fn uploaded_id(data: &Value) -> Option<String> {
    data.get("id")
        .or_else(|| data.get("file_id"))
        .or_else(|| data.pointer("/file/id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

/// The files in the answer of `fetch_files`: a list under `files`, a bare list, or a map by id.
fn remote_files(data: &Value) -> Vec<RemoteFile> {
    let entries: Vec<(Option<String>, &Value)> = match data.get("files").unwrap_or(data) {
        Value::Array(list) => list.iter().map(|file| (None, file)).collect(),
        Value::Object(map) => map
            .iter()
            .map(|(id, file)| (Some(id.clone()), file))
            .collect(),
        _ => Vec::new(),
    };
    let text = |file: &Value, key: &str| match file.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        Some(Value::Number(number)) => Some(number.to_string()),
        _ => None,
    };
    entries
        .into_iter()
        .filter_map(|(key, file)| {
            Some(RemoteFile {
                id: text(file, "id").or(key)?,
                status: text(file, "status").unwrap_or_default().to_uppercase(),
                error: text(file, "error_reason").or_else(|| text(file, "error_code")),
            })
        })
        .collect()
}

/// Percent-encoding for a query value (file ids are plain, but who knows).
fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
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

    #[test]
    fn upload_answers_in_every_known_shape() {
        assert_eq!(
            uploaded_id(&json!({"id":"f1","status":"PENDING"})).as_deref(),
            Some("f1")
        );
        assert_eq!(uploaded_id(&json!({"file_id":"f2"})).as_deref(), Some("f2"));
        assert_eq!(
            uploaded_id(&json!({"file":{"id":"f3"}})).as_deref(),
            Some("f3")
        );
        assert_eq!(uploaded_id(&json!({"id":""})), None);
        let list = remote_files(&json!({"files":[
            {"id":"a","status":"success"},
            {"id":"b","status":"PARSE_FAILED","error_code":"PARSE_FAILED"},
            {"id":"c","status":"PENDING"}
        ]}));
        assert!(list[0].ready() && !list[0].failed());
        assert!(list[1].failed());
        assert_eq!(list[1].error.as_deref(), Some("PARSE_FAILED"));
        assert!(!list[2].ready() && !list[2].failed());
        let map = remote_files(&json!({"x":{"status":"SUCCESS"}}));
        assert_eq!(map[0].id, "x");
        assert_eq!(url_encode("a b/c"), "a%20b%2Fc");
    }

    #[test]
    fn the_real_api_is_refused_in_anything_that_looks_like_a_test() {
        let mock = Some("http://127.0.0.1:8765/");
        // A mock address always wins.
        assert_eq!(
            choose_base(mock, true, false, true).unwrap(),
            "http://127.0.0.1:8765"
        );
        // A normal run of the installed app.
        assert_eq!(choose_base(None, false, false, false).unwrap(), BASE);
        // The unit tests, with or without the switch.
        assert!(choose_base(None, false, true, true).is_err());
        // A custom home: only on purpose.
        assert!(choose_base(None, true, false, false).is_err());
        assert_eq!(choose_base(None, true, true, false).unwrap(), BASE);
        assert!(choose_base(Some(""), true, false, false).is_err());
        // And the real environment of this test run.
        assert!(base().is_err() || std::env::var(BASE_ENV).is_ok());
    }
}
