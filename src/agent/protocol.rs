// SPDX-License-Identifier: GPL-3.0-or-later
//! Wire formats only. History retains native output items, including opaque reasoning.
use crate::{
    code_tools,
    config::{Api, Connection},
};
use serde_json::{Value, json};

pub(super) struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

pub(super) struct Output {
    pub items: Vec<Value>,
    pub calls: Vec<ToolCall>,
    pub text: String,
    pub usage: Value,
}

pub(super) fn request(connection: &Connection, history: &[Value]) -> Value {
    let mut body = match connection.api {
        Api::ChatCompletions => json!({"messages":history,"tools":code_tools::definitions(),
            "tool_choice":"auto","stream":false,"n":1}),
        Api::Responses => {
            let tools: Vec<Value> = code_tools::definitions()
                .as_array()
                .unwrap()
                .iter()
                .map(|tool| {
                    let mut function = tool["function"].clone();
                    function["type"] = json!("function");
                    // Preserve optional arguments in the existing tool schemas.
                    function["strict"] = json!(false);
                    function
                })
                .collect();
            // Encrypted reasoning enables stateless continuation on OpenAI. Compatible
            // providers can override these optional defaults through extra_body.
            json!({"input":history,"tools":tools,"stream":false,"store":false,
                "include":["reasoning.encrypted_content"]})
        }
    };
    body.as_object_mut()
        .unwrap()
        .extend(connection.extra_body.clone());
    if connection.api == Api::Responses {
        // A null override omits an optional default for providers that reject it.
        for key in ["store", "include"] {
            if body[key].is_null() {
                body.as_object_mut().unwrap().remove(key);
            }
        }
    }
    body["model"] = json!(connection.model);
    body
}

pub(super) fn parse(api: Api, value: Value) -> Result<Output, String> {
    let (items, raw_calls, text) = match api {
        Api::ChatCompletions => {
            let message = value
                .get("choices")
                .and_then(|v| v.get(0))
                .and_then(|v| v.get("message"))
                .filter(|v| v.is_object())
                .ok_or("AI response is missing an assistant message.")?;
            if message["role"] != "assistant" {
                return Err("AI response has an invalid message role.".into());
            }
            if value["choices"][0]["finish_reason"] == "length" {
                return Err("AI response hit its output limit. Increase the provider's output token parameter and restart.".into());
            }
            let calls = match message.get("tool_calls") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(calls)) => calls.clone(),
                _ => return Err("AI response contains invalid tool calls.".into()),
            };
            (
                vec![message.clone()],
                calls,
                message["content"].as_str().unwrap_or("").to_owned(),
            )
        }
        Api::Responses => {
            if !value["error"].is_null() {
                return Err(format!(
                    "AI response failed: {}",
                    value["error"]["message"]
                        .as_str()
                        .unwrap_or("provider error")
                ));
            }
            if value["status"] != "completed" {
                return Err(format!(
                    "AI response status {}: {}",
                    value["status"].as_str().unwrap_or("missing or invalid"),
                    value["incomplete_details"]["reason"]
                        .as_str()
                        .unwrap_or("no completed answer")
                ));
            }
            let items = value["output"]
                .as_array()
                .ok_or("AI response is missing its output items.")?
                .clone();
            let mut calls = Vec::new();
            let mut parts = Vec::new();
            for item in &items {
                if item
                    .get("status")
                    .is_some_and(|s| !s.is_null() && s != "completed")
                {
                    return Err("AI response contains an unfinished output item.".into());
                }
                match item["type"].as_str() {
                    Some("message") => {
                        if item["role"] != "assistant" {
                            return Err("AI response has an invalid message role.".into());
                        }
                        for part in item["content"]
                            .as_array()
                            .ok_or("AI response contains invalid message content.")?
                        {
                            match part["type"].as_str() {
                                Some("output_text") => parts.push(
                                    part["text"]
                                        .as_str()
                                        .ok_or("AI response contains invalid output text.")?
                                        .to_owned(),
                                ),
                                Some("refusal") => parts.push(
                                    part["refusal"]
                                        .as_str()
                                        .ok_or("AI response contains an invalid refusal.")?
                                        .to_owned(),
                                ),
                                _ => {
                                    return Err(
                                        "AI response contains unsupported message content.".into(),
                                    );
                                }
                            }
                        }
                    }
                    Some("function_call") => calls
                        .push(json!({"type":"function","id":item["call_id"],
                        "function":{"name":item["name"],"arguments":item["arguments"]}})),
                    Some("reasoning") => (), // Retain verbatim, never render.
                    _ => return Err("AI response contains an unsupported output item.".into()),
                }
            }
            (items, calls, parts.join("\n\n"))
        }
    };
    let mut ids = std::collections::HashSet::new();
    let mut calls = Vec::new();
    for call in raw_calls {
        if call["type"] != "function"
            || !call["id"]
                .as_str()
                .is_some_and(|s| !s.is_empty() && ids.insert(s.to_owned()))
            || !call["function"]["name"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
            || !call["function"]["arguments"].is_string()
        {
            return Err("AI response contains a malformed tool call.".into());
        }
        calls.push(ToolCall {
            id: call["id"].as_str().unwrap().into(),
            name: call["function"]["name"].as_str().unwrap().into(),
            arguments: call["function"]["arguments"].as_str().unwrap().into(),
        });
    }
    Ok(Output {
        items,
        calls,
        text,
        usage: value["usage"].clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_connection;

    #[test]
    fn optional_defaults_can_be_omitted_for_compatible_providers() {
        let config = test_connection(
            "https://example.com",
            json!({"api":"responses","extra_body":{"store":null,"include":null}}),
        );
        let body = request(&config, &[]);
        assert!(body.get("store").is_none());
        assert!(body.get("include").is_none());
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn malformed_calls_and_duplicate_call_ids_are_rejected() {
        let call = json!({"type":"function_call","id":"item","call_id":"call","name":"read_file","arguments":"{}"});
        let mut invalid = call.clone();
        invalid["arguments"] = json!({});
        for output in [json!([invalid]), json!([call.clone(), call])] {
            let error = parse(
                Api::Responses,
                json!({"status":"completed","output":output}),
            )
            .err()
            .unwrap();
            assert!(error.contains("malformed tool call"));
        }
    }

    #[test]
    fn refusals_render_but_reasoning_is_opaque() {
        let reasoning = json!({"type":"reasoning","status":null,"summary":[{"text":"private"}],"encrypted_content":"opaque"});
        let output = parse(Api::Responses, json!({"status":"completed","output":[reasoning.clone(),
            {"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot answer."}]}]})).unwrap();
        assert_eq!(output.text, "Cannot answer.");
        assert_eq!(output.items[0], reasoning);
    }
}

pub(super) fn tool_result(api: Api, id: &str, result: Value) -> Value {
    match api {
        Api::ChatCompletions => {
            json!({"role":"tool","tool_call_id":id,"content":result.to_string()})
        }
        Api::Responses => {
            json!({"type":"function_call_output","call_id":id,"output":result.to_string()})
        }
    }
}
