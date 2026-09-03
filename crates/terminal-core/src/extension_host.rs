use std::collections::{BTreeMap, btree_map::Entry};
use std::fmt;

use crate::{ExtensionId, InboxOutcome};

/// Host-owned thresholds for isolating extensions that cannot drain bounded inboxes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionHostPolicy {
    suspend_after_backpressure: u32,
    disconnect_after_backpressure: u32,
}

impl ExtensionHostPolicy {
    /// Creates a lifecycle policy.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionHostError::InvalidPolicy`] when suspension is disabled or the
    /// disconnect threshold is lower than the suspension threshold.
    pub fn new(
        suspend_after_backpressure: u32,
        disconnect_after_backpressure: u32,
    ) -> Result<Self, ExtensionHostError> {
        if suspend_after_backpressure == 0
            || disconnect_after_backpressure < suspend_after_backpressure
        {
            return Err(ExtensionHostError::InvalidPolicy);
        }
        Ok(Self {
            suspend_after_backpressure,
            disconnect_after_backpressure,
        })
    }
}

/// Host-visible lifecycle state for an extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionHostState {
    Active,
    Suspended,
    Disconnected,
}

/// Policy action emitted after observing extension inbox pressure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionHostAction {
    None,
    Suspend,
    Disconnect,
}

/// Read-only health snapshot for one registered extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionHealth {
    state: ExtensionHostState,
    consecutive_backpressure: u32,
}

impl ExtensionHealth {
    /// Returns the current lifecycle state.
    #[must_use]
    pub fn state(self) -> ExtensionHostState {
        self.state
    }

    /// Returns the current consecutive critical-backpressure count.
    #[must_use]
    pub fn consecutive_backpressure(self) -> u32 {
        self.consecutive_backpressure
    }
}

/// Minimal host-side lifecycle tracker; it owns policy but no plugin runtime or transport.
#[derive(Debug)]
pub struct ExtensionHost {
    policy: ExtensionHostPolicy,
    health: BTreeMap<ExtensionId, ExtensionHealth>,
}

impl ExtensionHost {
    /// Creates an empty lifecycle tracker.
    #[must_use]
    pub fn new(policy: ExtensionHostPolicy) -> Self {
        Self {
            policy,
            health: BTreeMap::new(),
        }
    }

    /// Registers one authorized extension identity with active health.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionHostError::AlreadyRegistered`] when the identity already exists.
    pub fn register(&mut self, id: ExtensionId) -> Result<(), ExtensionHostError> {
        match self.health.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(ExtensionHealth {
                    state: ExtensionHostState::Active,
                    consecutive_backpressure: 0,
                });
                Ok(())
            }
            Entry::Occupied(_) => Err(ExtensionHostError::AlreadyRegistered),
        }
    }

    /// Returns the current health snapshot, if registered.
    #[must_use]
    pub fn health(&self, id: &ExtensionId) -> Option<ExtensionHealth> {
        self.health.get(id).copied()
    }

    /// Records the result of a capability-filtered inbox delivery and emits a host action.
    ///
    /// Advisory drops and authorization denials do not count against extension health.
    /// Successful critical delivery resets the consecutive-pressure counter.
    ///
    /// # Errors
    ///
    /// Returns [`ExtensionHostError::NotRegistered`] for an unknown identity.
    pub fn observe_delivery(
        &mut self,
        id: &ExtensionId,
        outcome: InboxOutcome,
    ) -> Result<ExtensionHostAction, ExtensionHostError> {
        let health = self
            .health
            .get_mut(id)
            .ok_or(ExtensionHostError::NotRegistered)?;

        if health.state == ExtensionHostState::Disconnected {
            return Ok(ExtensionHostAction::None);
        }

        match outcome {
            InboxOutcome::BackpressuredCritical => {
                health.consecutive_backpressure =
                    health.consecutive_backpressure.saturating_add(1);
            }
            InboxOutcome::Accepted | InboxOutcome::Coalesced => {
                health.consecutive_backpressure = 0;
                if health.state == ExtensionHostState::Suspended {
                    health.state = ExtensionHostState::Active;
                }
            }
            InboxOutcome::DroppedAdvisory | InboxOutcome::NotAuthorized => {}
        }

        if health.consecutive_backpressure >= self.policy.disconnect_after_backpressure {
            health.state = ExtensionHostState::Disconnected;
            return Ok(ExtensionHostAction::Disconnect);
        }
        if health.consecutive_backpressure >= self.policy.suspend_after_backpressure
            && health.state == ExtensionHostState::Active
        {
            health.state = ExtensionHostState::Suspended;
            return Ok(ExtensionHostAction::Suspend);
        }

        Ok(ExtensionHostAction::None)
    }
}

