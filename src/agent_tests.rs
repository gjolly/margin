use super::*;
use crate::config::test_connection;
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
