use std::collections::BTreeSet;
use std::fmt;

use crate::{BoundedEventQueue, EndpointEvent, PushOutcome};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ExtensionId(String);

impl ExtensionId {
    /// Creates a non-empty extension identity.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError::InvalidIdentity`] for empty or whitespace-only values.
    pub fn new(value: impl Into<String>) -> Result<Self, ExtensionError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(ExtensionError::InvalidIdentity);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionOrigin {
    TrustedUserConfig,
    ThirdParty,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ExtensionCapability {
    ObserveOutput,
    ObserveLifecycle,
    ObservePresentation,
    ContributeCommands,
    ContributeAnnotations,
    ContributeUi,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionRequest {
    pub id: ExtensionId,
    pub origin: ExtensionOrigin,
    pub capabilities: BTreeSet<ExtensionCapability>,
}

impl ExtensionRequest {
    #[must_use]
    pub fn new(
        id: ExtensionId,
        origin: ExtensionOrigin,
        capabilities: impl IntoIterator<Item = ExtensionCapability>,
    ) -> Self {
        Self {
            id,
            origin,
            capabilities: capabilities.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtensionPolicy {
    trusted_user_config: BTreeSet<ExtensionCapability>,
    third_party: BTreeSet<ExtensionCapability>,
}

impl ExtensionPolicy {
    #[must_use]
    pub fn new(
        trusted_user_config: impl IntoIterator<Item = ExtensionCapability>,
        third_party: impl IntoIterator<Item = ExtensionCapability>,
    ) -> Self {
        Self {
            trusted_user_config: trusted_user_config.into_iter().collect(),
            third_party: third_party.into_iter().collect(),
        }
    }

    fn grants(&self, origin: ExtensionOrigin) -> &BTreeSet<ExtensionCapability> {
        match origin {
            ExtensionOrigin::TrustedUserConfig => &self.trusted_user_config,
            ExtensionOrigin::ThirdParty => &self.third_party,
        }
    }

    /// Validates a registration request against the authority granted to its origin.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionError::CapabilityDenied`] for the first requested capability
    /// that is not explicitly granted by policy.
    pub fn authorize(
        &self,
        request: ExtensionRequest,
    ) -> Result<AuthorizedExtension, ExtensionError> {
        let grants = self.grants(request.origin);
        if let Some(capability) = request
            .capabilities
            .iter()
            .find(|capability| !grants.contains(capability))
        {
            return Err(ExtensionError::CapabilityDenied(*capability));
        }

        Ok(AuthorizedExtension {
            id: request.id,
            origin: request.origin,
            capabilities: request.capabilities,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedExtension {
    id: ExtensionId,
    origin: ExtensionOrigin,
    capabilities: BTreeSet<ExtensionCapability>,
}

impl AuthorizedExtension {
    #[must_use]
    pub fn id(&self) -> &ExtensionId {
        &self.id
    }

    #[must_use]
    pub fn origin(&self) -> ExtensionOrigin {
        self.origin
    }

    #[must_use]
    pub fn has(&self, capability: ExtensionCapability) -> bool {
        self.capabilities.contains(&capability)
    }

    #[must_use]
    pub fn event_delivery(&self, event: &EndpointEvent) -> EventDelivery {
        let required = match event {
            EndpointEvent::Output(_) => ExtensionCapability::ObserveOutput,
            EndpointEvent::Exited { .. } | EndpointEvent::Disconnected { .. } => {
                ExtensionCapability::ObserveLifecycle
            }
            EndpointEvent::Bell | EndpointEvent::TitleChanged(_) => {
                ExtensionCapability::ObservePresentation
            }
        };

        if self.has(required) {
            EventDelivery::Allowed
        } else {
            EventDelivery::NotAuthorized
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventDelivery {
    Allowed,
    NotAuthorized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtensionError {
    InvalidIdentity,
    CapabilityDenied(ExtensionCapability),
}

impl fmt::Display for ExtensionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity => write!(f, "extension identity must not be empty"),
            Self::CapabilityDenied(capability) => {
                write!(f, "extension capability is not granted: {capability:?}")
            }
        }
    }
}

impl std::error::Error for ExtensionError {}

/// Capability-filtered, bounded event inbox owned by one extension.
pub struct ExtensionInbox {
    extension: AuthorizedExtension,
    queue: BoundedEventQueue,
}

impl ExtensionInbox {
    #[must_use]
    pub fn new(extension: AuthorizedExtension, capacity: usize, max_output_bytes: usize) -> Self {
        Self {
            extension,
            queue: BoundedEventQueue::new(capacity, max_output_bytes),
        }
    }

    #[must_use]
    pub fn extension(&self) -> &AuthorizedExtension {
        &self.extension
    }

    pub fn deliver(&mut self, event: EndpointEvent) -> InboxOutcome {
        if self.extension.event_delivery(&event) == EventDelivery::NotAuthorized {
            return InboxOutcome::NotAuthorized;
        }

        match self.queue.push(event) {
            PushOutcome::Accepted => InboxOutcome::Accepted,
            PushOutcome::Coalesced => InboxOutcome::Coalesced,
            PushOutcome::DroppedAdvisory => InboxOutcome::DroppedAdvisory,
            PushOutcome::BackpressuredCritical => InboxOutcome::BackpressuredCritical,
        }
    }

    pub fn pop(&mut self) -> Option<EndpointEvent> {
        self.queue.pop()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InboxOutcome {
    Accepted,
    Coalesced,
    DroppedAdvisory,
    BackpressuredCritical,
    NotAuthorized,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ExtensionId {
        ExtensionId::new("example.extension").expect("valid extension id")
    }

    #[test]
    fn third_party_capability_must_be_explicitly_granted() {
        let policy = ExtensionPolicy::new(
            [ExtensionCapability::ContributeCommands],
            [ExtensionCapability::ObservePresentation],
        );
        let request = ExtensionRequest::new(
            id(),
            ExtensionOrigin::ThirdParty,
            [ExtensionCapability::ObserveOutput],
        );

        assert_eq!(
            policy.authorize(request),
            Err(ExtensionError::CapabilityDenied(
                ExtensionCapability::ObserveOutput
            ))
        );
    }

    #[test]
    fn trusted_user_config_and_third_party_have_separate_grants() {
        let policy = ExtensionPolicy::new(
            [ExtensionCapability::ContributeUi],
            [ExtensionCapability::ObservePresentation],
        );
        let request = ExtensionRequest::new(
            id(),
            ExtensionOrigin::TrustedUserConfig,
            [ExtensionCapability::ContributeUi],
        );

        assert!(policy.authorize(request).is_ok());
    }

    #[test]
    fn inbox_filters_events_before_queueing() {
        let policy = ExtensionPolicy::new([], [ExtensionCapability::ObserveLifecycle]);
        let extension = policy
            .authorize(ExtensionRequest::new(
                id(),
                ExtensionOrigin::ThirdParty,
                [ExtensionCapability::ObserveLifecycle],
            ))
            .expect("capability granted");
        let mut inbox = ExtensionInbox::new(extension, 2, 16);

        assert_eq!(
            inbox.deliver(EndpointEvent::Output(vec![1, 2, 3])),
            InboxOutcome::NotAuthorized
        );
        assert!(inbox.pop().is_none());
        assert_eq!(
            inbox.deliver(EndpointEvent::Exited {
                code: Some(0),
                signal: None,
            }),
            InboxOutcome::Accepted
        );
    }

    #[test]
    fn slow_output_observer_gets_explicit_backpressure() {
        let policy = ExtensionPolicy::new([], [ExtensionCapability::ObserveOutput]);
        let extension = policy
            .authorize(ExtensionRequest::new(
                id(),
                ExtensionOrigin::ThirdParty,
                [ExtensionCapability::ObserveOutput],
            ))
            .expect("capability granted");
        let mut inbox = ExtensionInbox::new(extension, 1, 4);

        assert_eq!(
            inbox.deliver(EndpointEvent::Output(vec![1, 2, 3, 4])),
            InboxOutcome::Accepted
        );
        assert_eq!(
            inbox.deliver(EndpointEvent::Output(vec![5])),
            InboxOutcome::BackpressuredCritical
        );
    }
}
