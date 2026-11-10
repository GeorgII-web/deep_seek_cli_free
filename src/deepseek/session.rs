//! One conversation with DeepSeek: session id, message chain, proof of work per turn.

use super::client::{ApiError, Client, TurnOpts};
use super::{pow, throttle};
use crate::config::{Paths, Throttle};
use anyhow::{Context, Result};
use std::fs;
use std::time::Duration;

pub struct Conversation {
    client: Client,
    paths: Paths,
    throttle: Throttle,
    wasm: Option<Vec<u8>>,
    session_id: Option<String>,
    /// Id of the last answer; the next prompt hangs below it
    pub parent_message_id: Option<u64>,
}

impl Conversation {
    pub fn new(client: Client, paths: Paths, throttle: Throttle) -> Self {
        Self {
            client,
            paths,
            throttle,
            wasm: None,
            session_id: None,
            parent_message_id: None,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// The proof-of-work module: memory, then `<home>/cache/sha3.wasm` when it was
    /// downloaded from the current URL, else a download.
    pub async fn wasm(&mut self) -> Result<&[u8]> {
        if self.wasm.is_none() {
            let dir = self.paths.cache();
            let (file, url_file) = (dir.join("sha3.wasm"), dir.join("sha3.url"));
            let url = self.client.wasm_url();
            let cached = fs::read_to_string(&url_file).is_ok_and(|saved| saved == url);
            let bytes = match fs::read(&file) {
                Ok(bytes) if cached => bytes,
                _ => {
                    let bytes = self.client.fetch_wasm().await?;
                    fs::create_dir_all(&dir)?;
                    fs::write(&file, &bytes)?;
                    fs::write(&url_file, url)?;
                    bytes
                }
            };
            self.wasm = Some(bytes);
        }
        Ok(self.wasm.as_deref().unwrap())
    }

    /// Fetches and solves a challenge; returns the `X-DS-PoW-Response` value.
    pub async fn pow_header(&mut self) -> Result<String> {
        let challenge = self.client.create_challenge().await?;
        let wasm = self.wasm().await?.to_vec();
        let solved = challenge.clone();
        let answer = tokio::task::spawn_blocking(move || pow::solve(&wasm, &solved))
            .await
            .context("proof-of-work task")??;
        Ok(pow::header(&challenge, answer))
    }

    /// Sends `prompt` as the next message; the response body is the SSE stream.
    /// Throttled; HTTP 429 is retried as the settings say.
    pub async fn send(&mut self, prompt: &str, opts: &TurnOpts) -> Result<reqwest::Response> {
        let mut retries = self.throttle.retries_on_429;
        loop {
            throttle::acquire(&self.paths, &self.throttle).await;
            match self.send_once(prompt, opts).await {
                Err(err) => match err.downcast_ref::<ApiError>() {
                    Some(ApiError::RateLimited(after)) if retries > 0 => {
                        retries -= 1;
                        let secs = after.unwrap_or(self.throttle.wait_on_429_secs);
                        eprintln!("rate limited by DeepSeek, retry in {secs} s");
                        tokio::time::sleep(Duration::from_secs(secs)).await;
                    }
                    _ => return Err(err),
                },
                ok => return ok,
            }
        }
    }

    async fn send_once(&mut self, prompt: &str, opts: &TurnOpts) -> Result<reqwest::Response> {
        let session_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                let id = self.client.create_session().await?;
                self.session_id = Some(id.clone());
                id
            }
        };
        let pow = self.pow_header().await?;
        self.client
            .completion(&session_id, self.parent_message_id, prompt, opts, &pow)
            .await
    }
}
