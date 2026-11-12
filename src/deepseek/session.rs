//! One conversation with DeepSeek: session id, message chain, proof of work per turn.

use super::client::{ApiError, COMPLETION_PATH, Client, NO_POW, TurnOpts, UPLOAD_PATH};
use super::stream::Parser;
use super::{pow, throttle};
use crate::attach::{self, Attachment};
use crate::backend::{Backend, Event};
use crate::config::{Paths, Throttle};
use anyhow::{Context, Result};
use futures_util::StreamExt;
use std::fs;
use std::time::{Duration, Instant};

/// Between two status questions while DeepSeek reads uploaded files, and how long to wait in all.
const POLL_EVERY: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(180);

pub struct Conversation {
    client: Client,
    paths: Paths,
    throttle: Throttle,
    opts: TurnOpts,
    wasm: Option<Vec<u8>>,
    session_id: Option<String>,
    /// Id of the last answer; the next prompt hangs below it
    parent_message_id: Option<u64>,
    /// Files for the next prompt (`Backend::attach`), uploaded when it is sent
    pending: Vec<Attachment>,
    /// The chat for web searches (`Backend::search`): its session and last answer.
    /// Separate from the conversation, without the agent rules, as the site asks.
    search_session: Option<String>,
    search_parent: Option<u64>,
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
            pending: Vec::new(),
            search_session: None,
            search_parent: None,
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    /// What is needed to continue this conversation later: the remote session id
    /// and the id of the last answer. `None` before the first request.
    pub fn state(&self) -> Option<(String, Option<u64>)> {
        self.session_id
            .clone()
            .map(|id| (id, self.parent_message_id))
    }

    /// Continues an earlier conversation: the next prompt goes into that remote
    /// session, below that answer.
    pub fn resume(&mut self, session_id: String, parent_message_id: Option<u64>) {
        self.session_id = Some(session_id);
        self.parent_message_id = parent_message_id;
    }

