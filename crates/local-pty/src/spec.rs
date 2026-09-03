use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};

use terminal_core::EndpointSize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalProcessSpecError {
    EmptyProgram,
}

impl fmt::Display for LocalProcessSpecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProgram => write!(f, "local process program must not be empty"),
        }
    }
}

impl std::error::Error for LocalProcessSpecError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalProcessSpec {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: Vec<(OsString, OsString)>,
    initial_size: EndpointSize,
}

impl LocalProcessSpec {
    /// Creates a launch specification for one program attached to a PTY.
    ///
    /// # Errors
    ///
    /// Returns [`LocalProcessSpecError::EmptyProgram`] when the executable name is empty.
    pub fn new(
        program: impl Into<OsString>,
        initial_size: EndpointSize,
    ) -> Result<Self, LocalProcessSpecError> {
        let program = program.into();
        if program.is_empty() {
            return Err(LocalProcessSpecError::EmptyProgram);
        }
        Ok(Self {
            program,
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            initial_size,
        })
    }

    #[must_use]
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn with_env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn env(&self) -> impl Iterator<Item = (&OsStr, &OsStr)> {
        self.env
            .iter()
            .map(|(name, value)| (name.as_os_str(), value.as_os_str()))
    }

    #[must_use]
    pub fn initial_size(&self) -> EndpointSize {
        self.initial_size
    }
}
