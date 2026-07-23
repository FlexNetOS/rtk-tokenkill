//! Canonical registry of RTK agent integrations.
//!
//! Installation, verification, the server API, and the dashboard all consume
//! this registry so they cannot silently disagree about which agents are
//! intercepted automatically and which only receive prompt instructions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationKind {
    NativeHook,
    Plugin,
    PromptOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IntegrationScope {
    Global,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Integration {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: IntegrationKind,
    pub scope: IntegrationScope,
    pub automatically_intercepts: bool,
    pub hook_command: Option<&'static str>,
}

pub const INTEGRATIONS: &[Integration] = &[
    Integration {
        id: "claude",
        name: "Claude Code",
        kind: IntegrationKind::NativeHook,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: Some("rtk hook claude"),
    },
    Integration {
        id: "codex",
        name: "OpenAI Codex",
        kind: IntegrationKind::NativeHook,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: Some("rtk hook codex"),
    },
    Integration {
        id: "cursor",
        name: "Cursor",
        kind: IntegrationKind::NativeHook,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: Some("rtk hook cursor"),
    },
    Integration {
        id: "gemini",
        name: "Gemini CLI",
        kind: IntegrationKind::NativeHook,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: Some("rtk hook gemini"),
    },
    Integration {
        id: "copilot",
        name: "GitHub Copilot",
        kind: IntegrationKind::NativeHook,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: Some("rtk hook copilot"),
    },
    Integration {
        id: "droid",
        name: "Factory Droid",
        kind: IntegrationKind::NativeHook,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: Some("rtk hook droid"),
    },
    Integration {
        id: "opencode",
        name: "OpenCode",
        kind: IntegrationKind::Plugin,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: None,
    },
    Integration {
        id: "pi",
        name: "Pi",
        kind: IntegrationKind::Plugin,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: None,
    },
    Integration {
        id: "hermes",
        name: "Hermes",
        kind: IntegrationKind::Plugin,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: None,
    },
    Integration {
        id: "openclaw",
        name: "OpenClaw",
        kind: IntegrationKind::Plugin,
        scope: IntegrationScope::Global,
        automatically_intercepts: true,
        hook_command: None,
    },
    Integration {
        id: "windsurf",
        name: "Windsurf",
        kind: IntegrationKind::PromptOnly,
        scope: IntegrationScope::Project,
        automatically_intercepts: false,
        hook_command: None,
    },
    Integration {
        id: "cline",
        name: "Cline / Roo Code",
        kind: IntegrationKind::PromptOnly,
        scope: IntegrationScope::Project,
        automatically_intercepts: false,
        hook_command: None,
    },
    Integration {
        id: "kilocode",
        name: "Kilo Code",
        kind: IntegrationKind::PromptOnly,
        scope: IntegrationScope::Project,
        automatically_intercepts: false,
        hook_command: None,
    },
    Integration {
        id: "antigravity",
        name: "Google Antigravity",
        kind: IntegrationKind::PromptOnly,
        scope: IntegrationScope::Project,
        automatically_intercepts: false,
        hook_command: None,
    },
    Integration {
        id: "kimi",
        name: "Kimi AI",
        kind: IntegrationKind::PromptOnly,
        scope: IntegrationScope::Project,
        automatically_intercepts: false,
        hook_command: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn registry_ids_are_unique() {
        let mut ids = HashSet::new();
        for integration in INTEGRATIONS {
            assert!(
                ids.insert(integration.id),
                "duplicate id: {}",
                integration.id
            );
        }
    }

    #[test]
    fn prompt_only_integrations_never_claim_automatic_interception() {
        for integration in INTEGRATIONS
            .iter()
            .filter(|entry| entry.kind == IntegrationKind::PromptOnly)
        {
            assert!(!integration.automatically_intercepts, "{}", integration.id);
            assert!(integration.hook_command.is_none(), "{}", integration.id);
        }
    }

    #[test]
    fn every_native_hook_has_a_hook_command() {
        for integration in INTEGRATIONS
            .iter()
            .filter(|entry| entry.kind == IntegrationKind::NativeHook)
        {
            assert!(integration.hook_command.is_some(), "{}", integration.id);
        }
    }

    #[test]
    fn registry_lookup_is_exact() {
        assert_eq!(
            INTEGRATIONS
                .iter()
                .find(|entry| entry.id == "codex")
                .map(|entry| entry.name),
            Some("OpenAI Codex")
        );
        assert!(INTEGRATIONS.iter().all(|entry| entry.id != "Codex"));
    }
}
