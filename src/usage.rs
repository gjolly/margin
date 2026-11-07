// SPDX-License-Identifier: GPL-3.0-or-later
//! Provider-reported usage for one successful turn, including tool round trips.
use serde_json::Value;
use std::time::Duration;

#[derive(Default)]
pub struct TurnStats {
    pub requests: usize,
    pub tool_calls: usize,
    pub elapsed: Duration,
    input: Option<u128>,
    output: Option<u128>,
    total: Option<u128>,
    partial: bool,
}

impl TurnStats {
    pub fn record(&mut self, usage: &Value) {
        self.requests += 1;
        let input = usage["prompt_tokens"].as_u64().map(u128::from);
        let output = usage["completion_tokens"].as_u64().map(u128::from);
        let total = usage["total_tokens"].as_u64().map(u128::from)
            .or_else(|| Some(input? + output?));
        self.partial |= input.is_none() || output.is_none() || total.is_none();
        for (sum, value) in [(&mut self.input, input), (&mut self.output, output), (&mut self.total, total)] {
            if let Some(value) = value { *sum = Some(sum.unwrap_or(0) + value); }
        }
    }

    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(total) = self.total { parts.push(format!("{total} tokens")); }
        if let Some(input) = self.input { parts.push(format!("{input} input")); }
        if let Some(output) = self.output { parts.push(format!("{output} output")); }
        if parts.is_empty() { parts.push("Token usage not reported".into()); }
        else if self.partial { parts.push("partial usage".into()); }
        parts.push(format!("{:.1}s", self.elapsed.as_secs_f64()));
        parts.push(format!("{} API request{}", self.requests, if self.requests == 1 { "" } else { "s" }));
        parts.push(format!("{} tool call{}", self.tool_calls, if self.tool_calls == 1 { "" } else { "s" }));
        parts.join(" · ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn totals_include_every_request_and_preserve_reported_total() {
        let mut stats = TurnStats::default();
        stats.record(&json!({"prompt_tokens":100,"completion_tokens":20,"total_tokens":120}));
        stats.record(&json!({"prompt_tokens":150,"completion_tokens":30}));
        stats.elapsed = Duration::from_millis(1250);
        stats.tool_calls = 2;
        assert_eq!(stats.label(), "300 tokens · 250 input · 50 output · 1.2s · 2 API requests · 2 tool calls");
        stats.record(&json!({"prompt_tokens":1,"completion_tokens":2,"total_tokens":7}));
        assert!(stats.label().starts_with("307 tokens · 251 input · 52 output"));
    }

    #[test]
    fn missing_malformed_and_partial_usage_are_explicit() {
        let mut stats = TurnStats::default();
        stats.record(&Value::Null);
        assert!(stats.label().starts_with("Token usage not reported"));
        stats.record(&json!({"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}));
        assert!(stats.label().starts_with("12 tokens · 10 input · 2 output · partial usage"));
        let mut stats = TurnStats::default();
        stats.record(&json!({"prompt_tokens":-1,"completion_tokens":"secret","total_tokens":1.5}));
        assert!(stats.label().starts_with("Token usage not reported"));
        stats.record(&json!({"total_tokens":0}));
        assert!(stats.label().starts_with("0 tokens · partial usage"));
    }
}
