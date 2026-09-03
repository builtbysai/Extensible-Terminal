use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct EndpointId(String);

impl EndpointId {
    /// Creates a non-empty endpoint identity.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::InvalidIdentity`] when the supplied value is empty or whitespace.
    pub fn new(value: impl Into<String>) -> Result<Self, EndpointError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(EndpointError::InvalidIdentity);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointKind {
    LocalPty,
    SshPty,
    TmuxPane,
    Serial,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointLocality {
    Local,
    Remote { host: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EndpointSize {
    pub cols: u16,
    pub rows: u16,
}

impl EndpointSize {
    /// Creates a terminal size with non-zero dimensions.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::InvalidSize`] when either dimension is zero.
    pub fn new(cols: u16, rows: u16) -> Result<Self, EndpointError> {
        if cols == 0 || rows == 0 {
            return Err(EndpointError::InvalidSize { cols, rows });
        }
        Ok(Self { cols, rows })
    }
}

/// Independent transport capabilities. Keeping these as explicit flags is intentional:
/// endpoint providers may support arbitrary combinations and the UI must not infer parity.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EndpointCapabilities {
    pub detachable: bool,
    pub persistent: bool,
    pub remote: bool,
    pub authoritative_scrollback: bool,
    pub resize: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointEvent {
    Output(Vec<u8>),
    Bell,
    TitleChanged(String),
    Exited {
        code: Option<i32>,
        signal: Option<String>,
    },
    Disconnected { reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EndpointError {
    InvalidIdentity,
    InvalidSize { cols: u16, rows: u16 },
    Unsupported(&'static str),
    Transport(String),
}

impl fmt::Display for EndpointError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity => write!(f, "endpoint identity must not be empty"),
            Self::InvalidSize { cols, rows } => {
                write!(f, "endpoint size must be non-zero, got {cols}x{rows}")
            }
            Self::Unsupported(feature) => write!(f, "endpoint does not support {feature}"),
            Self::Transport(message) => write!(f, "endpoint transport error: {message}"),
        }
    }
}

impl std::error::Error for EndpointError {}

/// Transport/lifecycle contract implemented by endpoint providers.
///
/// The core deliberately does not promise persistence after detach. Providers
/// advertise that behavior via `capabilities()` rather than the UI fabricating
/// parity between local PTYs, SSH, tmux, or future transports.
pub trait Endpoint {
    fn id(&self) -> &EndpointId;
    fn kind(&self) -> EndpointKind;
    fn locality(&self) -> &EndpointLocality;
    fn capabilities(&self) -> EndpointCapabilities;

    /// Attaches the provider to its transport.
    ///
    /// # Errors
    ///
    /// Returns an endpoint-specific transport or lifecycle error when attachment fails.
    fn attach(&mut self) -> Result<(), EndpointError>;

    /// Detaches from the transport when the provider supports detachment.
    ///
    /// # Errors
    ///
    /// Returns [`EndpointError::Unsupported`] for providers that cannot detach, or a transport
    /// error when detachment fails.
    fn detach(&mut self) -> Result<(), EndpointError>;

    /// Resizes the underlying terminal transport.
    ///
    /// # Errors
    ///
    /// Returns an endpoint-specific error when resizing is unsupported or fails.
    fn resize(&mut self, size: EndpointSize) -> Result<(), EndpointError>;

    /// Sends raw input bytes to the endpoint.
    ///
    /// # Errors
    ///
    /// Returns an endpoint-specific error when the write fails.
    fn send(&mut self, input: &[u8]) -> Result<(), EndpointError>;

    /// Polls for one endpoint event without blocking the application event loop.
    ///
    /// # Errors
    ///
    /// Returns an endpoint-specific error when event polling cannot continue safely.
    fn poll_event(&mut self) -> Result<Option<EndpointEvent>, EndpointError>;
}
