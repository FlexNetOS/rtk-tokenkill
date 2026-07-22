//! Five-view terminal dashboard backed by local state or `rtk server`.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::io::{self, IsTerminal, Read, Write};
use std::time::Duration;

use crate::hooks::verify_cmd::{AgentStatus, CheckStatus};
use crate::observability::{
    ApiEnvelope, AuditPayload, DashboardSnapshot, FailuresPayload, GainPayload, HookPayload,
    API_VERSION,
};

const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DashboardView {
    Overview,
    Hooks,
    Agents,
    SavingsFailures,
    Icm,
}

impl DashboardView {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Hooks,
        Self::Agents,
        Self::SavingsFailures,
        Self::Icm,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Hooks => "Hooks",
            Self::Agents => "Agents",
            Self::SavingsFailures => "Savings / Failures",
            Self::Icm => "ICM",
        }
    }
}

pub fn run(server: Option<&str>, icm_url: Option<&str>) -> Result<()> {
    let mut snapshot = load_snapshot(server, icm_url)?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        print!("{}", render_all(&snapshot));
        return Ok(());
    }

    let mut active = DashboardView::Overview;
    loop {
        print!("\x1b[2J\x1b[H{}", render_view(active, &snapshot));
        println!("\n[1] Overview  [2] Hooks  [3] Agents  [4] Savings/Failures  [5] ICM");
        print!("[r] Refresh  [q] Quit > ");
        io::stdout().flush()?;
        let mut input = String::new();
        if io::stdin().read_line(&mut input)? == 0 {
            break;
        }
        match input.trim() {
            "1" => active = DashboardView::Overview,
            "2" => active = DashboardView::Hooks,
            "3" => active = DashboardView::Agents,
            "4" => active = DashboardView::SavingsFailures,
            "5" => active = DashboardView::Icm,
            "r" | "R" => snapshot = load_snapshot(server, icm_url)?,
            "q" | "Q" => break,
            _ => {}
        }
    }
    Ok(())
}

fn load_snapshot(server: Option<&str>, icm_url: Option<&str>) -> Result<DashboardSnapshot> {
    if let Some(server) = server {
        let token = std::env::var("RTK_SERVER_TOKEN")
            .context("RTK_SERVER_TOKEN is required with --server")?;
        if token.trim().is_empty() {
            anyhow::bail!("RTK_SERVER_TOKEN must not be empty with --server");
        }
        let mut snapshot = load_remote_snapshot(server, &token)?;
        if icm_url.is_some() {
            snapshot.icm = crate::icm_bridge::check(icm_url);
        }
        Ok(snapshot)
    } else {
        crate::observability::collect_snapshot(icm_url)
    }
}

fn load_remote_snapshot(server: &str, token: &str) -> Result<DashboardSnapshot> {
    let base = crate::icm_bridge::normalize_loopback_http_url(server, "RTK server")?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_millis(750))
        .timeout_read(Duration::from_secs(10))
        .timeout_write(Duration::from_millis(750))
        .build();
    let hooks = fetch::<Vec<HookPayload>>(&agent, &base, "/v1/hooks", token)?;
    let agents = fetch(&agent, &base, "/v1/agents", token)?;
    let gain = fetch::<GainPayload>(&agent, &base, "/v1/gain", token)?;
    let failures = fetch::<FailuresPayload>(&agent, &base, "/v1/failures", token)?;
    let audit = fetch::<AuditPayload>(&agent, &base, "/v1/audit", token)?;
    let config = fetch(&agent, &base, "/v1/config", token)?;
    let icm = fetch(&agent, &base, "/v1/icm", token)?;
    Ok(DashboardSnapshot {
        agents,
        hooks,
        gain,
        failures,
        audit,
        config,
        icm,
    })
}

