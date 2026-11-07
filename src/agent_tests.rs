// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use serde_json::json;
use crate::{code_tools, config::test_connection};
use std::{io::{Read, Write}, net::TcpListener, sync::Mutex, thread, time::Duration};

struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    join: thread::JoinHandle<()>,
}

fn mock(replies: Vec<(u16, String, Duration)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    let join = thread::spawn(move || {
        for (status, body, delay) in replies {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut bytes = Vec::new();
            let header_end = loop {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") { break pos + 4; }
            };
            let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
            let length = headers.lines().find_map(|line| {
                let (key, val) = line.split_once(':')?;
                key.eq_ignore_ascii_case("content-length").then(|| val.trim().parse::<usize>().unwrap())
            }).unwrap();
            while bytes.len() < header_end + length {
                let mut buf = [0; 4096];
                let n = socket.read(&mut buf).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buf[..n]);
            }
            let request = serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap();
            capture.lock().unwrap().push((headers, request));
            thread::sleep(delay);
            let response = format!("HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(response.as_bytes());
        }
    });
    Mock { url, requests, join }
}

fn reply(message: Value) -> (u16, String, Duration) {
    (200, json!({"choices":[{"message":message,"finish_reason":"stop"}]}).to_string(), Duration::ZERO)
}

fn question() -> Vec<Value> { vec![json!({"role":"user","content":"Explain this code"})] }

#[tokio::test]
async fn tool_loop_preserves_history_and_extra_fields() {
    let server = mock(vec![
        reply(json!({"role":"assistant","content":null,"reasoning_content":"inspection",
            "tool_calls":[{"id":"call-1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"sample.rs\"}"}}]})),
        reply(json!({"role":"assistant","content":"It returns 42. TEST_SECRET"})),
        reply(json!({"role":"assistant","content":"A follow-up answer."})),
    ]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.rs"), "fn answer() -> u32 { 42 }\n").unwrap();
    let config = test_connection(&server.url, json!({"headers":{"X-Test":"custom"},"extra_body":{"temperature":0.3,"reasoning_effort":"low"}}));
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(config.timeout).build().unwrap();
    let mut history = question();
    history[0]["content"] = json!("Explain this code containing TEST_SECRET");
    let answer = run(&client, &config, dir.path().to_owned(), history, &|_| {}).await.unwrap();
    assert_eq!(answer.text, "It returns 42. [redacted]");
    assert!(answer.stats.label().starts_with("Token usage not reported"));
    assert_eq!(answer.history.len(), 4);
    let mut followup = answer.history;
    followup.push(json!({"role":"user","content":"Why?"}));
    run(&client, &config, dir.path().to_owned(), followup, &|_| {}).await.unwrap();
    server.join.join().unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].0.starts_with("POST /v1/chat/completions "));
    assert!(requests[0].0.to_lowercase().contains("authorization: bearer test_secret"));
    assert!(requests[0].0.to_lowercase().contains("x-test: custom"));
    assert_eq!(requests[0].1["temperature"], 0.3);
    assert_eq!(requests[0].1["model"], "test-model");
    assert_eq!(requests[0].1["stream"], false);
    assert_eq!(requests[0].1["tools"].as_array().unwrap().len(), 3);
    assert!(!requests[0].1["messages"].to_string().contains("TEST_SECRET"));
    assert_eq!(requests[1].1["messages"][1]["reasoning_content"], "inspection");
    assert_eq!(requests[1].1["messages"][2]["tool_call_id"], "call-1");
    assert!(requests[1].1["messages"][2]["content"].as_str().unwrap().contains("42"));
    assert_eq!(requests[2].1["messages"][4]["content"], "Why?");
}

#[tokio::test]
async fn provider_failures_are_bounded_and_do_not_expose_secrets() {
    for (reply, expected) in [
        ((401, "TEST_SECRET".into(), Duration::ZERO), "HTTP 401"),
        ((302, "TEST_SECRET".into(), Duration::ZERO), "HTTP 302"),
        ((200, "not json TEST_SECRET".into(), Duration::ZERO), "invalid JSON"),
        ((200, "{}".into(), Duration::ZERO), "missing an assistant"),
        ((200, "x".repeat(RESPONSE_LIMIT + 1), Duration::ZERO), "exceeded 2 MiB"),
        (reply(json!({"role":"assistant","content":""})), "no answer text"),
        (reply(json!({"role":"assistant","tool_calls":[{"type":"function","function":{"name":"read_file"}}]})), "malformed tool call"),
        ((200, "{}".into(), Duration::from_millis(1300)), "timed out"),
    ] {
        let server = mock(vec![reply]);
        let config = test_connection(&server.url, json!({}));
        let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(config.timeout).build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let error = run(&client, &config, dir.path().to_owned(), question(), &|_| {}).await.err().unwrap();
        assert!(error.contains(expected), "expected {expected}, got {error}");
        assert!(!error.contains("TEST_SECRET"));
        server.join.join().unwrap();
    }
}

