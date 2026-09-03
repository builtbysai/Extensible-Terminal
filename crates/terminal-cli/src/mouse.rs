use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use terminal_core::MouseTrackingMode;

const BUTTON_MOTION: u16 = 32;
const WHEEL_BASE: u16 = 64;
const SHIFT_MODIFIER: u16 = 4;
const ALT_MODIFIER: u16 = 8;
const CONTROL_MODIFIER: u16 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EncodedMouse {
    code: u16,
    release: bool,
}

pub(crate) fn capture_transition(current: bool, requested: bool) -> Option<bool> {
    (current != requested).then_some(requested)
}

pub(crate) fn mouse_bytes(
    event: MouseEvent,
    tracking: MouseTrackingMode,
    sgr_encoding: bool,
) -> Option<Vec<u8>> {
    if tracking == MouseTrackingMode::Off || !sgr_encoding {
        return None;
    }

    let encoded = event_code(event.kind, tracking)?;
    let code = encoded.code.saturating_add(modifier_code(event.modifiers));
    let column = u32::from(event.column) + 1;
    let row = u32::from(event.row) + 1;
    let suffix = if encoded.release { 'm' } else { 'M' };
    Some(format!("\x1b[<{code};{column};{row}{suffix}").into_bytes())
}

fn event_code(kind: MouseEventKind, tracking: MouseTrackingMode) -> Option<EncodedMouse> {
    match kind {
        MouseEventKind::Down(button) => Some(EncodedMouse {
            code: button_code(button),
            release: false,
        }),
        MouseEventKind::Up(button) => Some(EncodedMouse {
            code: button_code(button),
            release: true,
        }),
        MouseEventKind::Drag(button)
            if matches!(
                tracking,
                MouseTrackingMode::ButtonEvent | MouseTrackingMode::AnyEvent
            ) =>
        {
            Some(EncodedMouse {
                code: button_code(button).saturating_add(BUTTON_MOTION),
                release: false,
            })
        }
        MouseEventKind::Moved if tracking == MouseTrackingMode::AnyEvent => Some(EncodedMouse {
            code: 3 + BUTTON_MOTION,
            release: false,
        }),
        MouseEventKind::ScrollUp => Some(EncodedMouse {
            code: WHEEL_BASE,
            release: false,
        }),
        MouseEventKind::ScrollDown => Some(EncodedMouse {
            code: WHEEL_BASE + 1,
            release: false,
        }),
        MouseEventKind::ScrollLeft => Some(EncodedMouse {
            code: WHEEL_BASE + 2,
            release: false,
        }),
        MouseEventKind::ScrollRight => Some(EncodedMouse {
            code: WHEEL_BASE + 3,
            release: false,
        }),
        _ => None,
    }
}

fn button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn modifier_code(modifiers: KeyModifiers) -> u16 {
    let mut code = 0;
    if modifiers.contains(KeyModifiers::SHIFT) {
        code += SHIFT_MODIFIER;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        code += ALT_MODIFIER;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        code += CONTROL_MODIFIER;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(kind: MouseEventKind, column: u16, row: u16, modifiers: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers,
        }
    }

    #[test]
    fn capture_changes_only_at_negotiation_boundaries() {
        assert_eq!(capture_transition(false, false), None);
        assert_eq!(capture_transition(false, true), Some(true));
        assert_eq!(capture_transition(true, true), None);
        assert_eq!(capture_transition(true, false), Some(false));
    }

    #[test]
    fn press_release_mode_filters_motion_and_uses_one_based_coordinates() {
        assert_eq!(
            mouse_bytes(
                mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, KeyModifiers::NONE),
                MouseTrackingMode::PressRelease,
                true,
            )
            .unwrap(),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            mouse_bytes(
                mouse(MouseEventKind::Up(MouseButton::Left), 4, 2, KeyModifiers::NONE),
                MouseTrackingMode::PressRelease,
                true,
            )
            .unwrap(),
            b"\x1b[<0;5;3m"
        );
        assert!(
            mouse_bytes(
                mouse(MouseEventKind::Drag(MouseButton::Left), 1, 1, KeyModifiers::NONE),
                MouseTrackingMode::PressRelease,
                true,
            )
            .is_none()
        );
        assert!(
            mouse_bytes(
                mouse(MouseEventKind::Moved, 1, 1, KeyModifiers::NONE),
                MouseTrackingMode::PressRelease,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn button_event_mode_reports_drag_but_not_unpressed_motion() {
        assert_eq!(
            mouse_bytes(
                mouse(MouseEventKind::Drag(MouseButton::Right), 7, 5, KeyModifiers::NONE),
                MouseTrackingMode::ButtonEvent,
                true,
            )
            .unwrap(),
            b"\x1b[<34;8;6M"
        );
        assert!(
            mouse_bytes(
                mouse(MouseEventKind::Moved, 7, 5, KeyModifiers::NONE),
                MouseTrackingMode::ButtonEvent,
                true,
            )
            .is_none()
        );
    }

    #[test]
    fn any_event_mode_reports_motion_with_modifiers() {
        let modifiers = KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL;
        assert_eq!(
            mouse_bytes(
                mouse(MouseEventKind::Moved, 2, 3, modifiers),
                MouseTrackingMode::AnyEvent,
                true,
            )
            .unwrap(),
            b"\x1b[<63;3;4M"
        );
    }

    #[test]
    fn wheel_events_are_press_like_and_have_no_release_sequence() {
        assert_eq!(
            mouse_bytes(
                mouse(MouseEventKind::ScrollUp, 9, 4, KeyModifiers::SHIFT),
                MouseTrackingMode::PressRelease,
                true,
            )
            .unwrap(),
            b"\x1b[<68;10;5M"
        );
        assert_eq!(
            mouse_bytes(
                mouse(MouseEventKind::ScrollDown, 9, 4, KeyModifiers::NONE),
                MouseTrackingMode::PressRelease,
                true,
            )
            .unwrap(),
            b"\x1b[<65;10;5M"
        );
    }

    #[test]
    fn unsupported_or_unnegotiated_mouse_input_stays_local() {
        let event = mouse(MouseEventKind::Down(MouseButton::Left), 0, 0, KeyModifiers::NONE);
        assert!(mouse_bytes(event, MouseTrackingMode::Off, true).is_none());
        assert!(
            mouse_bytes(event, MouseTrackingMode::PressRelease, false).is_none()
        );
    }

    #[test]
    fn one_based_coordinates_preserve_the_full_host_range() {
        assert_eq!(
            mouse_bytes(
                mouse(
                    MouseEventKind::Down(MouseButton::Middle),
                    u16::MAX,
                    u16::MAX,
                    KeyModifiers::NONE,
                ),
                MouseTrackingMode::PressRelease,
                true,
            )
            .unwrap(),
            b"\x1b[<1;65536;65536M"
        );
    }
}
