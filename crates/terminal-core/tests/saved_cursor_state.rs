use terminal_core::{
    CellAttributes, Cursor, EndpointSize, TerminalColor, TerminalState,
};

#[test]
fn alternate_screen_decsc_cannot_corrupt_primary_1049_restore_state() {
    let mut terminal = TerminalState::new(EndpointSize::new(8, 6).unwrap(), 2);

    terminal.feed(
        b"\x1b[2;5r\x1b[?6h\x1b[3;4H\x1b[1;38;5;33m\x1b[?1049h\x1b[?6l\x1b[6;8H\x1b[0m\x1b7\x1b[?1049lX",
    );

    assert_eq!(terminal.cursor(), Cursor { row: 3, col: 4 });
    let restored = terminal.cell(3, 3).unwrap();
    assert_eq!(restored.character, 'X');
    assert!(restored.style.attributes.contains(CellAttributes::BOLD));
    assert_eq!(restored.style.foreground, TerminalColor::Indexed(33));

    terminal.feed(b"\x1b[2;5r\x1b[1;1H");
    assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });
}
