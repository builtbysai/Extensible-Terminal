use crate::{EndpointId, SurfaceId};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CommandId(String);

impl CommandId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        (!value.trim().is_empty()).then_some(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandTarget {
    Application,
    Endpoint(EndpointId),
    Surface(SurfaceId),
}

/// Explicit application action. Terminal byte streams are not commands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub id: CommandId,
    pub target: CommandTarget,
}
