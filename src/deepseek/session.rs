//! One conversation with DeepSeek: session id, message chain, proof of work per turn.

use super::client::{ApiError, Client, TurnOpts};
use super::stream::Parser;
use super::{pow, throttle};
use crate::backend::{Backend, Event};
use crate::config::{Paths, Throttle};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::fs;
use std::time::Duration;

pub struct Conversation {
    client: Client,
    paths: Paths,
    throttle: Throttle,
    opts: TurnOpts,
    wasm: Option<Vec<u8>>,
    session_id: Option<String>,
    /// Id of the last answer; the next prompt hangs below it
    parent_message_id: Option<u64>,
}

impl Conversation {
    pub fn new(client: Client, paths: Paths, throttle: Throttle, opts: TurnOpts) -> Self {
        Self {
            client,
            paths,
            throttle,
            opts,
            wasm: None,
            session_id: None,
            parent_message_id: None,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Continues the same conversation with another login (a new token of the same account).
    pub fn set_client(&mut self, client: Client) {
        self.client = client;
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
    /// Throttled; HTTP 429 is retried as the settings say. `sink` gets the progress.
    async fn send(
        &mut self,
        prompt: &str,
        sink: &mut dyn FnMut(&Event),
    ) -> Result<reqwest::Response> {
        let mut retries = self.throttle.retries_on_429;
        loop {
            let mut on_wait = |wait: Duration| {
                if wait >= Duration::from_secs(1) {
                    sink(&Event::Status(format!(
                        "Pausing {} s (throttle)",
                        wait.as_secs()
                    )));
                }
            };
            throttle::acquire(&self.paths, &self.throttle, &mut on_wait).await;
            match self.send_once(prompt, sink).await {
                Err(err) => match err.downcast_ref::<ApiError>() {
                    Some(ApiError::RateLimited(after)) => {
                        // Each further "too many requests" doubles the pause. It is also
                        // written down, so a new run does not start right into the limit.
                        let attempt = self.throttle.retries_on_429 - retries;
                        let secs =
                            after.unwrap_or(self.throttle.wait_on_429_secs << attempt.min(6));
                        throttle::cooldown(&self.paths, secs);
                        if retries == 0 {
                            return Err(err);
                        }
                        retries -= 1;
                        sink(&Event::Status(format!(
                            "Rate limited by DeepSeek, retry in {secs} s"
                        )));
                        tokio::time::sleep(Duration::from_secs(secs)).await;
                    }
                    _ => return Err(err),
                },
                ok => return ok,
            }
        }
    }

    async fn send_once(
        &mut self,
        prompt: &str,
        sink: &mut dyn FnMut(&Event),
    ) -> Result<reqwest::Response> {
        let session_id = match &self.session_id {
            Some(id) => id.clone(),
            None => {
                sink(&Event::Status("Opening conversation".into()));
                let id = self.client.create_session().await?;
                self.session_id = Some(id.clone());
                id
            }
        };
        sink(&Event::Status("Solving proof of work".into()));
        let pow = self.pow_header().await?;
        sink(&Event::Status("Waiting for DeepSeek".into()));
        self.client
            .completion(
                &session_id,
                self.parent_message_id,
                prompt,
                &self.opts,
                &pow,
            )
            .await
    }
}

impl Backend for Conversation {
    async fn ask(&mut self, prompt: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
        let mut body = self.send(prompt, sink).await?.bytes_stream();
        let mut parser = Parser::new();
        let mut answer = String::new();
        while let Some(chunk) = body.next().await {
            for event in parser.feed(&chunk.context("the answer stream broke")?) {
                if let Event::Text(text) = &event {
                    answer.push_str(text);
                }
                sink(&event);
            }
        }
        // The id is kept even for a cut answer, so the next prompt follows it.
        if let Some(id) = parser.response_message_id {
            self.parent_message_id = Some(id);
        }
        parser.finish()?;
        Ok(answer)
    }
}