    /// Deep thinking and web search for the requests from now on.
    pub fn set_thinking(&mut self, thinking: bool, search: bool) {
        self.opts.thinking = thinking;
        self.opts.search = search;
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

    /// Fetches and solves a challenge for the call to `target_path`; returns the
    /// `X-DS-PoW-Response` value. Each answer is good for one call.
    pub async fn pow_header(&mut self, target_path: &str) -> Result<String> {
        let challenge = self.client.create_challenge(target_path).await?;
        if challenge.algorithm == NO_POW {
            // The local mock: nothing to solve, nothing to download.
            return Ok(pow::header(&challenge, 0, target_path));
        }
        let wasm = self.wasm().await?.to_vec();
        let solved = challenge.clone();
        let answer = tokio::task::spawn_blocking(move || pow::solve(&wasm, &solved))
            .await
            .context("proof-of-work task")??;
        Ok(pow::header(&challenge, answer, target_path))
    }

    /// Uploads the pending files and waits until DeepSeek has read them; returns
    /// their ids for `ref_file_ids`. One proof of work per file. Uploads are not
    /// prompts: they do not take a place in the throttle, but a 429 pauses
    /// everything like one on a prompt.
    async fn upload_pending(&mut self, sink: &mut dyn FnMut(&Event)) -> Result<Vec<String>> {
        let files = std::mem::take(&mut self.pending);
        let mut ids = Vec::new();
        for file in &files {
            let bytes = tokio::fs::read(&file.path)
                .await
                .with_context(|| format!("read {}", file.path.display()))?;
            sink(&Event::Status(format!("Uploading {}", file.label())));
            let pow = self.pow_header(UPLOAD_PATH).await?;
            let id = self
                .client
                .upload_file(&file.name, bytes, attach::mime(&file.name), &pow)
                .await
                .inspect_err(|err| self.pause_on_429(err))?;
            ids.push(id);
        }
        if ids.is_empty() {
            return Ok(ids);
        }
        let names = |pick: &dyn Fn(&str) -> bool| -> String {
            files
                .iter()
                .zip(&ids)
                .filter(|(_, id)| pick(id))
                .map(|(file, _)| file.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let started = Instant::now();
        loop {
            let remote = self
                .client
                .fetch_files(&ids)
                .await
                .inspect_err(|err| self.pause_on_429(err))?;
            let status = |id: &str| remote.iter().find(|file| file.id == id);
            if let Some(failed) = remote.iter().find(|file| file.failed()) {
                let name = names(&|id| id == failed.id);
                let why = failed.error.as_deref().unwrap_or(&failed.status);
                anyhow::bail!("DeepSeek could not read {name} ({why})");
            }
            if ids
                .iter()
                .all(|id| status(id).is_some_and(|file| file.ready()))
            {
                return Ok(ids);
            }
            if started.elapsed() > READ_TIMEOUT {
                let waiting = names(&|id| !status(id).is_some_and(|file| file.ready()));
                anyhow::bail!(
                    "DeepSeek did not finish reading {waiting} in {} s",
                    READ_TIMEOUT.as_secs()
                );
            }
            let waiting = names(&|id| !status(id).is_some_and(|file| file.ready()));
            sink(&Event::Status(format!("DeepSeek is reading {waiting}")));
            tokio::time::sleep(POLL_EVERY).await;
        }
    }

    /// A 429 on a helper call: the same pause as after one on a prompt.
    fn pause_on_429(&self, err: &anyhow::Error) {
        if let Some(ApiError::RateLimited(after)) = err.downcast_ref::<ApiError>() {
            throttle::cooldown(&self.paths, after.unwrap_or(self.throttle.wait_on_429_secs));
        }
    }

    /// Sends `prompt` as the next message; the response body is the SSE stream.
    /// Throttled; HTTP 429 is retried as the settings say. `sink` gets the progress.
    async fn send(
        &mut self,
        prompt: &str,
        files: &[String],
        search: bool,
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
            match self.send_once(prompt, files, search, sink).await {
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

    /// One request; `search` = in the search chat (search on, thinking off) instead of
    /// the conversation.
    async fn send_once(
        &mut self,
        prompt: &str,
        files: &[String],
        search: bool,
        sink: &mut dyn FnMut(&Event),
    ) -> Result<reqwest::Response> {
        let known = match search {
            true => self.search_session.clone(),
            false => self.session_id.clone(),
        };
        let session_id = match known {
            Some(id) => id,
            None => {
                sink(&Event::Status("Opening conversation".into()));
                let id = self.client.create_session().await?;
                match search {
                    true => self.search_session = Some(id.clone()),
                    false => self.session_id = Some(id.clone()),
                }
                id
            }
        };
        let (parent, opts) = match search {
            true => (
                self.search_parent,
                TurnOpts {
                    thinking: false,
                    search: true,
                    ..self.opts.clone()
                },
            ),
            false => (self.parent_message_id, self.opts.clone()),
        };
        sink(&Event::Status("Solving proof of work".into()));
        let pow = self.pow_header(COMPLETION_PATH).await?;
        sink(&Event::Status("Waiting for DeepSeek".into()));
        self.client
            .completion(&session_id, parent, prompt, files, &opts, &pow)
            .await
    }
}

impl Backend for Conversation {
    fn attach(&mut self, files: Vec<Attachment>) {
        self.pending = files;
    }

    async fn ask(&mut self, prompt: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
        let files = self.upload_pending(sink).await?;
        self.answer(prompt, &files, false, sink).await
    }

    async fn search(&mut self, query: &str, sink: &mut dyn FnMut(&Event)) -> Result<String> {
        self.answer(query, &[], true, sink).await
    }
}

impl Conversation {
    /// Sends a prompt and reads the answer stream: in the conversation, or with
    /// `search` in the search chat. The answer's id becomes that chat's next parent.
    async fn answer(
        &mut self,
        prompt: &str,
        files: &[String],
        search: bool,
        sink: &mut dyn FnMut(&Event),
    ) -> Result<String> {
        let mut body = self.send(prompt, files, search, sink).await?.bytes_stream();
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
            match search {
                true => self.search_parent = Some(id),
                false => self.parent_message_id = Some(id),
            }
        }
        parser.finish()?;
        // An answer proves the token: the next start skips its check.
        throttle::token_ok(&self.paths);
        Ok(answer)
    }
}