#[tokio::test]
async fn unknown_tools_return_errors_and_request_limit_stops_loops() {
    let turn = || reply(json!({"role":"assistant","tool_calls":[{"id":"loop","type":"function","function":{"name":"shell","arguments":"{}"}}]}));
    let server = mock((0..16).map(|_| turn()).collect());
    let config = test_connection(&server.url, json!({}));
    let dir = tempfile::tempdir().unwrap();
    let error = run(&reqwest::Client::new(), &config, dir.path().to_owned(), question(), &|_| {}).await.err().unwrap();
    assert!(error.contains("16 request limit"));
    server.join.join().unwrap();
    let requests = server.requests.lock().unwrap();
    assert!(requests[1].1["messages"][2]["content"].as_str().unwrap().contains("Unknown tool"));
    let history = vec![json!({"role":"user","content":"x".repeat(HISTORY_LIMIT)})];
    assert!(within_budget(&history).is_err());
}

#[tokio::test]
async fn progress_tracks_tools_and_returns_to_waiting() {
    let calls: Vec<Value> = [
        ("list_files", json!({"path":"src"})),
        ("search_code", json!({"query":"answer", "path":"src"})),
        ("read_file", json!({"path":"src/sample.rs"})),
    ].into_iter().enumerate().map(|(i, (name, args))|
        json!({"id":format!("call-{i}"),"type":"function","function":{"name":name,"arguments":args.to_string()}})
    ).collect();
    let with_usage = |message: Value, usage: Value| {
        let mut response: Value = serde_json::from_str(&reply(message).1).unwrap();
        response["usage"] = usage;
        (200, response.to_string(), Duration::ZERO)
    };
    let server = mock(vec![
        with_usage(json!({"role":"assistant","tool_calls":calls}), json!({"prompt_tokens":100,"completion_tokens":20,"total_tokens":120})),
        with_usage(json!({"role":"assistant","content":"Done"}), json!({"prompt_tokens":150,"completion_tokens":30,"total_tokens":180})),
        with_usage(json!({"role":"assistant","content":"Follow-up"}), json!({"prompt_tokens":200,"completion_tokens":10,"total_tokens":210})),
    ]);
    let config = test_connection(&server.url, json!({}));
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/sample.rs"), "fn answer() {}\n").unwrap();
    let activities = Mutex::new(Vec::new());
    let progress = |activity| activities.lock().unwrap().push(activity);
    let answer = run(&reqwest::Client::new(), &config, dir.path().to_owned(), question(), &progress).await.unwrap();
    assert_eq!(answer.text, "Done");
    let stats = answer.stats.label();
    assert!(stats.starts_with("300 tokens · 250 input · 50 output"));
    assert!(stats.ends_with("2 API requests · 3 tool calls"));
    assert!(!stats.contains("partial"));
    assert_eq!(*activities.lock().unwrap(), [
        "Waiting for AI response…", "Preparing code tools…", "Listing files in src…",
        "Searching for “answer” in src…", "Reading src/sample.rs…", "Waiting for AI response…",
    ]);
    let mut history = answer.history;
    history.push(json!({"role":"user","content":"Why?"}));
    let followup = run(&reqwest::Client::new(), &config, dir.path().to_owned(), history, &|_| {}).await.unwrap();
    let stats = followup.stats.label();
    assert!(stats.starts_with("210 tokens · 200 input · 10 output"));
    assert!(stats.ends_with("1 API request · 0 tool calls"));
    server.join.join().unwrap();
}

#[test]
fn activity_details_are_bounded_and_redacted() {
    let config = test_connection("http://localhost/v1", json!({}));
    let activity = tool_activity("read_file", &json!({"path":"src/TEST_SECRET\nfile.rs"}).to_string(), &config);
    assert_eq!(activity, "Reading src/[redacted]file.rs…");
    let activity = tool_activity("search_code", &json!({"query":"é".repeat(1000), "path":"x".repeat(1000)}).to_string(), &config);
    assert!(activity.chars().count() < 300);
    assert_eq!(tool_activity("read_file", "invalid JSON", &config), "Reading file…");
    assert_eq!(tool_activity("list_files", "{}", &config), "Listing files…");
    assert_eq!(tool_activity("shell", "{}", &config), "Processing tool call…");
}

fn responses_reply(value: Value) -> (u16, String, Duration) {
    (200, value.to_string(), Duration::ZERO)
}

