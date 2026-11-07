// SPDX-License-Identifier: GPL-3.0-or-later
//! User-owned connection settings. Credentials never enter prompts or UI state.
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{io::Read, path::PathBuf, time::Duration};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    ai: Ai,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Ai {
    base_url: String,
    model: String,
    api_key_env: String,
    api_key: Option<String>,
    headers: Map<String, Value>,
    extra_body: Map<String, Value>,
    timeout_seconds: u64,
}

impl Default for Ai {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            model: String::new(),
            api_key_env: "OPENAI_API_KEY".into(),
            api_key: None,
            headers: Map::new(),
            extra_body: Map::new(),
            timeout_seconds: 180,
        }
    }
}

pub struct Connection {
    pub endpoint: reqwest::Url,
    pub model: String,
    pub headers: HeaderMap,
    pub extra_body: Map<String, Value>,
    pub timeout: Duration,
    // Redact exact configured secrets even if a server echoes them in its response.
    secrets: Vec<String>,
}

impl Connection {
    pub fn redact(&self, text: &str) -> String {
        self.secrets.iter().fold(text.to_owned(), |s, secret| s.replace(secret, "[redacted]"))
    }
}

fn config_path(xdg: Option<String>, home: Option<String>) -> Result<PathBuf, String> {
    let root = match xdg.filter(|s| !s.is_empty()) {
        Some(path) if PathBuf::from(&path).is_absolute() => PathBuf::from(path),
        _ => PathBuf::from(home.filter(|s| !s.is_empty()).ok_or("Cannot locate configuration: HOME is unset.")?).join(".config"),
    };
    Ok(root.join("margin/config.json"))
}

pub fn load() -> Result<Connection, String> {
    let path = config_path(std::env::var("XDG_CONFIG_HOME").ok(), std::env::var("HOME").ok())?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path).and_then(|file| file.take(1024 * 1024 + 1).read_to_end(&mut bytes))
        .map_err(|_| format!("Configure AI in {} and restart margin. See README for an example.", path.display()))?;
    if bytes.len() > 1024 * 1024 {
        return Err("AI configuration exceeds 1 MiB.".into());
    }
    parse(&bytes, |key| std::env::var(key).ok())
}

fn parse(bytes: &[u8], env: impl Fn(&str) -> Option<String>) -> Result<Connection, String> {
    // serde's detailed error can contain a literal credential, so don't surface it.
    let Config { ai } = serde_json::from_slice::<Config>(bytes)
        .map_err(|_| "Invalid AI configuration. Check the JSON fields against the README example.".to_owned())?;
    if ai.model.trim().is_empty() {
        return Err("AI configuration requires a nonempty model name.".into());
    }
    if !(1..=3600).contains(&ai.timeout_seconds) {
        return Err("timeout_seconds must be between 1 and 3600.".into());
    }
    let endpoint = reqwest::Url::parse(&format!("{}/chat/completions", ai.base_url.trim_end_matches('/')))
        .map_err(|_| "Invalid AI base_url.".to_owned())?;
    if !matches!(endpoint.scheme(), "http" | "https") || !endpoint.username().is_empty()
        || endpoint.password().is_some() || endpoint.query().is_some() || endpoint.fragment().is_some()
    {
        return Err("base_url must be an HTTP(S) URL without credentials, query, or fragment.".into());
    }
    for key in ["model", "messages", "tools", "tool_choice", "stream", "n"] {
        if ai.extra_body.contains_key(key) {
            return Err(format!("extra_body cannot override {key}."));
        }
    }
    let token = env(&ai.api_key_env).filter(|s| !s.trim().is_empty())
        .or(ai.api_key.clone().filter(|s| !s.trim().is_empty()))
        .ok_or("AI token is missing. Set api_key_env in your environment or api_key in config.json, then restart.")?;
    let mut secrets = vec![token.clone()];
    if let Some(key) = ai.api_key.filter(|s| !s.is_empty()) { secrets.push(key); }
    let mut headers = HeaderMap::new();
    for (name, value) in ai.headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| "Invalid AI header name.")?;
        if name == AUTHORIZATION || matches!(name.as_str(), "host" | "content-length" | "content-type") {
            return Err("headers cannot override Authorization, Host, Content-Length, or Content-Type.".into());
        }
        let value = value.as_str().ok_or("AI header values must be strings.")?;
        if !value.is_empty() { secrets.push(value.to_owned()); }
        let mut value = HeaderValue::from_str(value).map_err(|_| "Invalid AI header value.")?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    let mut auth = HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| "Invalid AI token.")?;
    auth.set_sensitive(true);
    headers.insert(AUTHORIZATION, auth);
    Ok(Connection { endpoint, model: ai.model, headers, extra_body: ai.extra_body,
        timeout: Duration::from_secs(ai.timeout_seconds), secrets })
}

#[cfg(test)]
pub fn test_connection(endpoint: &str, extra: Value) -> Connection {
    let mut value = serde_json::json!({"ai":{"base_url":endpoint,"model":"test-model","api_key":"TEST_SECRET","timeout_seconds":1}});
    value["ai"].as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    parse(&serde_json::to_vec(&value).unwrap(), |_| None).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_follow_xdg_and_home() {
        assert_eq!(config_path(Some("/config".into()), None).unwrap(), PathBuf::from("/config/margin/config.json"));
        assert_eq!(config_path(Some("relative".into()), Some("/user".into())).unwrap(), PathBuf::from("/user/.config/margin/config.json"));
        assert!(config_path(None, None).is_err());
    }

    #[test]
    fn defaults_and_env_override_inline_token() {
        let c = parse(br#"{"ai":{"model":"test","api_key":"inline"}}"#, |_| Some("environment".into())).unwrap();
        assert_eq!(c.endpoint.as_str(), "https://api.openai.com/v1/chat/completions");
        assert_eq!(c.timeout.as_secs(), 180);
        assert_eq!(c.headers[AUTHORIZATION], "Bearer environment");
        assert_eq!(c.redact("environment inline"), "[redacted] [redacted]");
        let c = parse(br#"{"ai":{"model":"test","api_key":"inline"}}"#, |_| None).unwrap();
        assert_eq!(c.headers[AUTHORIZATION], "Bearer inline");
    }

    #[test]
    fn invalid_settings_are_safe_errors() {
        for bytes in [
            r#"{"ai":{"model":"test","api_key":123456}}"#,
            r#"{"ai":{"api_key":"SECRET"}}"#,
            r#"{"ai":{"model":"test","api_key":"SECRET","headers":{"authorization":"SECRET"}}}"#,
            r#"{"ai":{"model":"test","api_key":"SECRET","extra_body":{"messages":[]}}}"#,
            r#"{"ai":{"model":"test","api_key":"SECRET","timeout_seconds":0}}"#,
            r#"{"ai":{"model":"test","api_key":"SECRET","base_url":"https://SECRET@example.com"}}"#,
        ] {
            let error = parse(bytes.as_bytes(), |_| None).err().unwrap();
            assert!(!error.contains("SECRET"));
            assert!(!error.contains("123456"));
        }
        assert!(parse(br#"{"ai":{"model":"test"}}"#, |_| None).is_err());
    }
}
