use std::fmt;

#[cfg(test)]
use std::ffi::{OsStr, OsString};
use terminal_core::{
    Endpoint, EndpointCapabilities, EndpointError, EndpointEvent, EndpointId, EndpointKind,
    EndpointLocality, EndpointSize,
};

use crate::{LocalProcessSpec, LocalProcessSpecError, LocalPtyEndpoint, LocalPtyState};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SshPtySpecError {
    EmptyTarget,
    OptionLikeTarget,
    WhitespaceTarget,
    Process(LocalProcessSpecError),
}

impl fmt::Display for SshPtySpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyTarget => write!(f, "SSH target must not be empty"),
            Self::OptionLikeTarget => write!(f, "SSH target must not begin with '-'"),
            Self::WhitespaceTarget => write!(f, "SSH target must not contain whitespace"),
            Self::Process(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for SshPtySpecError {}

impl From<LocalProcessSpecError> for SshPtySpecError {
    fn from(error: LocalProcessSpecError) -> Self {
        Self::Process(error)
    }
}

/// Launch configuration for a system OpenSSH client attached to the native PTY.
///
/// The target is passed to `ssh` as one process argument. No shell command is
/// constructed, and OpenSSH remains authoritative for configuration,
/// authentication, host-key verification, proxying, algorithms, and network
/// lifecycle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SshPtySpec {
    target: String,
    process: LocalProcessSpec,
}

impl SshPtySpec {
    /// Builds a system-SSH launch specification that requests a remote TTY.
    ///
    /// # Errors
    ///
    /// Rejects empty, whitespace-containing, or option-like targets so a host
    /// value cannot be confused with an OpenSSH command-line option.
    pub fn new(
        target: impl Into<String>,
        initial_size: EndpointSize,
    ) -> Result<Self, SshPtySpecError> {
        let target = target.into();
        if target.trim().is_empty() {
            return Err(SshPtySpecError::EmptyTarget);
        }
        if target.starts_with('-') {
            return Err(SshPtySpecError::OptionLikeTarget);
        }
        if target.chars().any(char::is_whitespace) {
            return Err(SshPtySpecError::WhitespaceTarget);
        }

        let process = LocalProcessSpec::new("ssh", initial_size)?
            .arg("-t")
            .arg(target.clone());
        Ok(Self { target, process })
    }

    #[must_use]
    pub fn target(&self) -> &str {
        &self.target
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

/// A remote interactive terminal implemented by running the user's system SSH
/// client inside the existing native PTY transport.
///
/// This provider intentionally owns no SSH protocol, credentials, host-key
/// policy, prompt semantics, persistence, detach lifecycle, or remote scrollback.
pub struct SshPtyEndpoint {
    locality: EndpointLocality,
    inner: LocalPtyEndpoint,
}

impl SshPtyEndpoint {
    #[must_use]
    pub fn new(id: EndpointId, spec: SshPtySpec) -> Self {
        let locality = EndpointLocality::Remote {
            host: spec.target.clone(),
        };
        let inner = LocalPtyEndpoint::new(id, spec.process);
        Self { locality, inner }
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

impl Endpoint for SshPtyEndpoint {
    fn id(&self) -> &EndpointId {
        self.inner.id()
    }

    fn kind(&self) -> EndpointKind {
        EndpointKind::SshPty
    }

    fn locality(&self) -> &EndpointLocality {
        &self.locality
    }

    fn capabilities(&self) -> EndpointCapabilities {
        EndpointCapabilities {
            detachable: false,
            persistent: false,
            remote: true,
            authoritative_scrollback: false,
            resize: true,
        }
    }

    fn attach(&mut self) -> Result<(), EndpointError> {
        self.inner.attach()
    }

    fn detach(&mut self) -> Result<(), EndpointError> {
        Err(EndpointError::Unsupported("detach for SSH PTY endpoints"))
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
    fn ssh_process_is_direct_and_requests_a_tty() {
        let spec = SshPtySpec::new("alice@example.test", size()).unwrap();
        assert_eq!(spec.process().program(), OsStr::new("ssh"));
        assert_eq!(
            spec.process().args(),
            [OsString::from("-t"), OsString::from("alice@example.test")]
        );
    }

    #[test]
    fn rejects_empty_whitespace_and_option_like_targets() {
        assert_eq!(
            SshPtySpec::new("", size()).unwrap_err(),
            SshPtySpecError::EmptyTarget
        );
        assert_eq!(
            SshPtySpec::new("   ", size()).unwrap_err(),
            SshPtySpecError::EmptyTarget
        );
        assert_eq!(
            SshPtySpec::new("-oProxyCommand=bad", size()).unwrap_err(),
            SshPtySpecError::OptionLikeTarget
        );
        assert_eq!(
            SshPtySpec::new("host name", size()).unwrap_err(),
            SshPtySpecError::WhitespaceTarget
        );
    }

    #[test]
    fn reports_only_proven_remote_capabilities() {
        let endpoint = SshPtyEndpoint::new(
            EndpointId::new("ssh:test").unwrap(),
            SshPtySpec::new("example.test", size()).unwrap(),
        );
        assert_eq!(endpoint.kind(), EndpointKind::SshPty);
        assert_eq!(
            endpoint.locality(),
            &EndpointLocality::Remote {
                host: "example.test".to_owned()
            }
        );
        assert_eq!(
            endpoint.capabilities(),
            EndpointCapabilities {
                detachable: false,
                persistent: false,
                remote: true,
                authoritative_scrollback: false,
                resize: true,
            }
        );
    }

    #[test]
    fn environment_overrides_stay_on_the_direct_ssh_process() {
        let spec = SshPtySpec::new("example.test", size())
            .unwrap()
            .with_env("TERM", "xterm-256color");
        let env: Vec<_> = spec.process().env().collect();
        assert_eq!(
            env,
            vec![(OsStr::new("TERM"), OsStr::new("xterm-256color"))]
        );
    }
}
