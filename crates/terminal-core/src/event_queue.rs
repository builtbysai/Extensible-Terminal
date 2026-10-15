use std::collections::VecDeque;

use crate::EndpointEvent;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct QueueStats {
    pub accepted: u64,
    pub coalesced: u64,
    pub dropped_advisory: u64,
    pub critical_backpressure: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PushOutcome {
    Accepted,
    Coalesced,
    DroppedAdvisory,
    BackpressuredCritical,
}

/// Bounded delivery queue for plugin/observer-facing endpoint events.
///
/// Output bytes are correctness-critical and are never silently dropped. Both
/// event count and total queued output bytes are bounded. Adjacent output may be
/// coalesced while it remains inside the byte budget; otherwise callers receive
/// explicit critical backpressure. Advisory events may be discarded when the
/// event-count bound is full.
pub struct BoundedEventQueue {
    events: VecDeque<EndpointEvent>,
    capacity: usize,
    max_output_bytes: usize,
    queued_output_bytes: usize,
    stats: QueueStats,
}

impl BoundedEventQueue {
    /// Creates a bounded observer queue.
    ///
    /// # Panics
    ///
    /// Panics when either `capacity` or `max_output_bytes` is zero.
    #[must_use]
    pub fn new(capacity: usize, max_output_bytes: usize) -> Self {
        assert!(capacity > 0, "capacity must be non-zero");
        assert!(max_output_bytes > 0, "max_output_bytes must be non-zero");
        Self {
            events: VecDeque::with_capacity(capacity),
            capacity,
            max_output_bytes,
            queued_output_bytes: 0,
            stats: QueueStats::default(),
        }
    }

    pub fn push(&mut self, event: EndpointEvent) -> PushOutcome {
        if let EndpointEvent::Output(mut incoming) = event {
            let incoming_len = incoming.len();
            if incoming_len > self.max_output_bytes
                || self.queued_output_bytes.saturating_add(incoming_len) > self.max_output_bytes
            {
                self.stats.critical_backpressure += 1;
                return PushOutcome::BackpressuredCritical;
            }

            if self.events.len() < self.capacity {
                self.queued_output_bytes += incoming_len;
                self.events.push_back(EndpointEvent::Output(incoming));
                self.stats.accepted += 1;
                return PushOutcome::Accepted;
            }

            if let Some(EndpointEvent::Output(existing)) = self.events.back_mut() {
                existing.append(&mut incoming);
                self.queued_output_bytes += incoming_len;
                self.stats.coalesced += 1;
                return PushOutcome::Coalesced;
            }

            self.stats.critical_backpressure += 1;
            return PushOutcome::BackpressuredCritical;
        }

        if self.events.len() < self.capacity {
            self.events.push_back(event);
            self.stats.accepted += 1;
            return PushOutcome::Accepted;
        }

        self.stats.dropped_advisory += 1;
        PushOutcome::DroppedAdvisory
    }

    pub fn pop(&mut self) -> Option<EndpointEvent> {
        let event = self.events.pop_front()?;
        if let EndpointEvent::Output(bytes) = &event {
            self.queued_output_bytes = self.queued_output_bytes.saturating_sub(bytes.len());
        }
        Some(event)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    #[must_use]
    pub fn queued_output_bytes(&self) -> usize {
        self.queued_output_bytes
    }

    #[must_use]
    pub fn stats(&self) -> QueueStats {
        self.stats
    }
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
mod tests {
    use super::*;

    #[test]
    fn critical_output_never_grows_past_capacity() {
        let mut queue = BoundedEventQueue::new(1, 4);
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![1, 2, 3, 4])),
            PushOutcome::Accepted
        );
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![5])),
            PushOutcome::BackpressuredCritical
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.queued_output_bytes(), 4);
        assert_eq!(queue.stats().critical_backpressure, 1);
    }

    #[test]
    fn adjacent_output_coalesces_within_byte_limit() {
        let mut queue = BoundedEventQueue::new(1, 4);
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![1, 2])),
            PushOutcome::Accepted
        );
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![3, 4])),
            PushOutcome::Coalesced
        );
        assert_eq!(queue.queued_output_bytes(), 4);
        assert_eq!(
            queue.pop(),
            Some(EndpointEvent::Output(vec![1, 2, 3, 4]))
        );
        assert_eq!(queue.queued_output_bytes(), 0);
    }

    #[test]
    fn oversized_first_output_is_backpressured() {
        let mut queue = BoundedEventQueue::new(8, 4);
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![1, 2, 3, 4, 5])),
            PushOutcome::BackpressuredCritical
        );
        assert!(queue.is_empty());
        assert_eq!(queue.queued_output_bytes(), 0);
    }

    #[test]
    fn aggregate_output_budget_is_enforced_before_event_capacity() {
        let mut queue = BoundedEventQueue::new(4, 4);
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![1, 2, 3])),
            PushOutcome::Accepted
        );
        assert_eq!(
            queue.push(EndpointEvent::Output(vec![4, 5])),
            PushOutcome::BackpressuredCritical
        );
        assert_eq!(queue.queued_output_bytes(), 3);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn advisory_event_is_explicitly_dropped_under_pressure() {
        let mut queue = BoundedEventQueue::new(1, 8);
        assert_eq!(queue.push(EndpointEvent::Bell), PushOutcome::Accepted);
        assert_eq!(
            queue.push(EndpointEvent::TitleChanged("busy".into())),
            PushOutcome::DroppedAdvisory
        );
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.stats().dropped_advisory, 1);
    }
}
