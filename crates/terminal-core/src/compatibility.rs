use std::collections::BTreeMap;

use crate::{EndpointCapabilities, EndpointKind};

pub const DEFAULT_TERM: &str = "xterm-256color";

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TerminalEnvironment {
    vars: BTreeMap<String, String>,
}

impl TerminalEnvironment {
    #[must_use]
    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            vars: pairs.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn term(&self) -> Option<&str> {
        self.get("TERM")
    }

    #[must_use]
    pub fn colorterm(&self) -> Option<&str> {
        self.get("COLORTERM")
    }

    #[must_use]
    pub fn tmux(&self) -> Option<&str> {
        self.get("TMUX")
    }

    #[must_use]
    pub fn ssh_connection(&self) -> Option<&str> {
        self.get("SSH_CONNECTION")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilitySeverity {
    Info,
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityCode {
    MissingTerm,
    UnknownTerm,
    TmuxEnvironmentMismatch,
    RemoteLocalityMismatch,
    NonPersistentTmuxEndpoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompatibilityDiagnostic {
    pub severity: CompatibilitySeverity,
    pub code: CompatibilityCode,
    pub message: &'static str,
}

#[must_use]
pub fn diagnose_endpoint_environment(
    kind: EndpointKind,
    capabilities: EndpointCapabilities,
    environment: &TerminalEnvironment,
) -> Vec<CompatibilityDiagnostic> {
    let mut diagnostics = Vec::new();

    match environment.term() {
        None | Some("") => diagnostics.push(CompatibilityDiagnostic {
            severity: CompatibilitySeverity::Warning,
            code: CompatibilityCode::MissingTerm,
            message: "TERM is unset; terminal applications may not discover capabilities correctly",
        }),
        Some(term) if !is_recognized_term(term) => diagnostics.push(CompatibilityDiagnostic {
            severity: CompatibilitySeverity::Info,
            code: CompatibilityCode::UnknownTerm,
            message: "TERM is not in the core compatibility allowlist; rely on the host terminfo database",
        }),
        Some(_) => {}
    }

    let in_tmux = environment.tmux().is_some();
    if in_tmux && kind != EndpointKind::TmuxPane {
        diagnostics.push(CompatibilityDiagnostic {
            severity: CompatibilitySeverity::Info,
            code: CompatibilityCode::TmuxEnvironmentMismatch,
            message: "TMUX is present but the endpoint is not a tmux pane; treat tmux metadata as advisory",
        });
    }

    if kind == EndpointKind::TmuxPane && !capabilities.persistent {
        diagnostics.push(CompatibilityDiagnostic {
            severity: CompatibilitySeverity::Error,
            code: CompatibilityCode::NonPersistentTmuxEndpoint,
            message: "tmux endpoints must advertise persistence so the UI does not misrepresent session lifecycle",
        });
    }

    let appears_remote = environment.ssh_connection().is_some();
    if appears_remote && !capabilities.remote {
        diagnostics.push(CompatibilityDiagnostic {
            severity: CompatibilitySeverity::Info,
            code: CompatibilityCode::RemoteLocalityMismatch,
            message: "SSH_CONNECTION is present but the endpoint is not marked remote; environment metadata is advisory",
        });
    }

    diagnostics
}

#[must_use]
pub fn is_recognized_term(term: &str) -> bool {
    matches!(
        term,
        "xterm" | "xterm-256color" | "screen" | "screen-256color" | "tmux" | "tmux-256color"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> TerminalEnvironment {
        TerminalEnvironment::from_pairs(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned())),
        )
    }

    #[test]
    fn recognized_terms_do_not_require_a_warning() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::LocalPty,
            EndpointCapabilities::default(),
            &env(&[("TERM", "xterm-256color")]),
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn missing_term_is_visible_without_blocking_transport() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::LocalPty,
            EndpointCapabilities::default(),
            &TerminalEnvironment::default(),
        );
        assert_eq!(diagnostics[0].code, CompatibilityCode::MissingTerm);
        assert_eq!(diagnostics[0].severity, CompatibilitySeverity::Warning);
    }

    #[test]
    fn unknown_term_defers_to_host_terminfo() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::SshPty,
            EndpointCapabilities {
                remote: true,
                resize: true,
                ..EndpointCapabilities::default()
            },
            &env(&[("TERM", "wezterm")]),
        );
        assert_eq!(diagnostics[0].code, CompatibilityCode::UnknownTerm);
        assert_eq!(diagnostics[0].severity, CompatibilitySeverity::Info);
    }

    #[test]
    fn tmux_metadata_is_advisory_for_non_tmux_endpoints() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::SshPty,
            EndpointCapabilities {
                remote: true,
                ..EndpointCapabilities::default()
            },
            &env(&[("TERM", "tmux-256color"), ("TMUX", "/tmp/tmux-1000/default,1,0")]),
        );
        assert!(diagnostics
            .iter()
            .any(|item| item.code == CompatibilityCode::TmuxEnvironmentMismatch));
    }

    #[test]
    fn tmux_provider_must_advertise_persistence() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::TmuxPane,
            EndpointCapabilities {
                detachable: true,
                remote: true,
                ..EndpointCapabilities::default()
            },
            &env(&[("TERM", "tmux-256color")]),
        );
        assert!(diagnostics.iter().any(|item| {
            item.code == CompatibilityCode::NonPersistentTmuxEndpoint
                && item.severity == CompatibilitySeverity::Error
        }));
    }

    #[test]
    fn persistent_tmux_provider_need_not_fabricate_programmatic_detach() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::TmuxPane,
            EndpointCapabilities {
                detachable: false,
                persistent: true,
                resize: true,
                ..EndpointCapabilities::default()
            },
            &env(&[("TERM", "tmux-256color")]),
        );
        assert!(diagnostics.is_empty());
    }

    #[test]
    fn ssh_environment_does_not_override_provider_locality() {
        let diagnostics = diagnose_endpoint_environment(
            EndpointKind::LocalPty,
            EndpointCapabilities::default(),
            &env(&[("TERM", "xterm-256color"), ("SSH_CONNECTION", "a b c d")]),
        );
        assert!(diagnostics
            .iter()
            .any(|item| item.code == CompatibilityCode::RemoteLocalityMismatch));
    }
}
