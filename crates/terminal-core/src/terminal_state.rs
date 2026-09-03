use crate::endpoint::EndpointSize;
use crate::terminal::{Cell, Cursor, TerminalState as GridTerminalState};

const MAX_MOUSE_CSI_PARAMS: usize = 16;
const MAX_TAB_CSI_PARAMS: usize = 16;
const DEFAULT_TAB_WIDTH: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TabAction {
    SetCurrent,
    ClearCurrent,
    ClearAll,
    HorizontalTab,
    Move(u16),
    Reset,
    RestartEscape,
    StartEscape,
    CancelCsi,
    Suppress,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum TabParserState {
    #[default]
    Ground,
    Escape,
    Csi {
        params: [u16; MAX_TAB_CSI_PARAMS],
        len: u8,
        current: u16,
        has_current: bool,
        private: bool,
        overflowed: bool,
    },
    DiscardCsi,
    ControlString {
        allow_bel: bool,
        escape_pending: bool,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TabStopTracker {
    stops: Vec<bool>,
    customized: bool,
    parser: TabParserState,
}

impl TabStopTracker {
    fn new(cols: u16) -> Self {
        let mut tracker = Self {
            stops: vec![false; usize::from(cols)],
            customized: false,
            parser: TabParserState::Ground,
        };
        tracker.install_default_stops(0);
        tracker
    }

    fn resize(&mut self, cols: u16) {
        let previous = self.stops.len();
        let next = usize::from(cols);
        self.stops.resize(next, false);
        if next > previous && !self.customized {
            self.install_default_stops(previous);
        }
    }

    fn install_default_stops(&mut self, start: usize) {
        for col in start..self.stops.len() {
            self.stops[col] = col > 0 && col % DEFAULT_TAB_WIDTH == 0;
        }
    }

    fn reset(&mut self) {
        self.stops.fill(false);
        self.customized = false;
        self.install_default_stops(0);
    }

    fn set_current(&mut self, col: u16) {
        if let Some(stop) = self.stops.get_mut(usize::from(col)) {
            *stop = true;
            self.customized = true;
        }
    }

    fn clear_current(&mut self, col: u16) {
        if let Some(stop) = self.stops.get_mut(usize::from(col)) {
            *stop = false;
            self.customized = true;
        }
    }

    fn clear_all(&mut self) {
        self.stops.fill(false);
        self.customized = true;
    }

    fn target(&self, start: u16, count: u16) -> u16 {
        let Some(right_margin) = self.stops.len().checked_sub(1) else {
            return 0;
        };
        let mut col = usize::from(start).min(right_margin);
        for _ in 0..count.max(1) {
            let next = self
                .stops
                .iter()
                .enumerate()
                .skip(col.saturating_add(1))
                .find_map(|(index, enabled)| enabled.then_some(index));
            if let Some(next) = next {
                col = next;
            } else {
                col = right_margin;
                break;
            }
        }
        u16::try_from(col).unwrap_or(u16::MAX)
    }

    fn feed_byte(&mut self, byte: u8) -> Option<TabAction> {
        match self.parser {
            TabParserState::Ground => match byte {
                b'\t' => Some(TabAction::HorizontalTab),
                0x1b => {
                    self.parser = TabParserState::Escape;
                    None
                }
                _ => None,
            },
            TabParserState::Escape => self.feed_escape(byte),
            TabParserState::Csi {
                params,
                len,
                current,
                has_current,
                private,
                overflowed,
            } => self.feed_csi(
                byte,
                params,
                len,
                current,
                has_current,
                (private, overflowed),
            ),
            TabParserState::DiscardCsi => Some(self.feed_discard_csi(byte)),
            TabParserState::ControlString {
                allow_bel,
                escape_pending,
            } => {
                self.feed_control_string(byte, allow_bel, escape_pending);
                None
            }
        }
    }

    fn feed_escape(&mut self, byte: u8) -> Option<TabAction> {
        match byte {
            b'[' => {
                self.parser = TabParserState::Csi {
                    params: [0; MAX_TAB_CSI_PARAMS],
                    len: 0,
                    current: 0,
                    has_current: false,
                    private: false,
                    overflowed: false,
                };
                None
            }
            b']' => {
                self.parser = TabParserState::ControlString {
                    allow_bel: true,
                    escape_pending: false,
                };
                None
            }
            b'P' | b'X' | b'^' | b'_' => {
                self.parser = TabParserState::ControlString {
                    allow_bel: false,
                    escape_pending: false,
                };
                None
            }
            b'H' => {
                self.parser = TabParserState::Ground;
                Some(TabAction::SetCurrent)
            }
            b'c' => {
                self.parser = TabParserState::Ground;
                Some(TabAction::Reset)
            }
            _ => {
                self.parser = TabParserState::Ground;
                None
            }
        }
    }

    fn feed_csi(
        &mut self,
        byte: u8,
        mut params: [u16; MAX_TAB_CSI_PARAMS],
        mut len: u8,
        mut current: u16,
        mut has_current: bool,
        flags: (bool, bool),
    ) -> Option<TabAction> {
        let (mut private, mut overflowed) = flags;
        match byte {
            0x1b => {
                self.parser = TabParserState::Escape;
                return Some(TabAction::RestartEscape);
            }
            0x18 | 0x1a => {
                self.parser = TabParserState::Ground;
                return Some(TabAction::CancelCsi);
            }
            0x00..=0x1f => {
                self.parser = TabParserState::Csi {
                    params,
                    len,
                    current,
                    has_current,
                    private,
                    overflowed,
                };
                return Some(TabAction::Suppress);
            }
            b'?' if len == 0 && !has_current => private = true,
            b'0'..=b'9' => {
                if usize::from(len) >= params.len() && !has_current {
                    self.parser = TabParserState::DiscardCsi;
                    return Some(TabAction::CancelCsi);
                }
                current = current
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
                has_current = true;
            }
            b';' => {
                Self::push_param(
                    &mut params,
                    &mut len,
                    current,
                    has_current,
                    &mut overflowed,
                );
                if overflowed {
                    self.parser = TabParserState::DiscardCsi;
                    return Some(TabAction::CancelCsi);
                }
                current = 0;
                has_current = false;
            }
            0x3a | 0x3c..=0x3f | 0x20..=0x2f => {
                self.parser = TabParserState::DiscardCsi;
                return Some(TabAction::CancelCsi);
            }
            0x40..=0x7e => {
                if has_current || len > 0 {
                    Self::push_param(
                        &mut params,
                        &mut len,
                        current,
                        has_current,
                        &mut overflowed,
                    );
                }
                self.parser = TabParserState::Ground;
                if overflowed {
                    return Some(TabAction::CancelCsi);
                }
                if private {
                    return None;
                }
                let first = params.first().copied().unwrap_or(0);
                return match byte {
                    b'I' => Some(TabAction::Move(first.max(1))),
                    b'g' if first == 0 => Some(TabAction::ClearCurrent),
                    b'g' if first == 3 => Some(TabAction::ClearAll),
                    _ => None,
                };
            }
            _ => {
                self.parser = TabParserState::Ground;
                return None;
            }
        }
        self.parser = TabParserState::Csi {
            params,
            len,
            current,
            has_current,
            private,
            overflowed,
        };
        None
    }

    fn feed_discard_csi(&mut self, byte: u8) -> TabAction {
        match byte {
            0x1b => {
                self.parser = TabParserState::Escape;
                TabAction::StartEscape
            }
            0x18 | 0x1a | 0x40..=0x7e => {
                self.parser = TabParserState::Ground;
                TabAction::Suppress
            }
            _ => TabAction::Suppress,
        }
    }

    fn push_param(
        params: &mut [u16; MAX_TAB_CSI_PARAMS],
        len: &mut u8,
        current: u16,
        has_current: bool,
        overflowed: &mut bool,
    ) {
        let index = usize::from(*len);
        if index < params.len() {
            params[index] = if has_current { current } else { 0 };
            *len += 1;
        } else {
            *overflowed = true;
        }
    }

    fn feed_control_string(&mut self, byte: u8, allow_bel: bool, escape_pending: bool) {
        if (allow_bel && byte == 0x07) || (escape_pending && byte == b'\\') {
            self.parser = TabParserState::Ground;
            return;
        }
        self.parser = TabParserState::ControlString {
            allow_bel,
            escape_pending: byte == 0x1b,
        };
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MouseTrackingMode {
    #[default]
    Off,
    PressRelease,
    ButtonEvent,
    AnyEvent,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MouseModes {
    tracking: MouseTrackingMode,
    sgr_encoding: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum MouseParserState {
    #[default]
    Ground,
    Escape,
    Csi {
        params: [u16; MAX_MOUSE_CSI_PARAMS],
        len: u8,
        current: u16,
        has_current: bool,
        private: bool,
        overflowed: bool,
    },
    ControlString {
        allow_bel: bool,
        escape_pending: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct MouseModeTracker {
    modes: MouseModes,
    parser: MouseParserState,
}

impl MouseModeTracker {
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.feed_byte(byte);
        }
    }

    fn feed_byte(&mut self, byte: u8) {
        match self.parser {
            MouseParserState::Ground => {
                if byte == 0x1b {
                    self.parser = MouseParserState::Escape;
                }
            }
            MouseParserState::Escape => self.feed_escape(byte),
            MouseParserState::Csi {
                params,
                len,
                current,
                has_current,
                private,
                overflowed,
            } => self.feed_csi(
                byte,
                params,
                len,
                current,
                has_current,
                (private, overflowed),
            ),
            MouseParserState::ControlString {
                allow_bel,
                escape_pending,
            } => self.feed_control_string(byte, allow_bel, escape_pending),
        }
    }

    fn feed_escape(&mut self, byte: u8) {
        self.parser = match byte {
            b'[' => MouseParserState::Csi {
                params: [0; MAX_MOUSE_CSI_PARAMS],
                len: 0,
                current: 0,
                has_current: false,
                private: false,
                overflowed: false,
            },
            b']' => MouseParserState::ControlString {
                allow_bel: true,
                escape_pending: false,
            },
            b'P' | b'X' | b'^' | b'_' => MouseParserState::ControlString {
                allow_bel: false,
                escape_pending: false,
            },
            b'c' => {
                self.modes = MouseModes::default();
                MouseParserState::Ground
            }
            _ => MouseParserState::Ground,
        };
    }

    fn feed_csi(
        &mut self,
        byte: u8,
        mut params: [u16; MAX_MOUSE_CSI_PARAMS],
        mut len: u8,
        mut current: u16,
        mut has_current: bool,
        flags: (bool, bool),
    ) {
        let (mut private, mut overflowed) = flags;
        match byte {
            0x1b => {
                self.parser = MouseParserState::Escape;
                return;
            }
            0x18 | 0x1a => {
                self.parser = MouseParserState::Ground;
                return;
            }
            0x00..=0x1f => {}
            b'?' if len == 0 && !has_current => private = true,
            b'0'..=b'9' => {
                if usize::from(len) >= params.len() && !has_current {
                    overflowed = true;
                    if private {
                        self.modes = MouseModes::default();
                    }
                }
                current = current
                    .saturating_mul(10)
                    .saturating_add(u16::from(byte - b'0'));
                has_current = true;
            }
            b';' => {
                Self::push_param(
                    &mut params,
                    &mut len,
                    current,
                    has_current,
                    &mut overflowed,
                );
                if overflowed && private {
                    self.modes = MouseModes::default();
                }
                current = 0;
                has_current = false;
            }
            0x40..=0x7e => {
                if has_current || len > 0 {
                    Self::push_param(
                        &mut params,
                        &mut len,
                        current,
                        has_current,
                        &mut overflowed,
                    );
                }
                self.parser = MouseParserState::Ground;
                if private && matches!(byte, b'h' | b'l') {
                    if overflowed {
                        self.modes = MouseModes::default();
                    } else {
                        self.apply_private_modes(&params[..usize::from(len)], byte == b'h');
                    }
                }
                return;
            }
            _ => {
                self.parser = MouseParserState::Ground;
                return;
            }
        }

        self.parser = MouseParserState::Csi {
            params,
            len,
            current,
            has_current,
            private,
            overflowed,
        };
    }

    fn push_param(
        params: &mut [u16; MAX_MOUSE_CSI_PARAMS],
        len: &mut u8,
        current: u16,
        has_current: bool,
        overflowed: &mut bool,
    ) {
        let index = usize::from(*len);
        if index < params.len() {
            params[index] = if has_current { current } else { 0 };
            *len += 1;
        } else {
            *overflowed = true;
        }
    }

    fn feed_control_string(&mut self, byte: u8, allow_bel: bool, escape_pending: bool) {
        if (allow_bel && byte == 0x07) || (escape_pending && byte == b'\\') {
            self.parser = MouseParserState::Ground;
            return;
        }
        self.parser = MouseParserState::ControlString {
            allow_bel,
            escape_pending: byte == 0x1b,
        };
    }

    fn apply_private_modes(&mut self, params: &[u16], enabled: bool) {
        for &param in params {
            match param {
                9 | 1001 if enabled => self.modes.tracking = MouseTrackingMode::Off,
                1000 if enabled => self.modes.tracking = MouseTrackingMode::PressRelease,
                1000 if self.modes.tracking == MouseTrackingMode::PressRelease => {
                    self.modes.tracking = MouseTrackingMode::Off;
                }
                1002 if enabled => self.modes.tracking = MouseTrackingMode::ButtonEvent,
                1002 if self.modes.tracking == MouseTrackingMode::ButtonEvent => {
                    self.modes.tracking = MouseTrackingMode::Off;
                }
                1003 if enabled => self.modes.tracking = MouseTrackingMode::AnyEvent,
                1003 if self.modes.tracking == MouseTrackingMode::AnyEvent => {
                    self.modes.tracking = MouseTrackingMode::Off;
                }
                1005 | 1015 | 1016 if enabled => self.modes.sgr_encoding = false,
                1006 => self.modes.sgr_encoding = enabled,
                _ => {}
            }
        }
    }
}

/// Terminal state plus bounded input-negotiation state needed by the frontend.
///
/// The existing grid/parser owns terminal rendering semantics. This wrapper observes only DEC
/// private mouse negotiation and horizontal tab controls so host input authority and geometry-
/// bounded tab state remain outside endpoint transports. Both observers retain fixed parser
/// parameter arrays and never retain control-string payloads. The tab observer also guards the
/// private grid parser's fixed CSI bound so oversized, unsupported-syntax, or interrupted CSI
/// cannot execute a partial prefix or leak its final byte through the public terminal stream.
pub struct TerminalState {
    inner: GridTerminalState,
    mouse: MouseModeTracker,
    tabs: TabStopTracker,
    scrollback_limit: usize,
}

impl TerminalState {
    #[must_use]
    pub fn new(size: EndpointSize, scrollback_limit: usize) -> Self {
        Self {
            inner: GridTerminalState::new(size, scrollback_limit),
            mouse: MouseModeTracker::default(),
            tabs: TabStopTracker::new(size.cols),
            scrollback_limit,
        }
    }

    #[must_use]
    pub fn size(&self) -> EndpointSize { self.inner.size() }
    #[must_use]
    pub fn cursor(&self) -> Cursor { self.inner.cursor() }
    #[must_use]
    pub fn cursor_visible(&self) -> bool { self.inner.cursor_visible() }
    #[must_use]
    pub fn application_cursor_keys(&self) -> bool { self.inner.application_cursor_keys() }
    #[must_use]
    pub fn application_keypad_mode(&self) -> bool { self.inner.application_keypad_mode() }
    #[must_use]
    pub fn bracketed_paste_mode(&self) -> bool { self.inner.bracketed_paste_mode() }
    #[must_use]
    pub fn mouse_tracking_mode(&self) -> MouseTrackingMode { self.mouse.modes.tracking }
    #[must_use]
    pub fn sgr_mouse_mode(&self) -> bool { self.mouse.modes.sgr_encoding }
    #[must_use]
    pub fn mouse_reporting_enabled(&self) -> bool {
        self.sgr_mouse_mode() && self.mouse_tracking_mode() != MouseTrackingMode::Off
    }
    #[must_use]
    pub fn alternate_screen_active(&self) -> bool { self.inner.alternate_screen_active() }
    #[must_use]
    pub fn scrollback_len(&self) -> usize { self.inner.scrollback_len() }
    #[must_use]
    pub fn scrollback_line_text(&self, index: usize) -> Option<String> {
        self.inner.scrollback_line_text(index)
    }
    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell> { self.inner.cell(row, col) }
    #[must_use]
    pub fn line_text(&self, row: u16) -> Option<String> { self.inner.line_text(row) }
    pub fn resize(&mut self, size: EndpointSize) {
        self.tabs.resize(size.cols);
        self.inner.resize(size);
    }
    pub fn feed(&mut self, bytes: &[u8]) {
        self.feed_with_responses(bytes, |_| {});
    }
    pub fn feed_with_responses(&mut self, bytes: &[u8], mut respond: impl FnMut(&[u8])) {
        self.mouse.feed(bytes);
        let mut start = 0;
        for (index, &byte) in bytes.iter().enumerate() {
            let Some(action) = self.tabs.feed_byte(byte) else {
                continue;
            };
            if start < index {
                self.inner
                    .feed_with_responses(&bytes[start..index], &mut respond);
            }
            let cursor_before = self.logical_cursor_col();
            match action {
                TabAction::RestartEscape => {
                    self.inner
                        .feed_with_responses(&[0x1b, 0x1b], &mut respond);
                }
                TabAction::StartEscape => {
                    self.inner.feed_with_responses(&[0x1b], &mut respond);
                }
                TabAction::CancelCsi => {
                    self.inner.feed_with_responses(&[0], &mut respond);
                }
                TabAction::Suppress => {}
                TabAction::HorizontalTab => {
                    self.inner.feed_with_responses(&[0], &mut respond);
                    let cursor_before = self.logical_cursor_col();
                    self.inner
                        .feed_with_responses(&bytes[index..=index], &mut respond);
                    self.apply_tab_action(action, cursor_before);
                }
                _ => {
                    self.inner
                        .feed_with_responses(&bytes[index..=index], &mut respond);
                    self.apply_tab_action(action, cursor_before);
                }
            }
            start = index + 1;
        }
        if start < bytes.len() {
            self.inner.feed_with_responses(&bytes[start..], respond);
        }
    }

    /// Finalizes one terminal byte stream without growing retained parser state.
    ///
    /// A NUL is ignored in Ground but forces the private decoder to replace any incomplete UTF-8
    /// scalar. The following ST is likewise invisible in Ground and closes a pending control
    /// string. Incomplete CSI/ESC state is cancelled rather than partially executed.
    pub fn finish(&mut self) {
        self.inner.feed(&[0, 0x1b, b'\\']);
        self.mouse.parser = MouseParserState::Ground;
        self.tabs.parser = TabParserState::Ground;
    }

    fn logical_cursor_col(&self) -> u16 {
        self.inner.cursor().col.min(self.inner.size().cols - 1)
    }

    fn reset_to_initial_state(&mut self) {
        let size = self.inner.size();
        self.inner = GridTerminalState::new(size, self.scrollback_limit);
        self.tabs.reset();
    }

    fn apply_tab_action(&mut self, action: TabAction, cursor_before: u16) {
        match action {
            TabAction::SetCurrent => self.tabs.set_current(cursor_before),
            TabAction::ClearCurrent => self.tabs.clear_current(cursor_before),
            TabAction::ClearAll => self.tabs.clear_all(),
            TabAction::Reset => self.reset_to_initial_state(),
            TabAction::HorizontalTab => {
                let target = self.tabs.target(cursor_before, 1);
                let sequence = format!("\x1b[{}G", u32::from(target) + 1);
                self.inner.feed(sequence.as_bytes());
            }
            TabAction::Move(count) => {
                let target = self.tabs.target(cursor_before, count);
                let sequence = format!("\x1b[{}G", u32::from(target) + 1);
                self.inner.feed(sequence.as_bytes());
            }
            TabAction::RestartEscape
            | TabAction::StartEscape
            | TabAction::CancelCsi
            | TabAction::Suppress => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CellStyle;

    fn terminal() -> TerminalState {
        TerminalState::new(EndpointSize::new(8, 3).unwrap(), 2)
    }

    fn overflow_private_mode_prefix() -> String {
        let params = (2000..2016)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(";");
        format!("\x1b[?{params};")
    }

    #[test]
    fn mouse_protocol_selection_is_mutually_exclusive() {
        let mut state = terminal();
        state.feed(b"\x1b[?1000;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::PressRelease);
        assert!(state.mouse_reporting_enabled());

        state.feed(b"\x1b[?1002h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::ButtonEvent);
        state.feed(b"\x1b[?1000l");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::ButtonEvent);

        state.feed(b"\x1b[?1003h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::AnyEvent);
        state.feed(b"\x1b[?1003l");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.mouse_reporting_enabled());
    }

    #[test]
    fn unsupported_tracking_selection_clears_stale_supported_mode() {
        let mut state = terminal();
        state.feed(b"\x1b[?1002;1006h");
        assert!(state.mouse_reporting_enabled());

        state.feed(b"\x1b[?9h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.mouse_reporting_enabled());

        state.feed(b"\x1b[?1003h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::AnyEvent);
        state.feed(b"\x1b[?1001h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);

        state.feed(b"\x1b[?1000h");
        state.feed(b"\x1b[?9l");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::PressRelease);
    }

    #[test]
    fn unsupported_encoding_selection_clears_stale_sgr_mode() {
        for unsupported in [1005, 1015, 1016] {
            let mut state = terminal();
            state.feed(b"\x1b[?1000;1006h");
            assert!(state.mouse_reporting_enabled());

            state.feed(format!("\x1b[?{unsupported}h").as_bytes());
            assert!(!state.sgr_mouse_mode());
            assert!(!state.mouse_reporting_enabled());

            state.feed(b"\x1b[?1006h");
            assert!(state.mouse_reporting_enabled());
            state.feed(format!("\x1b[?{unsupported}l").as_bytes());
            assert!(state.sgr_mouse_mode());
        }
    }

    #[test]
    fn mouse_csi_parameter_overflow_fails_closed() {
        let mut state = terminal();
        state.feed(b"\x1b[?1003;1006h");
        assert!(state.mouse_reporting_enabled());

        let prefix = overflow_private_mode_prefix();
        state.feed(prefix.as_bytes());
        assert!(state.mouse_reporting_enabled());
        state.feed(b"1015h");

        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());
    }

    #[test]
    fn mouse_overflow_fails_closed_before_intermediate_bytes_can_end_observation() {
        let params = (2000..2017)
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(";");
        let mut state = terminal();
        state.feed(b"\x1b[?1002;1006h");
        assert!(state.mouse_reporting_enabled());

        state.feed(format!("\x1b[?{params}").as_bytes());
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());

        state.feed(b" q");
        assert!(!state.mouse_reporting_enabled());
        state.feed(b"\x1b[?1000;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::PressRelease);
        assert!(state.mouse_reporting_enabled());
    }

    #[test]
    fn mouse_overflow_state_still_honors_escape_cancellation_and_ris() {
        let mut state = terminal();
        state.feed(b"\x1b[?1002;1006h");
        let prefix = overflow_private_mode_prefix();
        state.feed(prefix.as_bytes());
        state.feed(b"9999;\x1bc");

        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());
    }

    #[test]
    fn public_stream_cancels_interrupted_core_csi_without_rendering_escape_final() {
        let mut state = terminal();
        state.feed(b"A\x1b[31\x1b7Z");
        assert_eq!(state.line_text(0).unwrap(), "AZ      ");
        assert_eq!(state.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn csi_can_and_sub_cancel_before_the_next_printable_byte() {
        for cancel in [0x18, 0x1a] {
            let mut state = terminal();
            state.feed(b"A\x1b[31");
            state.feed(&[cancel, b'X']);
            assert_eq!(state.line_text(0).unwrap(), "AX      ");
            assert_eq!(state.cell(0, 1).unwrap().style, CellStyle::default());
        }
    }

    #[test]
    fn embedded_c0_does_not_expose_a_later_csi_final_as_text() {
        let mut state = terminal();
        state.feed(b"A\x1b[3\x07 qB");
        assert_eq!(state.line_text(0).unwrap(), "AB      ");
        assert_eq!(state.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn mouse_observer_keeps_private_csi_across_embedded_c0() {
        let mut state = terminal();
        state.feed(b"\x1b[?1000\x07;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::PressRelease);
        assert!(state.sgr_mouse_mode());
        assert!(state.mouse_reporting_enabled());
    }

    #[test]
    fn unsupported_csi_intermediates_are_consumed_through_the_final() {
        let mut state = terminal();
        state.feed(b"A\x1b[3 ");
        state.feed(b"qB");
        assert_eq!(state.line_text(0).unwrap(), "AB      ");
        assert_eq!(state.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn unsupported_csi_parameter_syntax_is_consumed_through_the_final() {
        let mut state = terminal();
        state.feed(b"A\x1b[>cB\x1b[38:2::1:2:3mC");
        assert_eq!(state.line_text(0).unwrap(), "ABC     ");
        assert_eq!(state.cell(0, 2).unwrap().style, CellStyle::default());
    }

    #[test]
    fn escape_restarts_while_unsupported_csi_intermediates_are_pending() {
        let mut state = terminal();
        state.feed(b"A\x1b[3 \x1b7B");
        assert_eq!(state.line_text(0).unwrap(), "AB      ");
        assert_eq!(state.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn public_stream_rejects_overflowed_core_private_modes_and_sgr() {
        let mut private = vec!["1".to_owned()];
        private.extend((2000..2016).map(|value| value.to_string()));
        let mut state = terminal();
        state.feed(format!("\x1b[?{}h", private.join(";")).as_bytes());
        assert!(!state.application_cursor_keys());

        let mut sgr = vec!["31".to_owned()];
        sgr.extend((40..56).map(|value| value.to_string()));
        state.feed(format!("\x1b[{}mX", sgr.join(";")).as_bytes());
        assert_eq!(state.cell(0, 0).unwrap().style, CellStyle::default());
    }

    #[test]
    fn overflow_discard_is_chunked_and_restarts_on_escape() {
        let mut params = (0..17).map(|_| "1").collect::<Vec<_>>().join(";");
        params.push(';');
        let mut state = terminal();
        state.feed(format!("\x1b[{params}").as_bytes());
        state.feed(b"999\x1b7Q");
        assert_eq!(state.line_text(0).unwrap(), "Q       ");
    }

    #[test]
    fn overflow_discard_consumes_intermediate_bytes_through_the_final() {
        let mut params = (0..17).map(|_| "1").collect::<Vec<_>>().join(";");
        params.push(';');
        let mut state = terminal();
        state.feed(format!("\x1b[{params}").as_bytes());
        state.feed(b" \x71X");
        assert_eq!(state.line_text(0).unwrap(), "X       ");
    }

    #[test]
    fn overflow_discard_honors_can_and_sub_cancellation() {
        for cancel in [0x18, 0x1a] {
            let mut params = (0..17).map(|_| "1").collect::<Vec<_>>().join(";");
            params.push(';');
            let mut state = terminal();
            state.feed(format!("\x1b[{params}").as_bytes());
            state.feed(&[cancel, b'X']);
            assert_eq!(state.line_text(0).unwrap(), "X       ");
            assert_eq!(state.cursor(), Cursor { row: 0, col: 1 });
        }
    }

    #[test]
    fn ris_resets_mouse_negotiation_and_chunked_reset() {
        let mut state = terminal();
        state.feed(b"\x1b[?1003;1006h");
        assert!(state.mouse_reporting_enabled());
        state.feed(b"\x1b");
        assert!(state.mouse_reporting_enabled());
        state.feed(b"c");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());
    }

    #[test]
    fn ris_allows_later_same_chunk_negotiation_to_become_authoritative() {
        let mut state = terminal();
        state.feed(b"\x1b[?1000;1006h\x1bc\x1b[?1002;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::ButtonEvent);
        assert!(state.sgr_mouse_mode());
        assert!(state.mouse_reporting_enabled());
    }

    #[test]
    fn ris_resets_complete_implemented_terminal_state() {
        let mut state = TerminalState::new(EndpointSize::new(4, 3).unwrap(), 2);
        state.feed(b"a\r\nb\r\nc\r\nd");
        assert!(state.scrollback_len() > 0);

        state.feed(b"\x1b[?1;6;7;2004h\x1b=\x1b[31m\x1b[?1003;1006h");
        state.feed(b"\x1b[?25l\x1b[?1049hALT");
        assert!(state.application_cursor_keys());
        assert!(state.application_keypad_mode());
        assert!(state.bracketed_paste_mode());
        assert!(state.mouse_reporting_enabled());
        assert!(!state.cursor_visible());
        assert!(state.alternate_screen_active());

        state.feed(b"\x1b");
        state.feed(b"c");
        assert_eq!(state.cursor(), Cursor { row: 0, col: 0 });
        assert!(state.cursor_visible());
        assert!(!state.application_cursor_keys());
        assert!(!state.application_keypad_mode());
        assert!(!state.bracketed_paste_mode());
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.alternate_screen_active());
        assert_eq!(state.scrollback_len(), 0);
        assert_eq!(state.line_text(0).unwrap(), "    ");
        assert_eq!(state.line_text(1).unwrap(), "    ");
        assert_eq!(state.line_text(2).unwrap(), "    ");

        state.feed(b"abcdX");
        assert_eq!(state.cell(0, 0).unwrap().style, CellStyle::default());
        assert_eq!(state.cell(1, 0).unwrap().character, 'X');
        assert_eq!(state.cursor(), Cursor { row: 1, col: 1 });
    }

    #[test]
    fn ris_interrupting_mouse_csi_resets_negotiation() {
        let mut state = terminal();
        state.feed(b"\x1b[?1003;1006h");
        assert!(state.mouse_reporting_enabled());

        state.feed(b"\x1b[?1000\x1bc");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());

        state.feed(b"\x1b[?1002;1006h\x1b[?1000\x1bH");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::ButtonEvent);
        assert!(state.sgr_mouse_mode());
        assert_eq!(state.line_text(0).unwrap(), "        ");
    }

    #[test]
    fn combined_and_chunked_unsupported_switches_do_not_leave_stale_state() {
        let mut state = terminal();
        state.feed(b"\x1b[?1002;1006h");
        state.feed(b"\x1b[?10");
        state.feed(b"01;1015h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());

        state.feed(b"\x1b[?9;1003;1005;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::AnyEvent);
        assert!(state.sgr_mouse_mode());
        assert!(state.mouse_reporting_enabled());
    }

    #[test]
    fn sgr_encoding_is_independent_and_combined_lists_are_bounded() {
        let mut state = terminal();
        state.feed(b"\x1b[?1000;1002;1003;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::AnyEvent);
        assert!(state.sgr_mouse_mode());
        assert!(state.mouse_reporting_enabled());

        state.feed(b"\x1b[?1006l");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::AnyEvent);
        assert!(!state.sgr_mouse_mode());
        assert!(!state.mouse_reporting_enabled());
    }

    #[test]
    fn mouse_negotiation_survives_chunks_and_ignores_control_string_payloads() {
        let mut state = terminal();
        state.feed(b"\x1b]0;\x1b[?1003;1006h");
        state.feed(b"\x07");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::Off);

        state.feed(b"\x1b[?10");
        state.feed(b"02;1006h");
        assert_eq!(state.mouse_tracking_mode(), MouseTrackingMode::ButtonEvent);
        assert!(state.mouse_reporting_enabled());
    }

    #[test]
    fn configurable_tab_stops_drive_ht_and_cht() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        state.feed(b"\x1b[3g");
        state.feed(b"\x1b[1;5H\x1bH");
        state.feed(b"\x1b[1;11H\x1bH");
        state.feed(b"\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 4);
        state.feed(b"\x1b[1;1H\x1b[2I");
        assert_eq!(state.cursor().col, 10);
    }

    #[test]
    fn tbc_clears_current_or_all_and_unsupported_parameters_are_ignored() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        state.feed(b"\x1b[1;9H\x1b[0g\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 16);

        state.feed(b"\x1b[2g");
        state.feed(b"\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 16);

        state.feed(b"\x1b[3g\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 19);
    }

    #[test]
    fn tab_csi_parameter_overflow_does_not_apply_partial_action() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        let mut params = vec!["3".to_owned()];
        params.extend((0..16).map(|_| "0".to_owned()));
        let sequence = format!("\x1b[{}g", params.join(";"));
        let split = sequence.len() - 2;
        state.feed(&sequence.as_bytes()[..split]);
        state.feed(&sequence.as_bytes()[split..]);
        state.feed(b"\x1b[1;1H\t");

        assert_eq!(state.cursor().col, 8);
    }

    #[test]
    fn tab_overflow_state_still_honors_ris() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        state.feed(b"\x1b[3g\x1b[1;5H\x1bH");
        let params = (0..17).map(|_| "3").collect::<Vec<_>>().join(";");
        state.feed(format!("\x1b[{params};\x1bc\x1b[1;1H\t").as_bytes());

        assert_eq!(state.cursor().col, 8);
    }

    #[test]
    fn tab_stop_resize_is_geometry_bounded_and_does_not_refabricate_customized_stops() {
        let mut pristine = TerminalState::new(EndpointSize::new(10, 3).unwrap(), 2);
        pristine.resize(EndpointSize::new(18, 3).unwrap());
        pristine.feed(b"\x1b[1;1H\x1b[2I");
        assert_eq!(pristine.cursor().col, 16);

        let mut customized = TerminalState::new(EndpointSize::new(10, 3).unwrap(), 2);
        customized.feed(b"\x1b[3g");
        customized.resize(EndpointSize::new(18, 3).unwrap());
        customized.feed(b"\x1b[1;1H\t");
        assert_eq!(customized.cursor().col, 17);

        customized.feed(b"\x1b[1;5H\x1bH");
        customized.resize(EndpointSize::new(3, 3).unwrap());
        customized.resize(EndpointSize::new(10, 3).unwrap());
        customized.feed(b"\x1b[1;1H\t");
        assert_eq!(customized.cursor().col, 9);
    }

    #[test]
    fn incomplete_utf8_is_recovered_before_horizontal_tab_origin_is_captured() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        state.feed(b"\x1b[3g\x1b[1;5H\x1bH\x1b[1;10H\x1bH");
        state.feed(b"\x1b[1;4H\xc2\t");
        assert_eq!(state.cell(0, 3).unwrap().character, '\u{fffd}');
        assert_eq!(state.cursor().col, 9);
    }

    #[test]
    fn finish_replaces_incomplete_utf8_once_and_leaves_complete_utf8_unchanged() {
        for prefix in [
            b"\xc2".as_slice(),
            b"\xe2\x98".as_slice(),
            b"\xf0\x9f\x92".as_slice(),
        ] {
            let mut incomplete = terminal();
            incomplete.feed(b"A");
            incomplete.feed(prefix);
            incomplete.finish();
            assert_eq!(incomplete.cell(0, 0).unwrap().character, 'A');
            assert_eq!(incomplete.cell(0, 1).unwrap().character, '\u{fffd}');
            assert_eq!(incomplete.cursor(), Cursor { row: 0, col: 2 });
            incomplete.finish();
            assert_eq!(incomplete.cursor(), Cursor { row: 0, col: 2 });
        }

        let mut complete = terminal();
        complete.feed("☃".as_bytes());
        complete.finish();
        assert_eq!(complete.cell(0, 0).unwrap().character, '☃');
        assert_eq!(complete.cursor(), Cursor { row: 0, col: 1 });
    }

    #[test]
    fn finish_cancels_incomplete_controls_without_leaking_payload_or_partial_actions() {
        let mut escape = terminal();
        escape.feed(b"A\x1b");
        escape.finish();
        escape.feed(b"B");
        assert_eq!(escape.line_text(0).unwrap(), "AB      ");

        let mut csi = terminal();
        csi.feed(b"A\x1b[31");
        csi.finish();
        csi.feed(b"B");
        assert_eq!(csi.line_text(0).unwrap(), "AB      ");
        assert_eq!(csi.cell(0, 1).unwrap().style, CellStyle::default());

        let mut control = terminal();
        control.feed(b"A\x1b]0;secret");
        control.finish();
        control.feed(b"B");
        assert_eq!(control.line_text(0).unwrap(), "AB      ");
    }

    #[test]
    fn delayed_wrap_uses_right_margin_for_hts_and_tbc() {
        let mut cleared = TerminalState::new(EndpointSize::new(9, 3).unwrap(), 2);
        cleared.feed(b"\x1b[1;9HX\x1b[g");
        cleared.resize(EndpointSize::new(18, 3).unwrap());
        cleared.feed(b"\x1b[1;1H\t");
        assert_eq!(cleared.cursor().col, 17);

        let mut set = TerminalState::new(EndpointSize::new(9, 3).unwrap(), 2);
        set.feed(b"\x1b[3g\x1b[1;9HX\x1bH");
        set.resize(EndpointSize::new(18, 3).unwrap());
        set.feed(b"\x1b[1;1H\t");
        assert_eq!(set.cursor().col, 8);
    }

    #[test]
    fn ris_restores_default_tab_stops_and_control_strings_do_not_mutate_them() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        state.feed(b"\x1b[3g\x1b[1;5H\x1bH");
        state.feed(b"\x1b]0;\x1bH\x07");
        state.feed(b"\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 4);

        state.feed(b"\x1bc\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 8);
    }

    #[test]
    fn ris_interrupting_tab_csi_restores_defaults_without_leaking_params() {
        let mut state = TerminalState::new(EndpointSize::new(20, 3).unwrap(), 2);
        state.feed(b"\x1b[3g\x1b[1;5H\x1bH");
        state.feed(b"\x1b[3\x1bc\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 8);

        state.feed(b"\x1b[3g\x1b[1;5H\x1bH\x1b[3\x1bH\x1b[1;1H\t");
        assert_eq!(state.cursor().col, 4);
        assert_eq!(state.line_text(0).unwrap(), "                    ");
    }

    #[test]
    fn wrapper_preserves_grid_and_response_behavior() {
        let mut state = terminal();
        state.feed(b"abc\x1b[2;3HZ");
        assert_eq!(state.line_text(0).unwrap(), "abc     ");
        assert_eq!(state.cell(1, 2).unwrap().character, 'Z');

        let mut replies = Vec::new();
        state.feed_with_responses(b"\x1b[6n", |reply| replies.push(reply.to_vec()));
        assert_eq!(replies, vec![b"\x1b[2;4R".to_vec()]);
    }
}
