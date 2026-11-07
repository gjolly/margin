// SPDX-License-Identifier: GPL-3.0-or-later
//! OpenAI-compatible tool loop, isolated from presentation and credentials UI.
use crate::{code_tools::{self, CodeTools}, config::Connection, usage::TurnStats};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Instant};

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

async fn response(client: &reqwest::Client, connection: &Connection, history: &[Value]) -> Result<(Value, Value), String> {
    let mut body = connection.extra_body.clone();
    body.extend(json!({"model":connection.model,"messages":history,"tools":code_tools::definitions(),
        "tool_choice":"auto","stream":false,"n":1}).as_object().unwrap().clone());
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
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| "AI endpoint returned invalid JSON.")?;
    let mut message = value.get("choices").and_then(|v| v.get(0)).and_then(|v| v.get("message")).cloned()
        .filter(Value::is_object).ok_or("AI response is missing an assistant message.")?;
    if message["role"] != "assistant" { return Err("AI response has an invalid message role.".into()); }
    redact_value(&mut message, connection);
    // An exhausted output budget is not a completed response; don't silently post it.
    if value["choices"][0]["finish_reason"] == "length" { return Err("AI response hit its output limit. Increase the provider's output token parameter and restart.".into()); }
    Ok((message, value["usage"].clone()))
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
        let (message, usage) = response(client, connection, &history).await?;
        stats.record(&usage);
        let tool_calls = match message.get("tool_calls") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(calls)) => calls.clone(),
            _ => return Err("AI response contains invalid tool calls.".into()),
        };
        if tool_calls.is_empty() {
            let text = message["content"].as_str().filter(|s| !s.trim().is_empty())
                .ok_or("AI endpoint returned no answer text.")?.to_owned();
            history.push(message);
            within_budget(&history)?;
            stats.elapsed = started.elapsed();
            stats.tool_calls = calls;
            return Ok(Answer { history, text, stats });
        }
        calls += tool_calls.len();
        if calls > 64 { return Err("AI turn exceeded its 64 tool-call limit.".into()); }
        let mut ids = std::collections::HashSet::new();
        for call in &tool_calls {
            if call["type"] != "function" || !call["id"].as_str().is_some_and(|s| !s.is_empty() && ids.insert(s))
                || !call["function"]["name"].is_string() || !call["function"]["arguments"].is_string()
            { return Err("AI response contains a malformed tool call.".into()); }
        }
        history.push(message);
        within_budget(&history)?;
        if tools.is_none() {
            progress("Preparing code tools…".into());
            let root = root.clone();
            tools = Some(Arc::new(tokio::task::spawn_blocking(move || CodeTools::new(&root)).await
                .map_err(|_| "Code inventory worker failed.")??));
        }
        for call in tool_calls {
            let tools = tools.as_ref().unwrap().clone();
            let name = call["function"]["name"].as_str().unwrap().to_owned();
            let args = call["function"]["arguments"].as_str().unwrap().to_owned();
            progress(tool_activity(&name, &args, connection));
            let mut result = tokio::task::spawn_blocking(move || tools.call(&name, &args)).await
                .map_err(|_| "Code inspection worker failed.")?;
            redact_value(&mut result, connection);
            history.push(json!({"role":"tool","tool_call_id":call["id"],"content":result.to_string()}));
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