#[tokio::test]
async fn responses_tools_and_stateless_followups_preserve_ordered_items() {
    let tool_output: Value = serde_json::from_str(include_str!("fixtures/responses-tools.json")).unwrap();
    let text_output: Value = serde_json::from_str(include_str!("fixtures/responses-text.json")).unwrap();
    let server = mock(vec![responses_reply(tool_output.clone()), responses_reply(text_output.clone()), responses_reply(text_output)]);
    let config = test_connection(&server.url, json!({"api":"responses", "headers":{"X-Custom":"value"},
        "extra_body":{"max_output_tokens":4096,"reasoning":{"effort":"low"}}}));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("sample.rs"), "fn answer() -> u32 { 42 }\n").unwrap();
    let mut history = vec![json!({"role":"system","content":"Help review code."})];
    history.extend(question());
    let answer = run(&reqwest::Client::new(), &config, dir.path().to_owned(), history.clone(), &|_| {}).await.unwrap();
    assert_eq!(answer.text, "It returns 42.\n\n[redacted]\n\nDone.");
    assert!(!answer.text.contains("PRIVATE"));
    assert!(answer.stats.label().starts_with("300 tokens · 250 input · 50 output"));
    assert_eq!(answer.stats.tool_calls, 2);
    let saved = answer.history.clone();
    let mut followup = saved.clone();
    followup.push(json!({"role":"user","content":"Why?"}));
    run(&reqwest::Client::new(), &config, dir.path().to_owned(), followup.clone(), &|_| {}).await.unwrap();
    server.join.join().unwrap();
    let requests = server.requests.lock().unwrap();
    let first = &requests[0].1;
    assert!(requests[0].0.starts_with("POST /v1/responses "));
    assert!(requests[0].0.to_lowercase().contains("x-custom: value"));
    assert!(requests[0].0.to_lowercase().contains("authorization: bearer test_secret"));
    assert_eq!(first["input"], json!(history));
    assert_eq!(first["model"], "test-model");
    assert_eq!(first["max_output_tokens"], 4096);
    assert_eq!(first["reasoning"]["effort"], "low");
    assert_eq!(first["store"], false);
    assert_eq!(first["include"], json!(["reasoning.encrypted_content"]));
    for key in ["messages", "n", "previous_response_id", "max_completion_tokens", "reasoning_effort"] {
        assert!(first.get(key).is_none());
    }
    let tools = first["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 3);
    for (tool, original) in tools.iter().zip(code_tools::definitions().as_array().unwrap()) {
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["parameters"], original["function"]["parameters"]);
        assert_eq!(tool["strict"], false);
        assert!(tool.get("function").is_none());
    }
    let continuation = requests[1].1["input"].as_array().unwrap();
    assert_eq!(&continuation[2..6], tool_output["output"].as_array().unwrap());
    for (index, call_id) in [(6, "call_1"), (7, "call_2")] {
        assert_eq!(continuation[index]["type"], "function_call_output");
        assert_eq!(continuation[index]["call_id"], call_id);
        assert!(continuation[index]["output"].as_str().unwrap().contains("42"));
    }
    assert_eq!(requests[2].1["input"], json!(followup));
    assert_eq!(&followup[..saved.len()], saved.as_slice());
}

#[tokio::test]
async fn responses_tool_errors_are_returned_to_model() {
    let calls: Vec<Value> = [("read_file", "not JSON"), ("shell", "{}"), ("read_file", "{\"path\":\"missing\"}")]
        .into_iter().enumerate().map(|(i, (name, args))| json!({"type":"function_call","id":format!("item_{i}"),
            "call_id":format!("call_{i}"),"name":name,"arguments":args})).collect();
    let server = mock(vec![
        responses_reply(json!({"status":"completed","output":calls})),
        responses_reply(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Done"}]}]})),
    ]);
    let config = test_connection(&server.url, json!({"api":"responses","extra_body":{"include":[],"store":true}}));
    let dir = tempfile::tempdir().unwrap();
    run(&reqwest::Client::new(), &config, dir.path().to_owned(), question(), &|_| {}).await.unwrap();
    server.join.join().unwrap();
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests[0].1["include"], json!([]));
    assert_eq!(requests[0].1["store"], true);
    for item in &requests[1].1["input"].as_array().unwrap()[4..] {
        let result: Value = serde_json::from_str(item["output"].as_str().unwrap()).unwrap();
        assert!(result["error"].is_string());
    }
}

#[tokio::test]
async fn responses_failures_have_safe_diagnostics() {
    for (status, body, expected) in [
        (401, json!({"error":{"message":"TEST_SECRET"}}), "HTTP 401"),
        (200, json!({"status":"failed","error":{"message":"provider TEST_SECRET"}}), "provider [redacted]"),
        (200, json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[]}), "incomplete: max_output_tokens"),
        (200, json!({"status":"in_progress","output":[]}), "in_progress"),
        (200, json!({"status":"cancelled","output":[]}), "cancelled"),
        (200, json!({"output":[]}), "missing or invalid"),
        (200, json!({"status":"completed"}), "missing its output"),
        (200, json!({"status":"completed","output":[]}), "no answer text"),
        (200, json!({"status":"completed","output":[{"type":"function_call","id":"item","name":"read_file","arguments":"{}"}]}), "malformed tool call"),
        (200, json!({"status":"completed","output":[{"type":"message","role":"assistant","status":"incomplete","content":[]}]}), "unfinished"),
    ] {
        let server = mock(vec![(status, body.to_string(), Duration::ZERO)]);
        let config = test_connection(&server.url, json!({"api":"responses"}));
        let dir = tempfile::tempdir().unwrap();
        let error = run(&reqwest::Client::new(), &config, dir.path().to_owned(), question(), &|_| {}).await.err().unwrap();
        assert!(error.contains(expected), "expected {expected}, got {error}");
        assert!(!error.contains("TEST_SECRET"));
        server.join.join().unwrap();
    }
}
