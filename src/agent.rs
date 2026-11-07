// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenAI-compatible tool loop, isolated from presentation and credentials UI.
use crate::{code_tools::CodeTools, config::Connection, usage::TurnStats};
use serde_json::Value;
use std::{path::PathBuf, sync::Arc, time::Instant};

mod protocol;

pub const HISTORY_LIMIT: usize = 512 * 1024;
const RESPONSE_LIMIT: usize = 2 * 1024 * 1024;

pub struct Answer {
    pub history: Vec<Value>,
    pub text: String,
    pub stats: TurnStats,
}

type Completion = Box<dyn FnOnce(Result<Answer, String>) + Send>;
type Progress = Box<dyn Fn(String) + Send + Sync>;
struct Job { history: Vec<Value>, progress: Progress, complete: Completion }

#[derive(Clone)]
pub struct Agent {
    sender: tokio::sync::mpsc::UnboundedSender<Job>,
}

impl Agent {
    pub fn start(connection: Connection, root: PathBuf) -> Result<Self, String> {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()
            .map_err(|_| "Could not start the AI worker runtime.")?;
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
            .timeout(connection.timeout).build().map_err(|_| "Could not initialize the AI HTTP client.")?;
        let connection = Arc::new(connection);
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Job>();
        std::thread::Builder::new().name("margin-agent".into()).spawn(move || {
            runtime.block_on(async move {
                while let Some(job) = receiver.recv().await {
                    let (connection, client, root) = (connection.clone(), client.clone(), root.clone());
                    tokio::spawn(async move {
                        let result = run(&client, &connection, root, job.history, &*job.progress).await;
                        (job.complete)(result);
                    });
                }
            });
        }).map_err(|_| "Could not start the AI worker thread.")?;
        Ok(Self { sender })
    }

    pub fn send(&self, history: Vec<Value>, progress: impl Fn(String) + Send + Sync + 'static,
        complete: impl FnOnce(Result<Answer, String>) + Send + 'static) -> bool {
        self.sender.send(Job { history, progress: Box::new(progress), complete: Box::new(complete) }).is_ok()
    }
}

fn within_budget(history: &[Value]) -> Result<(), String> {
    if serde_json::to_vec(history).map_err(|_| "Could not encode conversation.")?.len() > HISTORY_LIMIT {
        Err("Conversation reached its 512 KiB history limit. Start a thread on another line.".into())
    } else { Ok(()) }
}

async fn response(client: &reqwest::Client, connection: &Connection, history: &[Value]) -> Result<protocol::Output, String> {
    let body = protocol::request(connection, history);
    let mut response = client.post(connection.endpoint.clone()).headers(connection.headers.clone()).json(&body).send().await
        .map_err(|e| if e.is_timeout() { "AI request timed out." } else { "Could not connect to the AI endpoint." }.to_owned())?;
    if !response.status().is_success() {
        return Err(format!("AI endpoint returned HTTP {}. Check endpoint, credentials, model, and parameters.", response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| if e.is_timeout() { "AI response timed out." } else { "AI response was interrupted." })? {
        if bytes.len() + chunk.len() > RESPONSE_LIMIT { return Err("AI response exceeded 2 MiB.".into()); }
        bytes.extend_from_slice(&chunk);
    }
    let mut value: Value = serde_json::from_slice(&bytes).map_err(|_| "AI endpoint returned invalid JSON.")?;
    redact_value(&mut value, connection);
    protocol::parse(connection.api, value)
}

fn redact_value(value: &mut Value, connection: &Connection) {
    match value {
        Value::String(s) => *s = connection.redact(s),
        Value::Array(values) => values.iter_mut().for_each(|v| redact_value(v, connection)),
        Value::Object(values) => values.values_mut().for_each(|v| redact_value(v, connection)),
        _ => (),
    }
}

async fn run(client: &reqwest::Client, connection: &Connection, root: PathBuf, mut history: Vec<Value>,
    progress: &(dyn Fn(String) + Send + Sync)) -> Result<Answer, String> {
    // Code or a pasted question may itself contain the configured credential.
    // Scrub message content before any request, including the initial patch context.
    for message in &mut history { redact_value(message, connection); }
    let mut tools: Option<Arc<CodeTools>> = None;
    let started = Instant::now();
    let mut stats = TurnStats::default();
    let mut calls = 0;
    for _ in 0..16 {
        within_budget(&history)?;
        progress("Waiting for AI response…".into());
        let output = response(client, connection, &history).await?;
        stats.record(&output.usage);
        history.extend(output.items);
        within_budget(&history)?;
        if output.calls.is_empty() {
            if output.text.trim().is_empty() { return Err("AI endpoint returned no answer text.".into()); }
            stats.elapsed = started.elapsed();
            stats.tool_calls = calls;
            return Ok(Answer { history, text: output.text, stats });
        }
        calls += output.calls.len();
        if calls > 64 { return Err("AI turn exceeded its 64 tool-call limit.".into()); }
        if tools.is_none() {
            progress("Preparing code tools…".into());
            let root = root.clone();
            tools = Some(Arc::new(tokio::task::spawn_blocking(move || CodeTools::new(&root)).await
                .map_err(|_| "Code inventory worker failed.")??));
        }
        for call in output.calls {
            let tools = tools.as_ref().unwrap().clone();
            let name = call.name;
            let args = call.arguments;
            progress(tool_activity(&name, &args, connection));
            let mut result = tokio::task::spawn_blocking(move || tools.call(&name, &args)).await
                .map_err(|_| "Code inspection worker failed.")?;
            redact_value(&mut result, connection);
            history.push(protocol::tool_result(connection.api, &call.id, result));
            within_budget(&history)?;
        }
    }
    Err("AI turn exceeded its 16 request limit.".into())
}

// Tool arguments are untrusted. Keep activity text short, single-line, and redacted.
fn tool_activity(name: &str, args: &str, connection: &Connection) -> String {
    let args: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let detail = |key: &str| {
        let redacted = connection.redact(args[key].as_str().unwrap_or(""));
        let single_line: String = redacted.chars().filter(|c| !c.is_control()).collect();
        let redacted = connection.redact(&single_line);
        let mut chars = redacted.chars();
        let mut text: String = chars.by_ref().take(120).collect();
        if chars.next().is_some() { text.push('…'); }
        text
    };
    let path = detail("path");
    match name {
        "list_files" if path.is_empty() => "Listing files…".into(),
        "list_files" => format!("Listing files in {path}…"),
        "read_file" if path.is_empty() => "Reading file…".into(),
        "read_file" => format!("Reading {path}…"),
        "search_code" => {
            let query = detail("query");
            let scope = if path.is_empty() { String::new() } else { format!(" in {path}") };
            if query.is_empty() { format!("Searching code{scope}…") }
            else { format!("Searching for “{query}”{scope}…") }
        }
        _ => "Processing tool call…".into(),
    }
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