/// Lifecycle-policy errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionHostError {
    InvalidPolicy,
    AlreadyRegistered,
    NotRegistered,
}

impl fmt::Display for ExtensionHostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPolicy => write!(f, "extension host policy thresholds are invalid"),
            Self::AlreadyRegistered => write!(f, "extension is already registered"),
            Self::NotRegistered => write!(f, "extension is not registered"),
        }
    }
}

impl std::error::Error for ExtensionHostError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> ExtensionId {
        ExtensionId::new("example.extension").expect("valid extension id")
    }

    #[test]
    fn invalid_policy_is_rejected() {
        assert_eq!(
            ExtensionHostPolicy::new(0, 1),
            Err(ExtensionHostError::InvalidPolicy)
        );
        assert_eq!(
            ExtensionHostPolicy::new(3, 2),
            Err(ExtensionHostError::InvalidPolicy)
        );
    }

    #[test]
    fn repeated_critical_backpressure_suspends_then_disconnects() {
        let policy = ExtensionHostPolicy::new(2, 4).expect("valid policy");
        let mut host = ExtensionHost::new(policy);
        let extension = id();
        host.register(extension.clone()).expect("register");

        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::BackpressuredCritical),
            Ok(ExtensionHostAction::None)
        );
        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::BackpressuredCritical),
            Ok(ExtensionHostAction::Suspend)
        );
        assert_eq!(
            host.health(&extension).expect("health").state(),
            ExtensionHostState::Suspended
        );
        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::BackpressuredCritical),
            Ok(ExtensionHostAction::None)
        );
        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::BackpressuredCritical),
            Ok(ExtensionHostAction::Disconnect)
        );
    }

    #[test]
    fn successful_delivery_recovers_suspended_extension() {
        let policy = ExtensionHostPolicy::new(1, 3).expect("valid policy");
        let mut host = ExtensionHost::new(policy);
        let extension = id();
        host.register(extension.clone()).expect("register");

        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::BackpressuredCritical),
            Ok(ExtensionHostAction::Suspend)
        );
        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::Accepted),
            Ok(ExtensionHostAction::None)
        );
        let health = host.health(&extension).expect("health");
        assert_eq!(health.state(), ExtensionHostState::Active);
        assert_eq!(health.consecutive_backpressure(), 0);
    }

    #[test]
    fn advisory_drop_does_not_punish_extension() {
        let policy = ExtensionHostPolicy::new(1, 2).expect("valid policy");
        let mut host = ExtensionHost::new(policy);
        let extension = id();
        host.register(extension.clone()).expect("register");

        assert_eq!(
            host.observe_delivery(&extension, InboxOutcome::DroppedAdvisory),
            Ok(ExtensionHostAction::None)
        );
        assert_eq!(
            host.health(&extension).expect("health").consecutive_backpressure(),
            0
        );
    }

    #[test]
    fn unknown_extension_is_rejected() {
        let policy = ExtensionHostPolicy::new(1, 2).expect("valid policy");
        let mut host = ExtensionHost::new(policy);
        assert_eq!(
            host.observe_delivery(&id(), InboxOutcome::Accepted),
            Err(ExtensionHostError::NotRegistered)
        );
    }
}
