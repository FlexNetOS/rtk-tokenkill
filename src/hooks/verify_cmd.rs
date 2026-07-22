//! Runs TOML filter inline tests to make sure filter rules work correctly.

use anyhow::{Context, Result};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::toml_filter;
use crate::discover::registry::rewrite_command;

use super::constants::{
    COPILOT_HOOK_FILE, COPILOT_INSTRUCTIONS_FILE, DROID_HOOKS_FILE, DROID_HOOKS_SUBDIR,
    DROID_SETTINGS_FILE, GEMINI_HOOK_FILE, HERMES_PLUGINS_SUBDIR, HERMES_PLUGIN_INIT_FILE,
    HERMES_PLUGIN_MANIFEST_FILE, HERMES_PLUGIN_NAME, HOOKS_JSON, HOOKS_SUBDIR, SETTINGS_JSON,
};
use super::init::IntegrationPaths;
use super::integrations::{Integration, IntegrationKind, IntegrationScope, INTEGRATIONS};
use super::integrity;

const AGENTS_MD: &str = "AGENTS.md";
const GEMINI_MD: &str = "GEMINI.md";
const RTK_MD: &str = "RTK.md";

/// Run TOML filter inline tests.
///
/// - `filter`: if `Some`, only run tests for that filter name
/// - `require_all`: fail if any filter has no inline tests
pub fn run(filter: Option<String>, require_all: bool) -> Result<()> {
    let results = toml_filter::run_filter_tests(filter.as_deref());

    let total = results.outcomes.len();
    let passed = results.outcomes.iter().filter(|o| o.passed).count();
    let failed = total - passed;

    // Print failures with details
    for outcome in &results.outcomes {
        if !outcome.passed {
            eprintln!(
                "FAIL [{}] {}\n  expected: {:?}\n  actual:   {:?}",
                outcome.filter_name, outcome.test_name, outcome.expected, outcome.actual
            );
        }
    }

    if total == 0 {
        println!("No inline tests found.");
    } else {
        println!("{}/{} tests passed", passed, total);
    }

    if require_all && !results.filters_without_tests.is_empty() {
        for name in &results.filters_without_tests {
            eprintln!("MISSING tests for filter: {}", name);
        }
        anyhow::bail!(
            "{} filter(s) have no inline tests (use --require-all in CI)",
            results.filters_without_tests.len()
        );
    }

    if failed > 0 {
        anyhow::bail!("{} test(s) failed", failed);
    }

    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Incomplete,
    NotApplicable,
    NotEnabled,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PathCheck {
    pub path: String,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct HashCheck {
    pub path: String,
    pub expected_sha256: Option<String>,
    pub actual_sha256: Option<String>,
    pub status: CheckStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentStatus {
    Ready,
    Incomplete,
    Failed,
    PromptOnly,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentVerification {
    pub id: String,
    pub name: String,
    pub kind: IntegrationKind,
    pub scope: IntegrationScope,
    pub automatically_intercepts: bool,
    pub status: AgentStatus,
    pub registration: Check,
    pub paths: Vec<PathCheck>,
    pub hashes: Vec<HashCheck>,
    pub payload_compatibility: Check,
    pub live_rewrite: Check,
    pub audit_logging: Check,
    pub trust: Check,
    pub stale_artifacts: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AllAgentsVerification {
    pub schema_version: u8,
    pub complete: bool,
    pub agents: Vec<AgentVerification>,
}

pub fn run_all_agents(format: &str) -> Result<bool> {
    let report = collect_all_agents()?;
    match format {
        "text" => print_text_report(&report),
        "json" => println!(
            "{}",
            serde_json::to_string_pretty(&report).context("Failed to serialize verification")?
        ),
        value => anyhow::bail!("unsupported verify format '{value}' (expected text or json)"),
    }
    Ok(report.complete)
}

pub fn collect_all_agents() -> Result<AllAgentsVerification> {
    let paths = IntegrationPaths::resolve()?;
    collect_all_agents_at(&paths)
}

pub(crate) fn collect_all_agents_at(paths: &IntegrationPaths) -> Result<AllAgentsVerification> {
    let rewrite = live_rewrite_check();
    let audit = audit_check();
    let binary_hash = current_binary_hash();
    let mut agents = Vec::with_capacity(INTEGRATIONS.len());

    for integration in INTEGRATIONS {
        agents.push(verify_integration(
            integration,
            paths,
            &rewrite,
            &audit,
            binary_hash.clone(),
        )?);
    }

    let complete = agents
        .iter()
        .all(|agent| !matches!(agent.status, AgentStatus::Failed | AgentStatus::Incomplete));
    Ok(AllAgentsVerification {
        schema_version: 1,
        complete,
        agents,
    })
}

fn verify_integration(
    integration: &Integration,
    paths: &IntegrationPaths,
    live_rewrite: &Check,
    audit: &Check,
    binary_hash: Option<HashCheck>,
) -> Result<AgentVerification> {
    let (registration, registration_paths, occurrences) = registration_check(integration, paths)?;
    let mut path_checks = registration_paths
        .iter()
        .map(|path| path_check(path))
        .collect::<Vec<_>>();
    if path_checks.is_empty() {
        path_checks.push(PathCheck {
            path: "n/a".to_string(),
            status: CheckStatus::NotApplicable,
            detail: "no filesystem registration".to_string(),
        });
    }

    let mut hashes = expected_artifacts(integration.id, paths)
        .into_iter()
        .map(|(path, expected)| hash_check(&path, Some(expected)))
        .collect::<Vec<_>>();
    if integration.kind == IntegrationKind::NativeHook {
        if let Some(binary_hash) = binary_hash {
            hashes.push(binary_hash);
        }
    }

    let payload_compatibility = if integration.kind == IntegrationKind::NativeHook {
        if super::hook_cmd::payload_compatibility_probe(integration.id) {
            pass("registered host payload is accepted by the native adapter")
        } else {
            fail("native adapter rejected its compatibility fixture")
        }
    } else {
        not_applicable("native hook payload protocol is not used")
    };

    let trust = if integration.id == "codex" && registration.status == CheckStatus::Pass {
        Check {
            status: CheckStatus::Incomplete,
            detail: "review and trust hooks.json from Codex /hooks; RTK does not read or modify Codex's trust database".to_string(),
        }
    } else if integration.kind == IntegrationKind::NativeHook {
        pass("registration is inside the host's supported hook surface")
    } else {
        not_applicable("this integration has no native hook trust record")
    };

    let audit_logging = if integration.automatically_intercepts {
        audit.clone()
    } else {
        not_applicable("prompt-only integration emits no hook audit events")
    };

    let stale_artifacts = stale_artifacts(integration.id, paths, occurrences)?;
    let any_failure = registration.status == CheckStatus::Fail
        || path_checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail)
        || hashes.iter().any(|check| check.status == CheckStatus::Fail)
        || payload_compatibility.status == CheckStatus::Fail
        || live_rewrite.status == CheckStatus::Fail
        || audit_logging.status == CheckStatus::Fail
        || !stale_artifacts.is_empty();
    let any_incomplete = trust.status == CheckStatus::Incomplete
        || matches!(
            audit_logging.status,
            CheckStatus::Incomplete | CheckStatus::NotEnabled
        );
    let status = if any_failure {
        AgentStatus::Failed
    } else if any_incomplete {
        AgentStatus::Incomplete
    } else if integration.kind == IntegrationKind::PromptOnly {
        AgentStatus::PromptOnly
    } else {
        AgentStatus::Ready
    };

    Ok(AgentVerification {
        id: integration.id.to_string(),
        name: integration.name.to_string(),
        kind: integration.kind,
        scope: integration.scope,
        automatically_intercepts: integration.automatically_intercepts,
        status,
        registration,
        paths: path_checks,
        hashes,
        payload_compatibility,
        live_rewrite: if integration.automatically_intercepts {
            live_rewrite.clone()
        } else {
            not_applicable("prompt-only integration does not intercept commands")
        },
        audit_logging,
        trust,
        stale_artifacts,
    })
}

fn registration_check(
    integration: &Integration,
    paths: &IntegrationPaths,
) -> Result<(Check, Vec<PathBuf>, usize)> {
    let (registered, files, occurrences, detail) = match integration.id {
        "claude" => {
            let path = paths.claude_dir.join(SETTINGS_JSON);
            let count = count_hook_command(&path, "claude")?;
            (count == 1, vec![path], count, "Claude PreToolUse/Bash")
        }
        "codex" => {
            let path = paths.codex_dir.join(HOOKS_JSON);
            let count = count_hook_command(&path, "codex")?;
            (count == 1, vec![path], count, "Codex PreToolUse/Bash")
        }
        "cursor" => {
            let path = paths.cursor_dir.join(HOOKS_JSON);
            let count = count_hook_command(&path, "cursor")?;
            (count == 1, vec![path], count, "Cursor preToolUse/Shell")
        }
        "gemini" => {
            let settings = paths.gemini_dir.join(SETTINGS_JSON);
            let hook = paths.gemini_dir.join(HOOKS_SUBDIR).join(GEMINI_HOOK_FILE);
            let count = count_json_string(&settings, &hook.to_string_lossy())?;
            (
                count == 1 && hook.exists(),
                vec![settings, hook],
                count,
                "Gemini BeforeTool/run_shell_command",
            )
        }
        "copilot" => {
            let hook = paths.copilot_dir.join(HOOKS_SUBDIR).join(COPILOT_HOOK_FILE);
            let count = count_hook_command(&hook, "copilot")?;
            (
                count == 3,
                vec![hook, paths.copilot_dir.join(COPILOT_INSTRUCTIONS_FILE)],
                count,
                "Copilot VS Code and CLI schemas",
            )
        }
        "droid" => {
            let files = [
                paths.droid_dir.join(DROID_HOOKS_FILE),
                paths
                    .droid_dir
                    .join(DROID_HOOKS_SUBDIR)
                    .join(DROID_HOOKS_FILE),
                paths.droid_dir.join(DROID_SETTINGS_FILE),
            ];
            let count = files.iter().try_fold(0, |count, path| {
                count_hook_command(path, "droid").map(|next| count + next)
            })?;
            let existing = files.iter().filter(|path| path.exists()).cloned().collect();
            (count == 1, existing, count, "Droid PreToolUse/Execute")
        }
        "opencode" => (
            file_equals(
                &paths.opencode_plugin,
                include_bytes!("../../hooks/opencode/rtk.ts"),
            ),
            vec![paths.opencode_plugin.clone()],
            usize::from(paths.opencode_plugin.exists()),
            "OpenCode plugin",
        ),
        "pi" => (
            file_equals(&paths.pi_plugin, include_bytes!("../../hooks/pi/rtk.ts")),
            vec![paths.pi_plugin.clone()],
            usize::from(paths.pi_plugin.exists()),
            "Pi extension",
        ),
        "hermes" => {
            let plugin = paths
                .hermes_home
                .join(HERMES_PLUGINS_SUBDIR)
                .join(HERMES_PLUGIN_NAME);
            let init = plugin.join(HERMES_PLUGIN_INIT_FILE);
            let manifest = plugin.join(HERMES_PLUGIN_MANIFEST_FILE);
            let config = paths.hermes_home.join("config.yaml");
            let registered = file_equals(
                &init,
                include_bytes!("../../hooks/hermes/rtk-rewrite/__init__.py"),
            ) && file_equals(
                &manifest,
                include_bytes!("../../hooks/hermes/rtk-rewrite/plugin.yaml"),
            ) && fs::read_to_string(&config)
                .ok()
                .is_some_and(|content| content.contains(HERMES_PLUGIN_NAME));
            (
                registered,
                vec![init, manifest, config],
                usize::from(registered),
                "Hermes plugin",
            )
        }
        "windsurf" => prompt_registration(&paths.project_dir.join(".windsurfrules")),
        "cline" => prompt_registration(&paths.project_dir.join(".clinerules")),
        "kilocode" => prompt_registration(&paths.project_dir.join(".kilocode/rules/rtk-rules.md")),
        "antigravity" => prompt_registration(
            &paths
                .project_dir
                .join(".agents/rules/antigravity-rtk-rules.md"),
        ),
        "kimi" => prompt_registration(&paths.project_dir.join(AGENTS_MD)),
        unknown => anyhow::bail!("registry integration has no verifier: {unknown}"),
    };

    let check = if registered {
        pass(format!("{detail} registration is present"))
    } else if occurrences > 1 {
        fail(format!(
            "{detail} has {occurrences} registrations; expected one"
        ))
    } else {
        fail(format!("{detail} registration is missing or invalid"))
    };
    Ok((check, files, occurrences))
}

fn prompt_registration(path: &Path) -> (bool, Vec<PathBuf>, usize, &'static str) {
    let present = fs::read_to_string(path).ok().is_some_and(|content| {
        content.contains("<!-- rtk-instructions") && content.contains("<!-- /rtk-instructions -->")
    });
    (
        present,
        vec![path.to_path_buf()],
        usize::from(present),
        "prompt-only rules",
    )
}

fn expected_artifacts(id: &str, paths: &IntegrationPaths) -> Vec<(PathBuf, &'static [u8])> {
    match id {
        "claude" => vec![(
            paths.claude_dir.join(RTK_MD),
            include_bytes!("../../hooks/claude/rtk-awareness.md"),
        )],
        "codex" => vec![(
            paths.codex_dir.join(RTK_MD),
            include_bytes!("../../hooks/codex/rtk-awareness.md"),
        )],
        "gemini" => vec![
            (
                paths.gemini_dir.join(HOOKS_SUBDIR).join(GEMINI_HOOK_FILE),
                b"#!/bin/bash\nexec rtk hook gemini\n",
            ),
            (
                paths.gemini_dir.join(GEMINI_MD),
                include_bytes!("../../hooks/claude/rtk-awareness.md"),
            ),
        ],
        "opencode" => vec![(
            paths.opencode_plugin.clone(),
            include_bytes!("../../hooks/opencode/rtk.ts"),
        )],
        "pi" => vec![(
            paths.pi_plugin.clone(),
            include_bytes!("../../hooks/pi/rtk.ts"),
        )],
        "hermes" => {
            let plugin = paths
                .hermes_home
                .join(HERMES_PLUGINS_SUBDIR)
                .join(HERMES_PLUGIN_NAME);
            vec![
                (
                    plugin.join(HERMES_PLUGIN_INIT_FILE),
                    include_bytes!("../../hooks/hermes/rtk-rewrite/__init__.py"),
                ),
                (
                    plugin.join(HERMES_PLUGIN_MANIFEST_FILE),
                    include_bytes!("../../hooks/hermes/rtk-rewrite/plugin.yaml"),
                ),
            ]
        }
        _ => Vec::new(),
    }
}

fn path_check(path: &Path) -> PathCheck {
    if path.is_file() {
        PathCheck {
            path: path.display().to_string(),
            status: CheckStatus::Pass,
            detail: "file exists".to_string(),
        }
    } else {
        PathCheck {
            path: path.display().to_string(),
            status: CheckStatus::Fail,
            detail: "file is missing".to_string(),
        }
    }
}

fn hash_check(path: &Path, expected: Option<&[u8]>) -> HashCheck {
    let expected_sha256 = expected.map(integrity::compute_hash_bytes);
    let actual_sha256 = integrity::compute_hash(path).ok();
    let status = match (&expected_sha256, &actual_sha256) {
        (Some(expected), Some(actual)) if expected == actual => CheckStatus::Pass,
        (None, Some(_)) => CheckStatus::Pass,
        _ => CheckStatus::Fail,
    };
    HashCheck {
        path: path.display().to_string(),
        expected_sha256,
        actual_sha256,
        status,
    }
}

fn current_binary_hash() -> Option<HashCheck> {
    let current = std::env::current_exe().ok()?;
    let expected_sha256 = integrity::compute_hash(&current).ok()?;
    #[cfg(test)]
    let resolved = Ok::<PathBuf, anyhow::Error>(current);
    #[cfg(not(test))]
    let resolved = crate::core::utils::resolve_binary("rtk");
    let (path, actual_sha256) = match resolved {
        Ok(path) => {
            let hash = integrity::compute_hash(&path).ok();
            (path.display().to_string(), hash)
        }
        Err(_) => ("rtk (unresolved on PATH)".to_string(), None),
    };
    let status = if actual_sha256.as_ref() == Some(&expected_sha256) {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    Some(HashCheck {
        path,
        expected_sha256: Some(expected_sha256),
        actual_sha256,
        status,
    })
}

fn live_rewrite_check() -> Check {
    match rewrite_command("git status", &[], &[]) {
        Some(rewritten) if rewritten == "rtk git status" => {
            pass("git status rewrites live to rtk git status")
        }
        Some(rewritten) => fail(format!("unexpected live rewrite: {rewritten}")),
        None => fail("live rewrite engine returned no rewrite for git status"),
    }
}

fn audit_check() -> Check {
    if std::env::var("RTK_HOOK_AUDIT").as_deref() != Ok("1") {
        return Check {
            status: CheckStatus::NotEnabled,
            detail: "RTK_HOOK_AUDIT is not 1; enable it to record hook events".to_string(),
        };
    }
    match super::audit::probe_writable() {
        Ok(path) => pass(format!("audit directory is appendable: {}", path.display())),
        Err(detail) => fail(detail),
    }
}

fn stale_artifacts(id: &str, paths: &IntegrationPaths, occurrences: usize) -> Result<Vec<String>> {
    let mut stale = Vec::new();
    if (id != "copilot" && occurrences > 1) || (id == "copilot" && occurrences > 3) {
        stale.push(format!("duplicate registrations: {occurrences}"));
    }
    let candidates = match id {
        "claude" => vec![
            paths.claude_dir.join(HOOKS_SUBDIR).join("rtk-rewrite.sh"),
            paths.claude_dir.join("settings.json.bak"),
        ],
        "codex" => vec![paths.codex_dir.join("hooks.json.bak")],
        "cursor" => vec![
            paths.cursor_dir.join(HOOKS_SUBDIR).join("rtk-rewrite.sh"),
            paths.cursor_dir.join("hooks.json.bak"),
        ],
        "droid" => vec![
            paths.droid_dir.join("hooks.json.bak"),
            paths
                .droid_dir
                .join(DROID_HOOKS_SUBDIR)
                .join("hooks.json.bak"),
            paths.droid_dir.join("settings.json.bak"),
        ],
        _ => Vec::new(),
    };
    for path in candidates {
        if path.exists()
            && (path.extension().and_then(|value| value.to_str()) != Some("bak")
                || count_hook_command(&path, id)? > 0)
        {
            stale.push(path.display().to_string());
        }
    }
    Ok(stale)
}

fn count_hook_command(path: &Path, agent: &str) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read hook registration: {}", path.display()))?;
    if path.extension().and_then(|value| value.to_str()) != Some("json")
        && path.extension().and_then(|value| value.to_str()) != Some("bak")
    {
        return Ok(usize::from(content.contains(&format!("rtk hook {agent}"))));
    }
    let root: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse hook registration: {}", path.display()))?;
    Ok(count_matching_strings(&root, &|value| {
        let parts = crate::discover::lexer::shell_split(value);
        let [binary, hook, target] = parts.as_slice() else {
            return false;
        };
        binary
            .rsplit(['/', '\\'])
            .next()
            .is_some_and(|name| name == "rtk")
            && hook == "hook"
            && target == agent
    }))
}

fn count_json_string(path: &Path, expected: &str) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let root: serde_json::Value = serde_json::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(count_matching_strings(&root, &|value| value == expected))
}

fn count_matching_strings(value: &serde_json::Value, predicate: &impl Fn(&str) -> bool) -> usize {
    match value {
        serde_json::Value::String(value) => usize::from(predicate(value)),
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| count_matching_strings(value, predicate))
            .sum(),
        serde_json::Value::Object(values) => values
            .values()
            .map(|value| count_matching_strings(value, predicate))
            .sum(),
        _ => 0,
    }
}

fn file_equals(path: &Path, expected: &[u8]) -> bool {
    fs::read(path).ok().as_deref() == Some(expected)
}

fn pass(detail: impl Into<String>) -> Check {
    Check {
        status: CheckStatus::Pass,
        detail: detail.into(),
    }
}

fn fail(detail: impl Into<String>) -> Check {
    Check {
        status: CheckStatus::Fail,
        detail: detail.into(),
    }
}

fn not_applicable(detail: impl Into<String>) -> Check {
    Check {
        status: CheckStatus::NotApplicable,
        detail: detail.into(),
    }
}

fn print_text_report(report: &AllAgentsVerification) {
    println!(
        "RTK all-agent verification (schema v{})",
        report.schema_version
    );
    for agent in &report.agents {
        println!(
            "{:<12} {:?} {}",
            agent.id, agent.status, agent.registration.detail
        );
        for (label, check) in [
            ("payload", &agent.payload_compatibility),
            ("rewrite", &agent.live_rewrite),
            ("audit", &agent.audit_logging),
            ("trust", &agent.trust),
        ] {
            if matches!(
                check.status,
                CheckStatus::Fail | CheckStatus::Incomplete | CheckStatus::NotEnabled
            ) {
                println!("  {label}: {}", check.detail);
            }
        }
        for path in &agent.paths {
            if path.status == CheckStatus::Fail {
                println!("  path: {} ({})", path.path, path.detail);
            }
        }
        for hash in &agent.hashes {
            if hash.status == CheckStatus::Fail {
                println!("  hash: {}", hash.path);
            }
        }
        for stale in &agent.stale_artifacts {
            println!("  stale: {stale}");
        }
    }
    println!(
        "overall: {}",
        if report.complete {
            "complete"
        } else {
            "incomplete"
        }
    );
}

#[cfg(test)]
mod all_agent_tests {
    use super::*;
    use crate::hooks::init::{install_all_agents_at, uninstall_all_agents_at, InitContext};
    use tempfile::TempDir;

    #[test]
    fn installed_report_is_structured_and_codex_requires_review() {
        let temp = TempDir::new().unwrap();
        let paths =
            IntegrationPaths::under(&temp.path().join("home"), &temp.path().join("project"));
        install_all_agents_at(&paths, InitContext::default()).unwrap();

        let report = collect_all_agents_at(&paths).unwrap();
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.agents.len(), INTEGRATIONS.len());
        assert!(!report.complete);
        let codex = report
            .agents
            .iter()
            .find(|agent| agent.id == "codex")
            .unwrap();
        assert_eq!(codex.status, AgentStatus::Incomplete, "{codex:#?}");
        assert!(codex.trust.detail.contains("/hooks"));
        for agent in report
            .agents
            .iter()
            .filter(|agent| agent.kind == IntegrationKind::PromptOnly)
        {
            assert_eq!(agent.status, AgentStatus::PromptOnly);
            assert!(!agent.automatically_intercepts);
        }

        uninstall_all_agents_at(&paths, InitContext::default()).unwrap();
    }

    #[test]
    fn modified_plugin_hash_fails_verification() {
        let temp = TempDir::new().unwrap();
        let paths =
            IntegrationPaths::under(&temp.path().join("home"), &temp.path().join("project"));
        install_all_agents_at(&paths, InitContext::default()).unwrap();
        fs::write(&paths.opencode_plugin, "modified").unwrap();

        let report = collect_all_agents_at(&paths).unwrap();
        let opencode = report
            .agents
            .iter()
            .find(|agent| agent.id == "opencode")
            .unwrap();
        assert_eq!(opencode.status, AgentStatus::Failed);
        assert!(opencode
            .hashes
            .iter()
            .any(|hash| hash.status == CheckStatus::Fail));
    }

    #[test]
    fn disabled_audit_keeps_intercepting_agent_incomplete() {
        let temp = TempDir::new().unwrap();
        let paths =
            IntegrationPaths::under(&temp.path().join("home"), &temp.path().join("project"));
        install_all_agents_at(&paths, InitContext::default()).unwrap();
        let claude = INTEGRATIONS
            .iter()
            .find(|agent| agent.id == "claude")
            .unwrap();

        let result = verify_integration(
            claude,
            &paths,
            &pass("rewrite probe passed"),
            &Check {
                status: CheckStatus::NotEnabled,
                detail: "audit disabled".to_string(),
            },
            current_binary_hash(),
        )
        .unwrap();

        assert_eq!(result.status, AgentStatus::Incomplete);
        assert_eq!(result.audit_logging.status, CheckStatus::NotEnabled);
    }

    #[test]
    fn copilot_requires_all_three_host_schema_commands() {
        let temp = TempDir::new().unwrap();
        let paths =
            IntegrationPaths::under(&temp.path().join("home"), &temp.path().join("project"));
        install_all_agents_at(&paths, InitContext::default()).unwrap();
        let hook = paths.copilot_dir.join(HOOKS_SUBDIR).join(COPILOT_HOOK_FILE);
        let mut root: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&hook).unwrap()).unwrap();
        root["hooks"]["preToolUse"][0]
            .as_object_mut()
            .unwrap()
            .remove("powershell");
        fs::write(&hook, serde_json::to_string_pretty(&root).unwrap()).unwrap();

        let report = collect_all_agents_at(&paths).unwrap();
        let copilot = report
            .agents
            .iter()
            .find(|agent| agent.id == "copilot")
            .unwrap();
        assert_eq!(copilot.status, AgentStatus::Failed);
        assert_eq!(copilot.registration.status, CheckStatus::Fail);
    }

    #[test]
    fn json_report_is_versioned_and_prompt_only_is_explicit() {
        let report = AllAgentsVerification {
            schema_version: 1,
            complete: true,
            agents: Vec::new(),
        };
        let json = serde_json::to_value(report).unwrap();
        assert_eq!(json["schema_version"], 1);
        assert_eq!(json["complete"], true);
    }
}
