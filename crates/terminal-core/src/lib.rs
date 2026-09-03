#![forbid(unsafe_code)]

mod annotation;
mod command;
mod compatibility;
mod composition;
mod endpoint;
mod event_queue;
mod extension;
mod extension_host;
mod surface;
mod terminal;
mod terminal_state;
mod transition;

pub use annotation::{Annotation, AnnotationKind};
pub use command::{Command, CommandId, CommandTarget};
pub use compatibility::{
    CompatibilityCode, CompatibilityDiagnostic, CompatibilitySeverity, DEFAULT_TERM,
    TerminalEnvironment, diagnose_endpoint_environment, is_recognized_term,
};
pub use composition::{Composition, CompositionError, LayoutNode, SplitAxis, SplitRatio};
pub use endpoint::{
    Endpoint, EndpointCapabilities, EndpointError, EndpointEvent, EndpointId, EndpointKind,
    EndpointLocality, EndpointSize,
};
pub use event_queue::{BoundedEventQueue, PushOutcome, QueueStats};
pub use extension::{
    AuthorizedExtension, EventDelivery, ExtensionCapability, ExtensionError, ExtensionId,
    ExtensionInbox, ExtensionOrigin, ExtensionPolicy, ExtensionRequest, InboxOutcome,
};
pub use extension_host::{
    ExtensionHealth, ExtensionHost, ExtensionHostAction, ExtensionHostError, ExtensionHostPolicy,
    ExtensionHostState,
};
pub use surface::{Surface, SurfaceId, SurfaceState};
pub use terminal::{
    Cell, CellAttributes, CellStyle, Cursor, MAX_COMBINING_MARKS_PER_CELL, TerminalColor,
};
pub use terminal_state::{MouseTrackingMode, TerminalState};
pub use transition::{
    TransitionError, TransitionId, TransitionKind, TransitionOutcome, TransitionSample,
    TransitionStats, TransitionTracker,
};