fn fetch<T: DeserializeOwned>(
    agent: &ureq::Agent,
    base: &str,
    path: &str,
    token: &str,
) -> Result<T> {
    let response = agent
        .get(&format!("{base}{path}"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .with_context(|| format!("RTK server request failed for {path}"))?;
    let envelope: ApiEnvelope<T> =
        serde_json::from_reader(response.into_reader().take(MAX_RESPONSE_BYTES))
            .with_context(|| format!("RTK server returned invalid JSON for {path}"))?;
    if envelope.api_version != API_VERSION {
        anyhow::bail!(
            "RTK server API version mismatch for {path}: expected {API_VERSION}, received {}",
            envelope.api_version
        );
    }
    Ok(envelope.data)
}

fn render_all(snapshot: &DashboardSnapshot) -> String {
    DashboardView::ALL
        .iter()
        .map(|view| render_view(*view, snapshot))
        .collect::<Vec<_>>()
        .join("\n")
}

fn render_view(view: DashboardView, snapshot: &DashboardSnapshot) -> String {
    let mut output = format!("RTK Dashboard - {}\n{}\n", view.title(), "=".repeat(40));
    match view {
        DashboardView::Overview => render_overview(&mut output, snapshot),
        DashboardView::Hooks => render_hooks(&mut output, &snapshot.hooks),
        DashboardView::Agents => render_agents(&mut output, snapshot),
        DashboardView::SavingsFailures => {
            render_savings_failures(&mut output, &snapshot.gain, &snapshot.failures)
        }
        DashboardView::Icm => render_icm(&mut output, snapshot),
    }
    output
}

fn render_overview(output: &mut String, snapshot: &DashboardSnapshot) {
    let ready_agents = snapshot
        .agents
        .agents
        .iter()
        .filter(|agent| matches!(agent.status, AgentStatus::Ready | AgentStatus::PromptOnly))
        .count();
    let ready_hooks = snapshot
        .hooks
        .iter()
        .filter(|hook| hook.status == AgentStatus::Ready)
        .count();
    push_line(
        output,
        format!(
            "Agents: {ready_agents}/{} ready or prompt-only",
            snapshot.agents.agents.len()
        ),
    );
    push_line(
        output,
        format!("Hooks: {ready_hooks}/{} ready", snapshot.hooks.len()),
    );
    push_line(
        output,
        format!(
            "Savings: {} tokens ({:.1}%)",
            snapshot.gain.total_saved, snapshot.gain.avg_savings_pct
        ),
    );
    push_line(
        output,
        format!(
            "Failures: {} ({:.1}% recovered)",
            snapshot.failures.total, snapshot.failures.recovery_rate
        ),
    );
    push_line(
        output,
        format!(
            "Audit: {} entries{}",
            snapshot.audit.entries.len(),
            if snapshot.audit.enabled {
                " (enabled)"
            } else {
                " (disabled)"
            }
        ),
    );
    push_line(output, format!("ICM: {}", snapshot.icm.status));
}

fn render_hooks(output: &mut String, hooks: &[HookPayload]) {
    if hooks.is_empty() {
        push_line(output, "No native hooks registered in the report.");
        return;
    }
    for hook in hooks {
        push_line(
            output,
            format!(
                "{:<12} {:<10} {}",
                hook.id,
                status_name(hook.status),
                hook.command.as_deref().unwrap_or("-")
            ),
        );
        for (label, check) in [
            ("registration", &hook.registration),
            ("payload", &hook.payload_compatibility),
            ("audit", &hook.audit_logging),
            ("trust", &hook.trust),
        ] {
            if !matches!(check.status, CheckStatus::Pass | CheckStatus::NotApplicable) {
                push_line(output, format!("  {label}: {}", check.detail));
            }
        }
    }
}

fn render_agents(output: &mut String, snapshot: &DashboardSnapshot) {
    for agent in &snapshot.agents.agents {
        push_line(
            output,
            format!(
                "{:<12} {:<12} {:<10} intercepted={}",
                agent.id,
                format!("{:?}", agent.kind),
                status_name(agent.status),
                agent.automatically_intercepts
            ),
        );
    }
}

fn render_savings_failures(output: &mut String, gain: &GainPayload, failures: &FailuresPayload) {
    push_line(output, format!("Commands: {}", gain.total_commands));
    push_line(output, format!("Input tokens: {}", gain.total_input));
    push_line(output, format!("Output tokens: {}", gain.total_output));
    push_line(output, format!("Saved tokens: {}", gain.total_saved));
    push_line(
        output,
        format!("Average savings: {:.1}%", gain.avg_savings_pct),
    );
    if !gain.by_command.is_empty() {
        push_line(output, "Top savings commands:");
        for command in gain.by_command.iter().take(5) {
            push_line(
                output,
                format!(
                    "  {}: {} saved across {} runs",
                    command.command, command.saved_tokens, command.count
                ),
            );
        }
    }
    push_line(output, format!("Parse failures: {}", failures.total));
    push_line(
        output,
        format!("Recovery rate: {:.1}%", failures.recovery_rate),
    );
    for failure in failures.recent.iter().take(5) {
        push_line(
            output,
            format!(
                "  {}: {} (fallback={})",
                failure.timestamp, failure.command, failure.fallback_succeeded
            ),
        );
    }
}

fn render_icm(output: &mut String, snapshot: &DashboardSnapshot) {
    push_line(output, format!("Configured: {}", snapshot.icm.configured));
    push_line(output, format!("Reachable: {}", snapshot.icm.reachable));
    push_line(output, format!("Status: {}", snapshot.icm.status));
    push_line(output, format!("Detail: {}", snapshot.icm.detail));
}

fn push_line(output: &mut String, line: impl AsRef<str>) {
    output.push_str(line.as_ref());
    output.push('\n');
}

fn status_name(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Ready => "ready",
        AgentStatus::Incomplete => "incomplete",
        AgentStatus::Failed => "failed",
        AgentStatus::PromptOnly => "prompt-only",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::verify_cmd::AllAgentsVerification;
    use crate::icm_bridge::IcmHealth;
    use serde::Serialize;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    fn sample_snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            agents: AllAgentsVerification {
                schema_version: 1,
                complete: true,
                agents: Vec::new(),
            },
            hooks: Vec::new(),
            gain: GainPayload {
                total_commands: 4,
                total_input: 100,
                total_output: 25,
                total_saved: 75,
                avg_savings_pct: 75.0,
                ..GainPayload::default()
            },
            failures: FailuresPayload::default(),
            audit: AuditPayload::default(),
            config: json!({"tracking": {"enabled": true}}),
            icm: IcmHealth::not_configured(),
        }
    }

    fn response<T: Serialize>(value: T) -> Vec<u8> {
        serde_json::to_vec(&ApiEnvelope::new(value)).unwrap()
    }

    #[test]
    fn noninteractive_renderer_contains_exactly_five_named_views() {
        let rendered = render_all(&sample_snapshot());
        for title in ["Overview", "Hooks", "Agents", "Savings / Failures", "ICM"] {
            assert_eq!(
                rendered
                    .matches(&format!("RTK Dashboard - {title}\n"))
                    .count(),
                1
            );
        }
        assert!(rendered.contains("Savings: 75 tokens (75.0%)"));
    }

    #[test]
    fn remote_snapshot_uses_all_versioned_endpoints_and_bearer_token() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let snapshot = sample_snapshot();
        let server = thread::spawn(move || {
            for _ in 0..7 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut raw = [0_u8; 4096];
                let size = stream.read(&mut raw).unwrap();
                let request = String::from_utf8_lossy(&raw[..size]);
                assert!(request.contains("Authorization: Bearer dashboard-secret\r\n"));
                let path = request
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let body = match path {
                    "/v1/hooks" => response(snapshot.hooks.clone()),
                    "/v1/agents" => response(snapshot.agents.clone()),
                    "/v1/gain" => response(snapshot.gain.clone()),
                    "/v1/failures" => response(snapshot.failures.clone()),
                    "/v1/audit" => response(snapshot.audit.clone()),
                    "/v1/config" => response(snapshot.config.clone()),
                    "/v1/icm" => response(snapshot.icm.clone()),
                    unexpected => panic!("unexpected path: {unexpected}"),
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });

        let loaded =
            load_remote_snapshot(&format!("http://{address}"), "dashboard-secret").unwrap();
        server.join().unwrap();
        assert_eq!(loaded.gain.total_saved, 75);
        assert_eq!(loaded.agents.schema_version, 1);
    }

    #[test]
    fn remote_dashboard_rejects_non_loopback_servers() {
        assert!(load_remote_snapshot("http://192.0.2.10:8745", "secret").is_err());
    }
}
