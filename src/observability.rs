//! Shared read-only data transfer objects for the HTTP server and dashboard.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use crate::core::config::Config;
use crate::core::tracking::{GainSummary, ParseFailureSummary, Tracker};
use crate::hooks::integrations::IntegrationKind;
use crate::hooks::verify_cmd::{AgentVerification, AllAgentsVerification, Check};

pub const API_VERSION: &str = "v1";
const AUDIT_TAIL_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ApiEnvelope<T> {
    pub api_version: String,
    pub data: T,
}

impl<T> ApiEnvelope<T> {
    pub fn new(data: T) -> Self {
        Self {
            api_version: API_VERSION.to_string(),
            data,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HealthPayload {
    pub status: String,
    pub version: String,
    pub api_version: String,
}

impl HealthPayload {
    pub fn healthy() -> Self {
        Self {
            status: "ok".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: API_VERSION.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookPayload {
    pub id: String,
    pub name: String,
    pub command: Option<String>,
    pub status: crate::hooks::verify_cmd::AgentStatus,
    pub registration: Check,
    pub payload_compatibility: Check,
    pub live_rewrite: Check,
    pub audit_logging: Check,
    pub trust: Check,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GainPayload {
    pub total_commands: usize,
    pub total_input: usize,
    pub total_output: usize,
    pub total_saved: usize,
    pub avg_savings_pct: f64,
    pub total_time_ms: u64,
    pub avg_time_ms: u64,
    pub by_command: Vec<CommandGainPayload>,
    pub by_day: Vec<DayGainPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandGainPayload {
    pub command: String,
    pub count: usize,
    pub saved_tokens: usize,
    pub avg_savings_pct: f64,
    pub avg_time_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DayGainPayload {
    pub date: String,
    pub saved_tokens: usize,
}

impl From<GainSummary> for GainPayload {
    fn from(summary: GainSummary) -> Self {
        Self {
            total_commands: summary.total_commands,
            total_input: summary.total_input,
            total_output: summary.total_output,
            total_saved: summary.total_saved,
            avg_savings_pct: summary.avg_savings_pct,
            total_time_ms: summary.total_time_ms,
            avg_time_ms: summary.avg_time_ms,
            by_command: summary
                .by_command
                .into_iter()
                .map(
                    |(command, count, saved_tokens, avg_savings_pct, avg_time_ms)| {
                        CommandGainPayload {
                            command,
                            count,
                            saved_tokens,
                            avg_savings_pct,
                            avg_time_ms,
                        }
                    },
                )
                .collect(),
            by_day: summary
                .by_day
                .into_iter()
                .map(|(date, saved_tokens)| DayGainPayload { date, saved_tokens })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FailuresPayload {
    pub total: usize,
    pub recovery_rate: f64,
    pub top_commands: Vec<FailureCountPayload>,
    pub recent: Vec<FailurePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureCountPayload {
    pub command: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailurePayload {
    pub timestamp: String,
    pub command: String,
    pub error: String,
    pub fallback_succeeded: bool,
}

impl From<ParseFailureSummary> for FailuresPayload {
    fn from(summary: ParseFailureSummary) -> Self {
        Self {
            total: summary.total,
            recovery_rate: summary.recovery_rate,
            top_commands: summary
                .top_commands
                .into_iter()
                .map(|(command, count)| FailureCountPayload { command, count })
                .collect(),
            recent: summary
                .recent
                .into_iter()
                .map(|failure| FailurePayload {
                    timestamp: failure.timestamp,
                    command: failure.raw_command,
                    error: failure.error_message,
                    fallback_succeeded: failure.fallback_succeeded,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AuditPayload {
    pub enabled: bool,
    pub configured: bool,
    pub path: Option<String>,
    pub truncated: bool,
    pub entries: Vec<AuditPayloadEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditPayloadEntry {
    pub timestamp: String,
    pub action: String,
    pub original_command: String,
    pub rewritten_command: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardSnapshot {
    pub agents: AllAgentsVerification,
    pub hooks: Vec<HookPayload>,
    pub gain: GainPayload,
    pub failures: FailuresPayload,
    pub audit: AuditPayload,
    pub config: Value,
    pub icm: crate::icm_bridge::IcmHealth,
}

pub fn collect_snapshot(icm_url: Option<&str>) -> Result<DashboardSnapshot> {
    let agents = collect_agents()?;
    let hooks = hooks_from_agents(&agents);
    Ok(DashboardSnapshot {
        agents,
        hooks,
        gain: collect_gain()?,
        failures: collect_failures()?,
        audit: collect_audit(100)?,
        config: collect_redacted_config()?,
        icm: crate::icm_bridge::check(icm_url),
    })
}

pub fn collect_agents() -> Result<AllAgentsVerification> {
    crate::hooks::verify_cmd::collect_all_agents_read_only()
}

pub fn hooks_from_agents(report: &AllAgentsVerification) -> Vec<HookPayload> {
    report
        .agents
        .iter()
        .filter(|agent| agent.kind == IntegrationKind::NativeHook)
        .map(hook_from_agent)
        .collect()
}

fn hook_from_agent(agent: &AgentVerification) -> HookPayload {
    let command = crate::hooks::integrations::INTEGRATIONS
        .iter()
        .find(|integration| integration.id == agent.id)
        .and_then(|integration| integration.hook_command)
        .map(str::to_string);
    HookPayload {
        id: agent.id.clone(),
        name: agent.name.clone(),
        command,
        status: agent.status,
        registration: agent.registration.clone(),
        payload_compatibility: agent.payload_compatibility.clone(),
        live_rewrite: agent.live_rewrite.clone(),
        audit_logging: agent.audit_logging.clone(),
        trust: agent.trust.clone(),
    }
}

pub fn collect_gain() -> Result<GainPayload> {
    Ok(match Tracker::open_read_only()? {
        Some(tracker) => tracker.get_summary()?.into(),
        None => GainPayload::default(),
    })
}

pub fn collect_failures() -> Result<FailuresPayload> {
    Ok(match Tracker::open_read_only()? {
        Some(tracker) => tracker.get_parse_failure_summary()?.into(),
        None => FailuresPayload::default(),
    })
}

pub fn collect_audit(limit: usize) -> Result<AuditPayload> {
    let enabled = std::env::var("RTK_HOOK_AUDIT").as_deref() == Ok("1");
    let Some(path) = crate::hooks::audit::log_path() else {
        return Ok(AuditPayload {
            enabled,
            ..AuditPayload::default()
        });
    };
    let display_path = path.display().to_string();
    if !path.is_file() {
        return Ok(AuditPayload {
            enabled,
            configured: true,
            path: Some(display_path),
            ..AuditPayload::default()
        });
    }

    let mut file = File::open(&path)
        .with_context(|| format!("Failed to open audit log: {}", path.display()))?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(AUDIT_TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    let content = String::from_utf8_lossy(&bytes);
    let mut lines = content.lines();
    if start > 0 {
        lines.next();
    }
    let parsed = lines.filter_map(parse_audit_line).collect::<Vec<_>>();
    let from = parsed.len().saturating_sub(limit);
    let truncated = start > 0 || from > 0;
    let entries = parsed.into_iter().skip(from).collect();

    Ok(AuditPayload {
        enabled,
        configured: true,
        path: Some(display_path),
        truncated,
        entries,
    })
}

fn parse_audit_line(line: &str) -> Option<AuditPayloadEntry> {
    let mut parts = line.splitn(4, " | ");
    Some(AuditPayloadEntry {
        timestamp: parts.next()?.to_string(),
        action: parts.next()?.to_string(),
        original_command: parts.next()?.to_string(),
        rewritten_command: parts.next().unwrap_or("-").to_string(),
    })
}

pub fn collect_redacted_config() -> Result<Value> {
    let mut value = serde_json::to_value(Config::load()?)?;
    redact_value(&mut value);
    Ok(value)
}

pub fn redact_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                if sensitive_key(key) {
                    *value = Value::String("[REDACTED]".to_string());
                } else {
                    redact_value(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_value),
        _ => {}
    }
}

fn sensitive_key(key: &str) -> bool {
    let normalized = key
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let segments = normalized
        .split('_')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let compact = segments.join("");
    segments.iter().any(|segment| {
        matches!(
            *segment,
            "auth" | "secret" | "token" | "password" | "credential" | "authorization" | "cookie"
        )
    }) || [
        "secret",
        "token",
        "password",
        "credential",
        "authorization",
        "cookie",
        "apikey",
        "privatekey",
        "accesskey",
        "secretkey",
    ]
    .iter()
    .any(|suffix| compact.ends_with(suffix))
        || normalized == "path"
        || normalized.ends_with("_path")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn api_envelope_is_explicitly_versioned() {
        let envelope = ApiEnvelope::new(json!({"ok": true}));
        assert_eq!(envelope.api_version, "v1");
    }

    #[test]
    fn recursive_redaction_covers_secrets_and_paths_without_false_key_matches() {
        let mut value = json!({
            "api_key": "key-value",
            "nested": {
                "access_token": "token-value",
                "database_path": "/private/location",
                "monkey": "visible",
                "clientSecret": "camel-secret"
            },
            "items": [{"password": "password-value"}]
        });
        redact_value(&mut value);
        assert_eq!(value["api_key"], "[REDACTED]");
        assert_eq!(value["nested"]["access_token"], "[REDACTED]");
        assert_eq!(value["nested"]["database_path"], "[REDACTED]");
        assert_eq!(value["nested"]["clientSecret"], "[REDACTED]");
        assert_eq!(value["items"][0]["password"], "[REDACTED]");
        assert_eq!(value["nested"]["monkey"], "visible");
        let serialized = serde_json::to_string(&value).unwrap();
        for secret in [
            "key-value",
            "token-value",
            "password-value",
            "camel-secret",
            "/private/location",
        ] {
            assert!(!serialized.contains(secret));
        }
    }

    #[test]
    fn audit_reader_is_bounded_and_keeps_latest_entries() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("hook-audit.log");
        let mut file = File::create(&path).unwrap();
        for index in 0..5 {
            writeln!(
                file,
                "2026-07-22T00:00:0{index}Z | rewrite | git status {index} | rtk git status {index}"
            )
            .unwrap();
        }
        drop(file);
        let content = std::fs::read_to_string(&path).unwrap();
        let entries = content
            .lines()
            .filter_map(parse_audit_line)
            .collect::<Vec<_>>();
        let from = entries.len().saturating_sub(2);
        let latest = entries.into_iter().skip(from).collect::<Vec<_>>();
        assert_eq!(latest.len(), 2);
        assert!(latest[0].original_command.ends_with('3'));
        assert!(latest[1].original_command.ends_with('4'));
    }
}
