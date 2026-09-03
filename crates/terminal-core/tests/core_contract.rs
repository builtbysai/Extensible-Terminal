use terminal_core::{
    BoundedEventQueue, EndpointCapabilities, EndpointError, EndpointEvent, EndpointId,
    EndpointSize, PushOutcome, Surface, SurfaceId,
};

#[test]
fn endpoint_identity_and_size_reject_ambiguous_values() {
    assert!(EndpointId::new(" ").is_err());
    assert_eq!(EndpointSize::new(0, 24), Err(EndpointError::InvalidSize { cols: 0, rows: 24 }));
    assert_eq!(EndpointSize::new(80, 24).unwrap().cols, 80);
}

#[test]
fn surface_can_rebind_without_claiming_endpoint_persistence() {
    let first = EndpointId::new("local:1").unwrap();
    let second = EndpointId::new("tmux:backend/2").unwrap();
    let mut surface = Surface::new(SurfaceId::new("main").unwrap(), first);

    surface.rebind(second.clone());
    assert_eq!(surface.endpoint_id(), &second);
}

#[test]
fn capabilities_expose_unequal_transport_semantics() {
    let local = EndpointCapabilities { resize: true, ..EndpointCapabilities::default() };
    let tmux = EndpointCapabilities {
        detachable: true,
        persistent: true,
        remote: true,
        authoritative_scrollback: false,
        resize: true,
    };

    assert!(!local.persistent);
    assert!(tmux.persistent);
    assert!(tmux.remote);
}

#[test]
fn output_is_not_dropped_when_observer_queue_is_full() {
    let mut queue = BoundedEventQueue::new(1, 16);
    assert_eq!(queue.push(EndpointEvent::Output(b"abc".to_vec())), PushOutcome::Accepted);
    assert_eq!(queue.push(EndpointEvent::Output(b"def".to_vec())), PushOutcome::Coalesced);
    assert_eq!(queue.pop(), Some(EndpointEvent::Output(b"abcdef".to_vec())));
}

#[test]
fn advisory_events_can_be_dropped_under_backpressure() {
    let mut queue = BoundedEventQueue::new(1, 16);
    queue.push(EndpointEvent::Output(b"abc".to_vec()));

    assert_eq!(queue.push(EndpointEvent::Bell), PushOutcome::DroppedAdvisory);
    assert_eq!(queue.stats().dropped_advisory, 1);
}

#[test]
fn oversized_output_reports_backpressure_without_unbounded_growth() {
    let mut queue = BoundedEventQueue::new(1, 4);
    queue.push(EndpointEvent::Output(b"abcd".to_vec()));

    assert_eq!(
        queue.push(EndpointEvent::Output(b"ef".to_vec())),
        PushOutcome::BackpressuredCritical
    );
    assert_eq!(queue.len(), 1);
    assert_eq!(queue.stats().critical_backpressure, 1);
    assert_eq!(queue.pop(), Some(EndpointEvent::Output(b"abcd".to_vec())));
}
