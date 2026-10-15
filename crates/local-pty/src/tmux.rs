use std::fmt;

#[cfg(test)]
use std::ffi::{OsStr, OsString};
use terminal_core::{
    Endpoint, EndpointCapabilities, EndpointError, EndpointEvent, EndpointId, EndpointKind,
    EndpointLocality, EndpointSize,
};
#[cfg(test)]
use terminal_core::{TerminalEnvironment, diagnose_endpoint_environment};

use crate::{LocalProcessSpec, LocalProcessSpecError, LocalPtyEndpoint, LocalPtyState};

const MAX_SESSION_NAME_BYTES: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TmuxPtySpecError {
    EmptySession,
    OptionLikeSession,
    InvalidSessionCharacter,
    SessionTooLong,
    Process(LocalProcessSpecError),
}

impl fmt::Display for TmuxPtySpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySession => write!(f, "tmux session name must not be empty"),
            Self::OptionLikeSession => write!(f, "tmux session name must not begin with '-'"),
            Self::InvalidSessionCharacter => write!(
                f,
                "tmux session name must not contain ':', whitespace, or control characters"
            ),
            Self::SessionTooLong => write!(
                f,
                "tmux session name must be at most {MAX_SESSION_NAME_BYTES} UTF-8 bytes"
            ),
            Self::Process(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for TmuxPtySpecError {}

impl From<LocalProcessSpecError> for TmuxPtySpecError {
    fn from(error: LocalProcessSpecError) -> Self {
        Self::Process(error)
    }
}

/// Launch configuration for a system tmux client attached to the native PTY.
///
/// The provider asks tmux to attach to an existing named session or create it
/// if absent. tmux remains authoritative for its server, socket, session,
/// configuration, key bindings, panes, and persistence lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TmuxPtySpec {
    session: String,
    process: LocalProcessSpec,
}

impl TmuxPtySpec {
    /// Builds a system-tmux launch specification using `new-session -A -s`.
    ///
    /// # Errors
    ///
    /// Session names are deliberately bounded and reject option-like,
    /// colon-bearing, whitespace, or control-bearing values. The name is
    /// always passed as a distinct process argument; no shell command is
    /// constructed.
    pub fn new(
        session: impl Into<String>,
        initial_size: EndpointSize,
    ) -> Result<Self, TmuxPtySpecError> {
        let session = session.into();
        if session.trim().is_empty() {
            return Err(TmuxPtySpecError::EmptySession);
        }
        if session.starts_with('-') {
            return Err(TmuxPtySpecError::OptionLikeSession);
        }
        if session.len() > MAX_SESSION_NAME_BYTES {
            return Err(TmuxPtySpecError::SessionTooLong);
        }
        if session.chars().any(|character| {
            character == ':' || character.is_whitespace() || character.is_control()
        }) {
            return Err(TmuxPtySpecError::InvalidSessionCharacter);
        }

        let process = LocalProcessSpec::new("tmux", initial_size)?
            .arg("new-session")
            .arg("-A")
            .arg("-s")
            .arg(session.clone());
        Ok(Self { session, process })
    }

    #[must_use]
    pub fn session(&self) -> &str {
        &self.session
    }

    #[must_use]
    pub fn with_env(
        mut self,
        name: impl Into<std::ffi::OsString>,
        value: impl Into<std::ffi::OsString>,
    ) -> Self {
        self.process = self.process.with_env(name, value);
        self
    }

    #[cfg(test)]
    fn process(&self) -> &LocalProcessSpec {
        &self.process
    }
}

/// A persistent local terminal session implemented by running the user's
/// installed tmux client inside the existing native PTY transport.
///
/// Persistence is a tmux server/session property and is therefore advertised.
/// Programmatic detach is intentionally not advertised: this provider does not
/// assume or override the user's tmux prefix/key configuration. Interactive
/// tmux detach remains owned by tmux itself.
pub struct TmuxPtyEndpoint {
    inner: LocalPtyEndpoint,
}

impl TmuxPtyEndpoint {
    #[must_use]
    pub fn new(id: EndpointId, spec: TmuxPtySpec) -> Self {
        Self {
            inner: LocalPtyEndpoint::new(id, spec.process),
        }
    }

    #[must_use]
    pub fn state(&self) -> LocalPtyState {
        self.inner.state()
    }

    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        self.inner.process_id()
    }
}

impl Endpoint for TmuxPtyEndpoint {
    fn id(&self) -> &EndpointId {
        self.inner.id()
    }

    fn kind(&self) -> EndpointKind {
        EndpointKind::TmuxPane
    }

    fn locality(&self) -> &EndpointLocality {
        self.inner.locality()
    }

    fn capabilities(&self) -> EndpointCapabilities {
        EndpointCapabilities {
            detachable: false,
            persistent: true,
            remote: false,
            authoritative_scrollback: false,
            resize: true,
        }
    }

    fn attach(&mut self) -> Result<(), EndpointError> {
        self.inner.attach()
    }

