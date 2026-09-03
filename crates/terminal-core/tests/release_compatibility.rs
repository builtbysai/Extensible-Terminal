use std::mem::size_of;

use terminal_core::{
    Cell, EndpointEvent, EndpointSize, ExtensionCapability, ExtensionHost, ExtensionHostAction,
    ExtensionHostPolicy, ExtensionHostState, ExtensionId, ExtensionInbox, ExtensionOrigin,
    ExtensionPolicy, ExtensionRequest, InboxOutcome, TerminalColor, TerminalState,
};

fn size(cols: u16, rows: u16) -> EndpointSize {
    EndpointSize::new(cols, rows).expect("test geometry is valid")
}

#[test]
fn representative_full_screen_trace_keeps_modes_buffers_and_styles_coherent() {
    let mut state = TerminalState::new(size(12, 6), 8);
    state.feed(b"PRIMARY");

    state.feed(b"\x1b[2;5r\x1b[?6h\x1b[?7h");
    state.feed(b"\x1b[?1049h\x1b[2;5r\x1b[?6h\x1b[?1h\x1b[?2004h\x1b=");
    assert!(state.alternate_screen_active());
    assert!(state.application_cursor_keys());
    assert!(state.application_keypad_mode());
    assert!(state.bracketed_paste_mode());

    state.feed(b"\x1b[H\x1b[44mALT\x1b[2K");
    assert_eq!(
        state.cell(1, 0).expect("alternate cell").style.background,
        TerminalColor::Indexed(4)
    );

    state.feed(b"\x1b[?1049l");
    assert!(!state.alternate_screen_active());
    assert!(state.line_text(0).expect("primary row").starts_with("PRIMARY"));

    state.feed(b"\x1b[?6l\x1b[2;3H\x1b7\x1b[5;8H\x1b8");
    assert_eq!(state.cursor().row, 1);
    assert_eq!(state.cursor().col, 2);

    state.feed(b"\x1b[1;1H\x1b[41m\x1b[2K");
    for col in 0..state.size().cols {
        let cell = state.cell(0, col).expect("row cell");
        assert_eq!(cell.display_character(), ' ');
        assert_eq!(cell.style.background, TerminalColor::Indexed(1));
    }

    state.feed(b"\x1b[?7l\x1b[1;12HXY");
    assert_eq!(state.cell(0, 11).expect("right margin").display_character(), 'Y');
    state.feed(b"\x1b[?7h\x1b[?1l\x1b[?2004l\x1b>");
    assert!(!state.application_cursor_keys());
    assert!(!state.application_keypad_mode());
    assert!(!state.bracketed_paste_mode());

    state.resize(size(14, 7));
    assert_eq!(state.size(), size(14, 7));
    assert!(state.scrollback_len() <= 8);
}

#[test]
fn repeated_alternate_screen_switches_preserve_primary_and_stay_bounded() {
    let mut state = TerminalState::new(size(20, 5), 4);
    state.feed(b"stable-primary");

    for _ in 0..256 {
        state.feed(b"\x1b[?1049h\x1b[H\xe7\x95\x8c\x1b[2J\x1b[?1049l");
        assert!(!state.alternate_screen_active());
        assert!(
            state
                .line_text(0)
                .expect("primary row survives")
                .starts_with("stable-primary")
        );
        assert!(state.scrollback_len() <= 4);
    }
}

#[test]
fn high_rate_output_keeps_scrollback_and_cell_storage_bounded() {
    const SCROLLBACK_LIMIT: usize = 512;
    let geometry = size(80, 24);
    let mut state = TerminalState::new(geometry, SCROLLBACK_LIMIT);
    let mut burst = Vec::with_capacity(64 * 1024);

    for _ in 0..4_000 {
        burst.extend_from_slice(b"release-load\r\n");
    }
    state.feed(&burst);

    assert_eq!(state.scrollback_len(), SCROLLBACK_LIMIT);
    let screen_rows = usize::from(geometry.rows) * 2;
    let retained_cell_bound = size_of::<Cell>()
        * usize::from(geometry.cols)
        * (screen_rows + SCROLLBACK_LIMIT);
    assert!(retained_cell_bound < 16 * 1024 * 1024);
}

#[test]
fn slow_extension_backpressure_isolated_from_terminal_progress() {
    let extension_id = ExtensionId::new("release.slow-observer").expect("valid extension id");
    let extension = ExtensionPolicy::new([], [ExtensionCapability::ObserveOutput])
        .authorize(ExtensionRequest::new(
            extension_id.clone(),
            ExtensionOrigin::ThirdParty,
            [ExtensionCapability::ObserveOutput],
        ))
        .expect("output observation is granted");
    let mut inbox = ExtensionInbox::new(extension, 1, 8);
    let mut host = ExtensionHost::new(ExtensionHostPolicy::new(2, 3).expect("valid policy"));
    host.register(extension_id.clone()).expect("register extension");
    let mut terminal = TerminalState::new(size(32, 4), 4);

    let accepted = inbox.deliver(EndpointEvent::Output(b"12345678".to_vec()));
    assert_eq!(accepted, InboxOutcome::Accepted);
    assert_eq!(
        host.observe_delivery(&extension_id, accepted),
        Ok(ExtensionHostAction::None)
    );

    let mut actions = Vec::new();
    for _ in 0..3 {
        terminal.feed(b"tick");
        let outcome = inbox.deliver(EndpointEvent::Output(vec![b'x']));
        assert_eq!(outcome, InboxOutcome::BackpressuredCritical);
        actions.push(
            host.observe_delivery(&extension_id, outcome)
                .expect("registered extension"),
        );
    }

    assert_eq!(
        actions,
        vec![
            ExtensionHostAction::None,
            ExtensionHostAction::Suspend,
            ExtensionHostAction::Disconnect,
        ]
    );
    assert_eq!(
        host.health(&extension_id).expect("health").state(),
        ExtensionHostState::Disconnected
    );
    assert!(terminal.line_text(0).expect("terminal row").starts_with("tickticktick"));
    assert_eq!(
        inbox.pop(),
        Some(EndpointEvent::Output(b"12345678".to_vec()))
    );
    assert!(inbox.pop().is_none());
}
