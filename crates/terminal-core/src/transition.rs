use std::collections::HashMap;
use std::time::Duration;

use crate::{EndpointId, SurfaceId};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TransitionId(u64);

impl TransitionId {
    #[must_use]
    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    FocusSurface {
        from: Option<SurfaceId>,
        to: SurfaceId,
    },
    RebindSurface {
        surface: SurfaceId,
        from: EndpointId,
        to: EndpointId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransitionSample {
    pub id: TransitionId,
    pub kind: TransitionKind,
    pub outcome: TransitionOutcome,
    pub latency: Duration,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransitionStats {
    pub completed: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub total_latency: Duration,
    pub max_latency: Duration,
}

impl TransitionStats {
    #[must_use]
    pub fn samples(self) -> u64 {
        self.completed + self.cancelled + self.failed
    }

    #[must_use]
    pub fn average_latency(self) -> Option<Duration> {
        let samples = self.samples();
        u32::try_from(samples)
            .ok()
            .and_then(|samples| self.total_latency.checked_div(samples))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransitionError {
    NoOpFocus,
    NoOpRebind,
    UnknownTransition(TransitionId),
    NonMonotonicTimestamp,
    IdExhausted,
}

#[derive(Debug)]
struct ActiveTransition {
    kind: TransitionKind,
    started_at: Duration,
}

#[derive(Debug, Default)]
pub struct TransitionTracker {
    next_id: u64,
    active: HashMap<TransitionId, ActiveTransition>,
    stats: TransitionStats,
}

impl TransitionTracker {
    #[must_use]
    pub fn stats(&self) -> TransitionStats {
        self.stats
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Starts a caller-observed focus or endpoint-rebind transition.
    ///
    /// Timestamps are supplied by the caller so the core does not own a clock or UI policy.
    ///
    /// # Errors
    ///
    /// Returns an error for no-op transitions or when transition identifiers are exhausted.
    pub fn begin(
        &mut self,
        kind: TransitionKind,
        started_at: Duration,
    ) -> Result<TransitionId, TransitionError> {
        validate_kind(&kind)?;
        let id = TransitionId(self.next_id);
        self.next_id = self.next_id.checked_add(1).ok_or(TransitionError::IdExhausted)?;
        self.active.insert(id, ActiveTransition { kind, started_at });
        Ok(id)
    }

    /// Finishes a transition and records its latency and outcome.
    ///
    /// # Errors
    ///
    /// Returns an error when the transition is unknown or when the supplied timestamp
    /// predates its start. A non-monotonic timestamp leaves the transition active so the
    /// caller can retry with a valid observation.
    pub fn finish(
        &mut self,
        id: TransitionId,
        finished_at: Duration,
        outcome: TransitionOutcome,
    ) -> Result<TransitionSample, TransitionError> {
        let Some(active) = self.active.get(&id) else {
            return Err(TransitionError::UnknownTransition(id));
        };
        let Some(latency) = finished_at.checked_sub(active.started_at) else {
            return Err(TransitionError::NonMonotonicTimestamp);
        };

        let active = self
            .active
            .remove(&id)
            .ok_or(TransitionError::UnknownTransition(id))?;
        match outcome {
            TransitionOutcome::Completed => self.stats.completed += 1,
            TransitionOutcome::Cancelled => self.stats.cancelled += 1,
            TransitionOutcome::Failed => self.stats.failed += 1,
        }
        self.stats.total_latency = self.stats.total_latency.saturating_add(latency);
        self.stats.max_latency = self.stats.max_latency.max(latency);

        Ok(TransitionSample {
            id,
            kind: active.kind,
            outcome,
            latency,
        })
    }
}

fn validate_kind(kind: &TransitionKind) -> Result<(), TransitionError> {
    match kind {
        TransitionKind::FocusSurface { from: Some(from), to } if from == to => {
            Err(TransitionError::NoOpFocus)
        }
        TransitionKind::RebindSurface { from, to, .. } if from == to => {
            Err(TransitionError::NoOpRebind)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface(value: &str) -> SurfaceId {
        SurfaceId::new(value).expect("surface id")
    }

    fn endpoint(value: &str) -> EndpointId {
        EndpointId::new(value).expect("endpoint id")
    }

    #[test]
    fn records_completed_transition_latency() {
        let mut tracker = TransitionTracker::default();
        let id = tracker
            .begin(
                TransitionKind::FocusSurface {
                    from: Some(surface("left")),
                    to: surface("right"),
                },
                Duration::from_millis(100),
            )
            .expect("begin");
        let sample = tracker
            .finish(id, Duration::from_millis(107), TransitionOutcome::Completed)
            .expect("finish");

        assert_eq!(sample.latency, Duration::from_millis(7));
        assert_eq!(tracker.stats().completed, 1);
        assert_eq!(tracker.stats().average_latency(), Some(Duration::from_millis(7)));
        assert_eq!(tracker.active_count(), 0);
    }

    #[test]
    fn rejected_timestamp_keeps_transition_active() {
        let mut tracker = TransitionTracker::default();
        let id = tracker
            .begin(
                TransitionKind::RebindSurface {
                    surface: surface("main"),
                    from: endpoint("local"),
                    to: endpoint("ssh"),
                },
                Duration::from_millis(20),
            )
            .expect("begin");

        assert_eq!(
            tracker.finish(id, Duration::from_millis(19), TransitionOutcome::Failed),
            Err(TransitionError::NonMonotonicTimestamp)
        );
        assert_eq!(tracker.active_count(), 1);
    }

    #[test]
    fn rejects_no_op_focus_and_rebind() {
        let mut tracker = TransitionTracker::default();
        let focus = surface("main");
        let local = endpoint("local");

        assert_eq!(
            tracker.begin(
                TransitionKind::FocusSurface {
                    from: Some(focus.clone()),
                    to: focus,
                },
                Duration::ZERO,
            ),
            Err(TransitionError::NoOpFocus)
        );
        assert_eq!(
            tracker.begin(
                TransitionKind::RebindSurface {
                    surface: surface("main"),
                    from: local.clone(),
                    to: local,
                },
                Duration::ZERO,
            ),
            Err(TransitionError::NoOpRebind)
        );
    }

    #[test]
    fn aggregates_mixed_outcomes_without_policy_thresholds() {
        let mut tracker = TransitionTracker::default();
        for (index, outcome) in [
            TransitionOutcome::Completed,
            TransitionOutcome::Cancelled,
            TransitionOutcome::Failed,
        ]
        .into_iter()
        .enumerate()
        {
            let id = tracker
                .begin(
                    TransitionKind::FocusSurface {
                        from: None,
                        to: surface(&format!("surface-{index}")),
                    },
                    Duration::from_millis(10),
                )
                .expect("begin");
            tracker
                .finish(id, Duration::from_millis(15 + index as u64), outcome)
                .expect("finish");
        }

        let stats = tracker.stats();
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.cancelled, 1);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.max_latency, Duration::from_millis(7));
        assert_eq!(stats.total_latency, Duration::from_millis(18));
    }
}