    fn detach(&mut self) -> Result<(), EndpointError> {
        Err(EndpointError::Unsupported(
            "programmatic detach for tmux endpoints",
        ))
    }

    fn resize(&mut self, size: EndpointSize) -> Result<(), EndpointError> {
        self.inner.resize(size)
    }

    fn send(&mut self, input: &[u8]) -> Result<(), EndpointError> {
        self.inner.send(input)
    }

    fn poll_event(&mut self) -> Result<Option<EndpointEvent>, EndpointError> {
        self.inner.poll_event()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size() -> EndpointSize {
        EndpointSize::new(80, 24).unwrap()
    }

    #[test]
    fn tmux_process_is_direct_and_uses_attach_or_create() {
        let spec = TmuxPtySpec::new("work", size()).unwrap();
        assert_eq!(spec.process().program(), OsStr::new("tmux"));
        assert_eq!(
            spec.process().args(),
            [
                OsString::from("new-session"),
                OsString::from("-A"),
                OsString::from("-s"),
                OsString::from("work"),
            ]
        );
    }

    #[test]
    fn similarly_named_sessions_do_not_change_the_requested_identity() {
        let short = TmuxPtySpec::new("work", size()).unwrap();
        let long = TmuxPtySpec::new("workbench", size()).unwrap();
        assert_eq!(short.session(), "work");
        assert_eq!(long.session(), "workbench");
        assert_eq!(short.process().args().last(), Some(&OsString::from("work")));
        assert_eq!(
            long.process().args().last(),
            Some(&OsString::from("workbench"))
        );
    }

    #[test]
    fn validates_and_bounds_session_identity() {
        assert_eq!(
            TmuxPtySpec::new("", size()).unwrap_err(),
            TmuxPtySpecError::EmptySession
        );
        assert_eq!(
            TmuxPtySpec::new("   ", size()).unwrap_err(),
            TmuxPtySpecError::EmptySession
        );
        assert_eq!(
            TmuxPtySpec::new("-Lother", size()).unwrap_err(),
            TmuxPtySpecError::OptionLikeSession
        );
        assert_eq!(
            TmuxPtySpec::new("work:1", size()).unwrap_err(),
            TmuxPtySpecError::InvalidSessionCharacter
        );
        assert_eq!(
            TmuxPtySpec::new("work session", size()).unwrap_err(),
            TmuxPtySpecError::InvalidSessionCharacter
        );
        assert_eq!(
            TmuxPtySpec::new("work\n", size()).unwrap_err(),
            TmuxPtySpecError::InvalidSessionCharacter
        );
        assert_eq!(
            TmuxPtySpec::new("x".repeat(MAX_SESSION_NAME_BYTES + 1), size()).unwrap_err(),
            TmuxPtySpecError::SessionTooLong
        );
        assert_eq!(
            TmuxPtySpec::new("開発", size()).unwrap().session(),
            "開発"
        );
    }

    #[test]
    fn reports_only_proven_tmux_capabilities() {
        let endpoint = TmuxPtyEndpoint::new(
            EndpointId::new("tmux:work").unwrap(),
            TmuxPtySpec::new("work", size()).unwrap(),
        );
        assert_eq!(endpoint.kind(), EndpointKind::TmuxPane);
        assert_eq!(endpoint.locality(), &EndpointLocality::Local);
        assert_eq!(
            endpoint.capabilities(),
            EndpointCapabilities {
                detachable: false,
                persistent: true,
                remote: false,
                authoritative_scrollback: false,
                resize: true,
            }
        );
    }

    #[test]
    #[allow(clippy::assert_is_empty)]
    fn concrete_tmux_provider_has_no_error_level_self_diagnostic() {
        let endpoint = TmuxPtyEndpoint::new(
            EndpointId::new("tmux:work").unwrap(),
            TmuxPtySpec::new("work", size()).unwrap(),
        );
        let environment = TerminalEnvironment::from_pairs([(
            "TERM".to_owned(),
            "tmux-256color".to_owned(),
        )]);
        let diagnostics = diagnose_endpoint_environment(
            endpoint.kind(),
            endpoint.capabilities(),
            &environment,
        );
        assert!(diagnostics.is_empty(), "expected no diagnostics");
    }

    #[test]
    fn programmatic_detach_is_not_fabricated() {
        let mut endpoint = TmuxPtyEndpoint::new(
            EndpointId::new("tmux:work").unwrap(),
            TmuxPtySpec::new("work", size()).unwrap(),
        );
        assert_eq!(
            endpoint.detach().unwrap_err(),
            EndpointError::Unsupported("programmatic detach for tmux endpoints")
        );
    }

    #[test]
    fn environment_overrides_stay_on_the_direct_tmux_process() {
        let spec = TmuxPtySpec::new("work", size())
            .unwrap()
            .with_env("TERM", "xterm-256color");
        let env: Vec<_> = spec.process().env().collect();
        assert_eq!(
            env,
            vec![(OsStr::new("TERM"), OsStr::new("xterm-256color"))]
        );
    }
}
