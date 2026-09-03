use std::collections::VecDeque;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::EndpointSize;

const WIDE_CONTINUATION: char = '\0';
const ZERO_WIDTH_JOINER: char = '\u{200d}';
const MAX_CSI_PARAMS: usize = 16;
const DEFAULT_TAB_WIDTH: u16 = 8;
pub const MAX_COMBINING_MARKS_PER_CELL: usize = 3;
const MAX_GRAPHEME_SUFFIX_SCALARS: usize = 15;
const MAX_CELL_SEQUENCE_BYTES: usize = (MAX_GRAPHEME_SUFFIX_SCALARS + 2) * 4;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TerminalColor {
    #[default]
    Default,
    Indexed(u8),
    Rgb {
        r: u8,
        g: u8,
        b: u8,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellAttributes(u8);

impl CellAttributes {
    pub const BOLD: Self = Self(1 << 0);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const INVERSE: Self = Self(1 << 3);

    #[must_use]
    pub const fn contains(self, attribute: Self) -> bool {
        self.0 & attribute.0 != 0
    }

    #[must_use]
    pub const fn with(self, attribute: Self) -> Self {
        Self(self.0 | attribute.0)
    }

    fn set(&mut self, attribute: Self, enabled: bool) {
        if enabled {
            self.0 |= attribute.0;
        } else {
            self.0 &= !attribute.0;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CellStyle {
    pub attributes: CellAttributes,
    pub foreground: TerminalColor,
    pub background: TerminalColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cell {
    pub character: char,
    combining: [char; MAX_GRAPHEME_SUFFIX_SCALARS],
    combining_len: u8,
    pub style: CellStyle,
}

impl Cell {
    #[must_use]
    pub fn display_character(&self) -> char {
        printable_character(self.character)
    }

    /// Returns the bounded scalar suffix stored with this cell's base character.
    ///
    /// Historically this contained only zero-width combining marks. It now also carries the
    /// small set of emoji sequence scalars that must stay attached to the base cell so the
    /// renderer can hand the sequence to the host shaper without false cursor advancement.
    #[must_use]
    pub fn combining_marks(&self) -> &[char] {
        &self.combining[..usize::from(self.combining_len)]
    }

    fn push_sequence_scalar(&mut self, character: char) -> bool {
        let index = usize::from(self.combining_len);
        if index >= MAX_GRAPHEME_SUFFIX_SCALARS {
            return false;
        }
        self.combining[index] = character;
        self.combining_len += 1;
        true
    }

    fn generic_combining_mark_count(&self) -> usize {
        self.combining_marks()
            .iter()
            .filter(|&&character| {
                terminal_cell_width(character) == 0 && !is_emoji_sequence_scalar(character)
            })
            .count()
    }

    fn sequence_width(&self) -> u16 {
        cell_sequence_width(self, None)
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            combining: ['\0'; MAX_GRAPHEME_SUFFIX_SCALARS],
            combining_len: 0,
            style: CellStyle::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SavedCursorState {
    cursor: Cursor,
    style: CellStyle,
    origin_mode: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct InputModes {
    application_cursor_keys: bool,
    application_keypad: bool,
    bracketed_paste: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PresentationModes {
    auto_wrap: bool,
    origin_mode: bool,
    right_margin_graphic: bool,
}

impl Default for PresentationModes {
    fn default() -> Self {
        Self {
            auto_wrap: true,
            origin_mode: false,
            right_margin_graphic: false,
        }
    }
}

#[derive(Clone, Debug)]
enum ParserState {
    Ground,
    Escape,
    Csi {
        params: Vec<u16>,
        current: Option<u16>,
        private: bool,
    },
    ControlString {
        allow_bel: bool,
        escape_pending: bool,
    },
}

/// Minimal VT-style terminal state machine.
///
/// This module owns terminal presentation state only. It does not know about
/// shell prompts, commands, annotations, endpoint persistence, or transports.
pub struct TerminalState {
    size: EndpointSize,
    primary: Vec<Cell>,
    alternate: Vec<Cell>,
    alternate_active: bool,
    cursor: Cursor,
    saved_cursor_state: SavedCursorState,
    alternate_entry_state: SavedCursorState,
    cursor_visible: bool,
    input_modes: InputModes,
    presentation_modes: PresentationModes,
    style: CellStyle,
    scroll_top: u16,
    scroll_bottom: u16,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_limit: usize,
    parser: ParserState,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_expected: usize,
}

impl TerminalState {
    #[must_use]
    pub fn new(size: EndpointSize, scrollback_limit: usize) -> Self {
        let cells = usize::from(size.cols) * usize::from(size.rows);
        Self {
            size,
            primary: vec![Cell::default(); cells],
            alternate: vec![Cell::default(); cells],
            alternate_active: false,
            cursor: Cursor::default(),
            saved_cursor_state: SavedCursorState::default(),
            alternate_entry_state: SavedCursorState::default(),
            cursor_visible: true,
            input_modes: InputModes::default(),
            presentation_modes: PresentationModes::default(),
            style: CellStyle::default(),
            scroll_top: 0,
            scroll_bottom: size.rows - 1,
            scrollback: VecDeque::new(),
            scrollback_limit,
            parser: ParserState::Ground,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_expected: 0,
        }
    }

    #[must_use]
    pub fn size(&self) -> EndpointSize {
        self.size
    }

    #[must_use]
    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    #[must_use]
    pub fn application_cursor_keys(&self) -> bool {
        self.input_modes.application_cursor_keys
    }

    #[must_use]
    pub fn application_keypad_mode(&self) -> bool {
        self.input_modes.application_keypad
    }

    #[must_use]
    pub fn bracketed_paste_mode(&self) -> bool {
        self.input_modes.bracketed_paste
    }

    #[must_use]
    pub fn alternate_screen_active(&self) -> bool {
        self.alternate_active
    }

    #[must_use]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }

    #[must_use]
    pub fn scrollback_line_text(&self, index: usize) -> Option<String> {
        self.scrollback.get(index).map(|line| line_text(line))
    }

    #[must_use]
    pub fn cell(&self, row: u16, col: u16) -> Option<Cell> {
        if row >= self.size.rows || col >= self.size.cols {
            return None;
        }
        self.active_cells().get(self.index(row, col)).copied()
    }

    #[must_use]
    pub fn line_text(&self, row: u16) -> Option<String> {
        if row >= self.size.rows {
            return None;
        }
        let start = self.index(row, 0);
        let end = start + usize::from(self.size.cols);
        Some(line_text(&self.active_cells()[start..end]))
    }

    /// Resizes both screen buffers while preserving the overlapping cell rectangle.
    ///
    /// This intentionally performs no line reflow. Reflow is a presentation policy that can
    /// be added later without changing endpoint transport semantics. Existing scrollback lines
    /// are truncated or padded to the new width so every retained row has a stable shape.
    pub fn resize(&mut self, size: EndpointSize) {
        if size == self.size {
            return;
        }

        let previous = self.size;
        self.primary = Self::resize_cells(&self.primary, previous, size);
        self.alternate = Self::resize_cells(&self.alternate, previous, size);
        for line in &mut self.scrollback {
            line.resize(usize::from(size.cols), Cell::default());
            normalize_wide_row(line);
        }
        self.size = size;
        self.scroll_top = 0;
        self.scroll_bottom = size.rows - 1;
        normalize_wide_grid(&mut self.primary, size);
        normalize_wide_grid(&mut self.alternate, size);
        self.cursor.row = self.cursor.row.min(size.rows - 1);
        self.cursor.col = self.cursor.col.min(size.cols - 1);
        self.saved_cursor_state.cursor.row = self.saved_cursor_state.cursor.row.min(size.rows - 1);
        self.saved_cursor_state.cursor.col = self.saved_cursor_state.cursor.col.min(size.cols - 1);
        self.alternate_entry_state.cursor.row =
            self.alternate_entry_state.cursor.row.min(size.rows - 1);
        self.alternate_entry_state.cursor.col =
            self.alternate_entry_state.cursor.col.min(size.cols - 1);
        self.presentation_modes.right_margin_graphic = false;
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.feed_with_responses(bytes, |_| {});
    }

    /// Feeds terminal output and synchronously emits protocol replies requested by the peer.
    ///
    /// Replies are delivered directly to the provided sink instead of being queued inside the
    /// terminal state, so hostile or buggy applications cannot create an unbounded response
    /// buffer by repeatedly issuing status queries.
    pub fn feed_with_responses(&mut self, bytes: &[u8], mut respond: impl FnMut(&[u8])) {
        for &byte in bytes {
            self.feed_byte(byte, &mut respond);
        }
    }

    fn resize_cells(cells: &[Cell], old_size: EndpointSize, new_size: EndpointSize) -> Vec<Cell> {
        let mut resized =
            vec![Cell::default(); usize::from(new_size.cols) * usize::from(new_size.rows)];
        let rows = old_size.rows.min(new_size.rows);
        let cols = old_size.cols.min(new_size.cols);
        for row in 0..rows {
            let old_start = usize::from(row) * usize::from(old_size.cols);
            let new_start = usize::from(row) * usize::from(new_size.cols);
            let count = usize::from(cols);
            resized[new_start..new_start + count]
                .copy_from_slice(&cells[old_start..old_start + count]);
        }
        resized
    }

    fn active_cells(&self) -> &[Cell] {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn active_cells_mut(&mut self) -> &mut [Cell] {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    fn index(&self, row: u16, col: u16) -> usize {
        usize::from(row) * usize::from(self.size.cols) + usize::from(col)
    }

    fn erase_cell(&self) -> Cell {
        Cell {
            style: CellStyle {
                background: self.style.background,
                ..CellStyle::default()
            },
            ..Cell::default()
        }
    }

    fn clear_wide_occupant(&mut self, row: u16, col: u16) {
        let index = self.index(row, col);
        let cells = self.active_cells_mut();
        if cells[index].character == WIDE_CONTINUATION {
            if index > 0 {
                cells[index - 1] = Cell::default();
            }
            cells[index] = Cell::default();
        } else if index + 1 < cells.len() && cells[index + 1].character == WIDE_CONTINUATION {
            cells[index] = Cell::default();
            cells[index + 1] = Cell::default();
        }
    }

    fn previous_graphic_position(&self) -> Option<(u16, u16)> {
        let row = self.cursor.row;
        let mut col =
            if self.presentation_modes.right_margin_graphic && !self.presentation_modes.auto_wrap {
                self.size.cols - 1
            } else {
                if self.cursor.col == 0 {
                    return None;
                }
                self.cursor.col.min(self.size.cols).saturating_sub(1)
            };
        let index = self.index(row, col);
        if self.active_cells()[index].character == WIDE_CONTINUATION && col > 0 {
            col -= 1;
        }
        Some((row, col))
    }

    fn should_extend_previous_cell(cell: Cell, character: char) -> bool {
        if cell.character == WIDE_CONTINUATION || cell.character == ' ' {
            return false;
        }

        let zero_width = terminal_cell_width(character) == 0;
        let emoji_sequence_scalar = is_emoji_sequence_scalar(character);
        let after_joiner = cell.combining_marks().last().copied() == Some(ZERO_WIDTH_JOINER);
        let regional_indicator_pair = is_regional_indicator(cell.character)
            && cell.combining_marks().is_empty()
            && is_regional_indicator(character);

        if zero_width
            && !emoji_sequence_scalar
            && cell.generic_combining_mark_count() >= MAX_COMBINING_MARKS_PER_CELL
        {
            return false;
        }
        if !(zero_width || emoji_sequence_scalar || after_joiner || regional_indicator_pair) {
            return false;
        }

        cell_sequence_width(&cell, Some(character)) <= 2
    }

    fn try_extend_previous_cell(&mut self, character: char) -> bool {
        let Some((row, col)) = self.previous_graphic_position() else {
            return false;
        };
        let index = self.index(row, col);
        let cell = self.active_cells()[index];
        if !Self::should_extend_previous_cell(cell, character) {
            return false;
        }

        if usize::from(cell.combining_len) >= MAX_GRAPHEME_SUFFIX_SCALARS {
            return true;
        }

        let old_width = cell.sequence_width();
        let mut updated = cell;
        if !updated.push_sequence_scalar(character) {
            return true;
        }
        let new_width = updated.sequence_width();
        self.replace_sequence_cell(row, col, updated, old_width, new_width);
        true
    }

    fn replace_sequence_cell(
        &mut self,
        row: u16,
        col: u16,
        updated: Cell,
        old_width: u16,
        new_width: u16,
    ) {
        if old_width == new_width {
            let index = self.index(row, col);
            self.active_cells_mut()[index] = updated;
            return;
        }

        if old_width == 1 && new_width == 2 {
            if col + 1 < self.size.cols {
                self.clear_wide_occupant(row, col + 1);
                let index = self.index(row, col);
                let cells = self.active_cells_mut();
                cells[index] = updated;
                cells[index + 1] = Cell {
                    character: WIDE_CONTINUATION,
                    style: updated.style,
                    ..Cell::default()
                };
                if self.cursor.row == row {
                    self.cursor.col = if self.presentation_modes.auto_wrap {
                        self.cursor.col.saturating_add(1)
                    } else {
                        self.cursor.col.saturating_add(1).min(self.size.cols - 1)
                    };
                }
                self.presentation_modes.right_margin_graphic = !self.presentation_modes.auto_wrap
                    && col.saturating_add(new_width) >= self.size.cols;
                return;
            }

            if !self.presentation_modes.auto_wrap || self.size.cols < 2 {
                let index = self.index(row, col);
                self.active_cells_mut()[index] = Cell {
                    character: '\u{fffd}',
                    style: updated.style,
                    ..Cell::default()
                };
                self.cursor.col = self.size.cols - 1;
                self.presentation_modes.right_margin_graphic = true;
                return;
            }

            let old_index = self.index(row, col);
            self.active_cells_mut()[old_index] = Cell::default();
            self.cursor.col = 0;
            self.line_feed();
            let target_row = self.cursor.row;
            self.clear_wide_occupant(target_row, 0);
            self.clear_wide_occupant(target_row, 1);
            let target_index = self.index(target_row, 0);
            let cells = self.active_cells_mut();
            cells[target_index] = updated;
            cells[target_index + 1] = Cell {
                character: WIDE_CONTINUATION,
                style: updated.style,
                ..Cell::default()
            };
            self.cursor.col = 2;
            return;
        }

        if old_width == 2 && new_width == 1 {
            let index = self.index(row, col);
            let cells = self.active_cells_mut();
            cells[index] = updated;
            if index + 1 < cells.len() && cells[index + 1].character == WIDE_CONTINUATION {
                cells[index + 1] = Cell::default();
            }
            if self.presentation_modes.auto_wrap && self.cursor.row == row && self.cursor.col > col {
                self.cursor.col = self.cursor.col.saturating_sub(1);
            }
            self.presentation_modes.right_margin_graphic = !self.presentation_modes.auto_wrap
                && col.saturating_add(new_width) >= self.size.cols;
            return;
        }

        let index = self.index(row, col);
        self.active_cells_mut()[index] = updated;
    }

    fn put_char(&mut self, character: char) {
        if self.try_extend_previous_cell(character) {
            return;
        }

        let width = terminal_cell_width(character);
        if width == 0 {
            return;
        }

        if self.presentation_modes.auto_wrap {
            if self.cursor.col >= self.size.cols
                || (width == 2 && self.cursor.col + 1 >= self.size.cols)
            {
                self.cursor.col = 0;
                self.line_feed();
            }
        } else {
            self.cursor.col = self.cursor.col.min(self.size.cols - 1);
            if width == 2 && self.cursor.col + 1 >= self.size.cols {
                let row = self.cursor.row;
                let col = self.size.cols - 1;
                self.clear_wide_occupant(row, col);
                let index = self.index(row, col);
                let style = self.style;
                self.active_cells_mut()[index] = Cell {
                    character: '\u{fffd}',
                    style,
                    ..Cell::default()
                };
                self.cursor.col = col;
                self.presentation_modes.right_margin_graphic = true;
                return;
            }
        }

        let row = self.cursor.row;
        let col = self.cursor.col;
        self.clear_wide_occupant(row, col);
        if width == 2 {
            self.clear_wide_occupant(row, col + 1);
        }

        let index = self.index(row, col);
        let style = self.style;
        let cells = self.active_cells_mut();
        cells[index] = Cell {
            character,
            style,
            ..Cell::default()
        };
        if width == 2 {
            cells[index + 1] = Cell {
                character: WIDE_CONTINUATION,
                style,
                ..Cell::default()
            };
        }
        let advanced = self.cursor.col.saturating_add(width);
        self.cursor.col = if self.presentation_modes.auto_wrap {
            advanced
        } else {
            advanced.min(self.size.cols - 1)
        };
        self.presentation_modes.right_margin_graphic =
            !self.presentation_modes.auto_wrap && col.saturating_add(width) >= self.size.cols;
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.presentation_modes.right_margin_graphic = false;
    }

    fn horizontal_tab(&mut self, count: u16) {
        let stop_index = u32::from(self.cursor.col / DEFAULT_TAB_WIDTH) + u32::from(count.max(1));
        let target = stop_index.saturating_mul(u32::from(DEFAULT_TAB_WIDTH));
        self.cursor.col = u16::try_from(target)
            .unwrap_or(u16::MAX)
            .min(self.size.cols - 1);
        self.presentation_modes.right_margin_graphic = false;
    }

    fn line_feed(&mut self) {
        self.presentation_modes.right_margin_graphic = false;
        if self.cursor.row == self.scroll_bottom {
            self.scroll_region_up(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor.row + 1 < self.size.rows {
            self.cursor.row += 1;
        }
        // Otherwise the cursor is already on the last physical row but below a
        // custom scroll region's bottom margin (scroll_bottom < rows - 1). Per
        // DECSTBM semantics, rows outside the active scroll region are never
        // touched by a margin-relative scroll, and the cursor cannot move down
        // further, so this is a no-op. Note this branch is unreachable when no
        // custom region is set (scroll_bottom == rows - 1), since cursor.row
        // then always equals scroll_bottom and takes the first branch instead.
    }

    fn reverse_index(&mut self) {
        self.presentation_modes.right_margin_graphic = false;
        if self.cursor.row == self.scroll_top {
            self.scroll_region_down(self.scroll_top, self.scroll_bottom, 1);
        } else if self.cursor.row > 0 {
            self.cursor.row -= 1;
        }
    }

    fn scroll_region_up(&mut self, top: u16, bottom: u16, count: u16) {
        if top > bottom || bottom >= self.size.rows {
            return;
        }
        let width = usize::from(self.size.cols);
        let height = usize::from(bottom - top + 1);
        let count = usize::from(count).min(height);
        if count == 0 {
            return;
        }

        if !self.alternate_active
            && top == 0
            && bottom == self.size.rows - 1
            && self.scrollback_limit > 0
        {
            for row in 0..count {
                let start = row * width;
                self.scrollback
                    .push_back(self.primary[start..start + width].to_vec());
                while self.scrollback.len() > self.scrollback_limit {
                    self.scrollback.pop_front();
                }
            }
        }

        let start = usize::from(top) * width;
        let end = (usize::from(bottom) + 1) * width;
        let shift = count * width;
        let blank = self.erase_cell();
        let cells = self.active_cells_mut();
        cells.copy_within(start + shift..end, start);
        cells[end - shift..end].fill(blank);
    }

    fn scroll_region_down(&mut self, top: u16, bottom: u16, count: u16) {
        if top > bottom || bottom >= self.size.rows {
            return;
        }
        let width = usize::from(self.size.cols);
        let height = usize::from(bottom - top + 1);
        let count = usize::from(count).min(height);
        if count == 0 {
            return;
        }
        let start = usize::from(top) * width;
        let end = (usize::from(bottom) + 1) * width;
        let shift = count * width;
        let blank = self.erase_cell();
        let cells = self.active_cells_mut();
        cells.copy_within(start..end - shift, start + shift);
        cells[start..start + shift].fill(blank);
    }

    fn clear_screen(&mut self) {
        let blank = self.erase_cell();
        self.active_cells_mut().fill(blank);
    }

    fn erase_display(&mut self, mode: u16) {
        let cursor = self.index(self.cursor.row, self.cursor.col.min(self.size.cols - 1));
        let blank = self.erase_cell();
        match mode {
            0 => {
                self.active_cells_mut()[cursor..].fill(blank);
                let size = self.size;
                normalize_wide_grid_with_blank(self.active_cells_mut(), size, blank);
            }
            1 => {
                self.active_cells_mut()[..=cursor].fill(blank);
                let size = self.size;
                normalize_wide_grid_with_blank(self.active_cells_mut(), size, blank);
            }
            2 => self.clear_screen(),
            3 => {
                self.clear_screen();
                if !self.alternate_active {
                    self.scrollback.clear();
                }
            }
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        let start = self.index(self.cursor.row, 0);
        let cursor = self.index(self.cursor.row, self.cursor.col.min(self.size.cols - 1));
        let end = start + usize::from(self.size.cols);
        let blank = self.erase_cell();
        let cells = self.active_cells_mut();
        let row = &mut cells[start..end];
        match mode {
            0 => row[cursor - start..].fill(blank),
            1 => row[..=cursor - start].fill(blank),
            2 => row.fill(blank),
            _ => return,
        }
        normalize_wide_row_with_blank(row, blank);
    }

    fn insert_chars(&mut self, count: u16) {
        self.presentation_modes.right_margin_graphic = false;
        let width = usize::from(self.size.cols);
        let row_start = usize::from(self.cursor.row) * width;
        let col = usize::from(self.cursor.col.min(self.size.cols - 1));
        let count = usize::from(count.max(1)).min(width - col);
        let blank = self.erase_cell();
        let cells = self.active_cells_mut();
        let row = &mut cells[row_start..row_start + width];
        row.copy_within(col..width - count, col + count);
        row[col..col + count].fill(blank);
        normalize_wide_row_with_blank(row, blank);
    }

    fn delete_chars(&mut self, count: u16) {
        self.presentation_modes.right_margin_graphic = false;
        let width = usize::from(self.size.cols);
        let row_start = usize::from(self.cursor.row) * width;
        let col = usize::from(self.cursor.col.min(self.size.cols - 1));
        let count = usize::from(count.max(1)).min(width - col);
        let blank = self.erase_cell();
        let cells = self.active_cells_mut();
        let row = &mut cells[row_start..row_start + width];
        row.copy_within(col + count..width, col);
        row[width - count..].fill(blank);
        normalize_wide_row_with_blank(row, blank);
    }

    fn insert_lines(&mut self, count: u16) {
        self.presentation_modes.right_margin_graphic = false;
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        self.scroll_region_down(self.cursor.row, self.scroll_bottom, count.max(1));
    }

    fn delete_lines(&mut self, count: u16) {
        self.presentation_modes.right_margin_graphic = false;
        if self.cursor.row < self.scroll_top || self.cursor.row > self.scroll_bottom {
            return;
        }
        self.scroll_region_up(self.cursor.row, self.scroll_bottom, count.max(1));
    }

    fn vertical_bounds(&self) -> (u16, u16) {
        if self.presentation_modes.origin_mode {
            (self.scroll_top, self.scroll_bottom)
        } else {
            (0, self.size.rows - 1)
        }
    }

    fn home_cursor(&mut self) {
        self.cursor = Cursor {
            row: if self.presentation_modes.origin_mode {
                self.scroll_top
            } else {
                0
            },
            col: 0,
        };
        self.presentation_modes.right_margin_graphic = false;
    }

    fn current_cursor_state(&self) -> SavedCursorState {
        SavedCursorState {
            cursor: self.cursor,
            style: self.style,
            origin_mode: self.presentation_modes.origin_mode,
        }
    }

    fn save_cursor_state(&mut self) {
        self.saved_cursor_state = self.current_cursor_state();
    }

    fn restore_saved_cursor_state(&mut self, saved: SavedCursorState) {
        self.presentation_modes.origin_mode = saved.origin_mode;
        let (top, bottom) = self.vertical_bounds();
        self.cursor = saved.cursor;
        self.cursor.row = self.cursor.row.clamp(top, bottom);
        if !self.presentation_modes.auto_wrap {
            self.cursor.col = self.cursor.col.min(self.size.cols - 1);
        }
        self.style = saved.style;
        self.presentation_modes.right_margin_graphic = false;
    }

    fn restore_cursor_state(&mut self) {
        self.restore_saved_cursor_state(self.saved_cursor_state);
    }

    fn move_cursor_up(&mut self, count: u16) {
        let (top, _) = self.vertical_bounds();
        self.cursor.row = self.cursor.row.saturating_sub(count.max(1)).max(top);
        self.presentation_modes.right_margin_graphic = false;
    }

    fn move_cursor_down(&mut self, count: u16) {
        let (_, bottom) = self.vertical_bounds();
        self.cursor.row = self.cursor.row.saturating_add(count.max(1)).min(bottom);
        self.presentation_modes.right_margin_graphic = false;
    }

    fn set_scroll_region(&mut self, params: &[u16]) {
        let top = params.first().copied().unwrap_or(1).max(1) - 1;
        let bottom = params
            .get(1)
            .copied()
            .unwrap_or(self.size.rows)
            .max(1)
            .saturating_sub(1)
            .min(self.size.rows - 1);
        if top < bottom && top < self.size.rows {
            self.scroll_top = top;
            self.scroll_bottom = bottom;
            self.home_cursor();
        }
    }

    fn set_cursor(&mut self, row: u16, col: u16) {
        let (_, bottom) = self.vertical_bounds();
        self.cursor.row = if self.presentation_modes.origin_mode {
            self.scroll_top.saturating_add(row).min(bottom)
        } else {
            row.min(bottom)
        };
        self.cursor.col = col.min(self.size.cols - 1);
        self.presentation_modes.right_margin_graphic = false;
    }

    fn set_vertical_position(&mut self, row: u16) {
        let col = self.cursor.col;
        self.set_cursor(row, col);
    }

    fn reported_cursor_row(&self) -> u16 {
        let origin = if self.presentation_modes.origin_mode {
            self.scroll_top
        } else {
            0
        };
        self.cursor.row.saturating_sub(origin) + 1
    }

    fn feed_byte(&mut self, byte: u8, respond: &mut impl FnMut(&[u8])) {
        if self.utf8_expected > 0 {
            self.feed_utf8_continuation(byte, respond);
            return;
        }

        match self.parser.clone() {
            ParserState::Ground => self.feed_ground(byte),
            ParserState::Escape => self.feed_escape(byte),
            ParserState::Csi {
                params,
                current,
                private,
            } => self.feed_csi(byte, params, current, private, respond),
            ParserState::ControlString {
                allow_bel,
                escape_pending,
            } => self.feed_control_string(byte, allow_bel, escape_pending),
        }
    }

    fn feed_ground(&mut self, byte: u8) {
        match byte {
            0x1b => self.parser = ParserState::Escape,
            b'\r' => self.carriage_return(),
            b'\n' => self.line_feed(),
            b'\t' => self.horizontal_tab(1),
            0x08 => {
                self.cursor.col = self.cursor.col.saturating_sub(1);
                self.presentation_modes.right_margin_graphic = false;
            }
            0x20..=0x7e => self.put_char(char::from(byte)),
            0x80..=0xff => self.start_utf8(byte),
            _ => {}
        }
    }

    fn feed_escape(&mut self, byte: u8) {
        self.parser = ParserState::Ground;
        match byte {
            b'[' => {
                self.parser = ParserState::Csi {
                    params: Vec::new(),
                    current: None,
                    private: false,
                };
            }
            b']' => {
                self.parser = ParserState::ControlString {
                    allow_bel: true,
                    escape_pending: false,
                };
            }
            b'P' | b'X' | b'^' | b'_' => {
                self.parser = ParserState::ControlString {
                    allow_bel: false,
                    escape_pending: false,
                };
            }
            b'=' => self.input_modes.application_keypad = true,
            b'>' => self.input_modes.application_keypad = false,
            b'7' => self.save_cursor_state(),
            b'8' => self.restore_cursor_state(),
            b'D' => self.line_feed(),
            b'E' => {
                self.carriage_return();
                self.line_feed();
            }
            b'M' => self.reverse_index(),
            _ => {}
        }
    }

    fn feed_control_string(&mut self, byte: u8, allow_bel: bool, escape_pending: bool) {
        if allow_bel && byte == 0x07 {
            self.parser = ParserState::Ground;
            return;
        }
        if escape_pending && byte == b'\\' {
            self.parser = ParserState::Ground;
            return;
        }
        self.parser = ParserState::ControlString {
            allow_bel,
            escape_pending: byte == 0x1b,
        };
    }

    fn feed_csi(
        &mut self,
        byte: u8,
        mut params: Vec<u16>,
        mut current: Option<u16>,
        mut private: bool,
        respond: &mut impl FnMut(&[u8]),
    ) {
        match byte {
            b'?' if params.is_empty() && current.is_none() => private = true,
            b'0'..=b'9' => {
                let digit = u16::from(byte - b'0');
                current = Some(
                    current
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(digit),
                );
            }
            b';' => {
                if params.len() < MAX_CSI_PARAMS {
                    params.push(current.unwrap_or(0));
                }
                current = None;
            }
            0x40..=0x7e => {
                if (current.is_some() || !params.is_empty()) && params.len() < MAX_CSI_PARAMS {
                    params.push(current.unwrap_or(0));
                }
                self.parser = ParserState::Ground;
                self.execute_csi(byte, &params, private, respond);
                return;
            }
            _ => {
                self.parser = ParserState::Ground;
                return;
            }
        }
        self.parser = ParserState::Csi {
            params,
            current,
            private,
        };
    }

    fn execute_csi(
        &mut self,
        command: u8,
        params: &[u16],
        private: bool,
        respond: &mut impl FnMut(&[u8]),
    ) {
        let first = params.first().copied().unwrap_or(0);
        match (private, command) {
            (false, b'@') => self.insert_chars(first),
            (false, b'A') => self.move_cursor_up(first),
            (false, b'B') => self.move_cursor_down(first),
            (false, b'C') => {
                self.cursor.col = self
                    .cursor
                    .col
                    .saturating_add(first.max(1))
                    .min(self.size.cols - 1);
                self.presentation_modes.right_margin_graphic = false;
            }
            (false, b'D') => {
                self.cursor.col = self.cursor.col.saturating_sub(first.max(1));
                self.presentation_modes.right_margin_graphic = false;
            }
            (false, b'E') => {
                self.move_cursor_down(first);
                self.cursor.col = 0;
            }
            (false, b'F') => {
                self.move_cursor_up(first);
                self.cursor.col = 0;
            }
            (false, b'G') => {
                self.cursor.col = first.max(1).saturating_sub(1).min(self.size.cols - 1);
                self.presentation_modes.right_margin_graphic = false;
            }
            (false, b'H' | b'f') => {
                let row = params.first().copied().unwrap_or(1).max(1) - 1;
                let col = params.get(1).copied().unwrap_or(1).max(1) - 1;
                self.set_cursor(row, col);
            }
            (false, b'I') => self.horizontal_tab(first),
            (false, b'J') => {
                self.presentation_modes.right_margin_graphic = false;
                self.erase_display(first);
            }
            (false, b'K') => {
                self.presentation_modes.right_margin_graphic = false;
                self.erase_line(first);
            }
            (false, b'L') => self.insert_lines(first),
            (false, b'M') => self.delete_lines(first),
            (false, b'P') => self.delete_chars(first),
            (false, b'd') => self.set_vertical_position(first.max(1).saturating_sub(1)),
            (false, b'm') => self.apply_sgr(params),
            (false, b'n') if first == 5 => respond(b"\x1b[0n"),
            (false, b'n') if first == 6 => {
                let reply = format!(
                    "\x1b[{};{}R",
                    self.reported_cursor_row(),
                    self.cursor.col + 1
                );
                respond(reply.as_bytes());
            }
            (true, b'n') if first == 6 => {
                let reply = format!(
                    "\x1b[?{};{}R",
                    self.reported_cursor_row(),
                    self.cursor.col + 1
                );
                respond(reply.as_bytes());
            }
            (false, b'c') if first == 0 => respond(b"\x1b[?1;2c"),
            (false, b'r') => self.set_scroll_region(params),
            (true, b'h') => self.set_dec_private_modes(params, true),
            (true, b'l') => self.set_dec_private_modes(params, false),
            _ => {}
        }
    }

    fn set_dec_private_modes(&mut self, params: &[u16], enabled: bool) {
        for &param in params {
            match param {
                1 => self.input_modes.application_cursor_keys = enabled,
                6 => {
                    self.presentation_modes.origin_mode = enabled;
                    self.home_cursor();
                }
                7 => {
                    self.presentation_modes.auto_wrap = enabled;
                    if !enabled {
                        self.cursor.col = self.cursor.col.min(self.size.cols - 1);
                        self.saved_cursor_state.cursor.col =
                            self.saved_cursor_state.cursor.col.min(self.size.cols - 1);
                        self.alternate_entry_state.cursor.col =
                            self.alternate_entry_state.cursor.col.min(self.size.cols - 1);
                    }
                    self.presentation_modes.right_margin_graphic = false;
                }
                25 => self.cursor_visible = enabled,
                1049 if enabled => self.enter_alternate_screen(),
                1049 => self.leave_alternate_screen(),
                2004 => self.input_modes.bracketed_paste = enabled,
                _ => {}
            }
        }
    }

    fn apply_sgr(&mut self, params: &[u16]) {
        let effective = if params.is_empty() { &[0][..] } else { params };
        let mut index = 0;
        while index < effective.len() {
            let param = effective[index];
            match param {
                0 => self.style = CellStyle::default(),
                1 => self.style.attributes.set(CellAttributes::BOLD, true),
                3 => self.style.attributes.set(CellAttributes::ITALIC, true),
                4 => self.style.attributes.set(CellAttributes::UNDERLINE, true),
                7 => self.style.attributes.set(CellAttributes::INVERSE, true),
                22 => self.style.attributes.set(CellAttributes::BOLD, false),
                23 => self.style.attributes.set(CellAttributes::ITALIC, false),
                24 => self.style.attributes.set(CellAttributes::UNDERLINE, false),
                27 => self.style.attributes.set(CellAttributes::INVERSE, false),
                30..=37 => {
                    self.style.foreground =
                        TerminalColor::Indexed(u8::try_from(param - 30).unwrap_or(0));
                }
                38 => {
                    index += self.apply_extended_color(&effective[index..], true);
                    continue;
                }
                39 => self.style.foreground = TerminalColor::Default,
                40..=47 => {
                    self.style.background =
                        TerminalColor::Indexed(u8::try_from(param - 40).unwrap_or(0));
                }
                48 => {
                    index += self.apply_extended_color(&effective[index..], false);
                    continue;
                }
                49 => self.style.background = TerminalColor::Default,
                90..=97 => {
                    self.style.foreground =
                        TerminalColor::Indexed(u8::try_from(param - 90 + 8).unwrap_or(8));
                }
                100..=107 => {
                    self.style.background =
                        TerminalColor::Indexed(u8::try_from(param - 100 + 8).unwrap_or(8));
                }
                _ => {}
            }
            index += 1;
        }
    }

    fn apply_extended_color(&mut self, params: &[u16], foreground: bool) -> usize {
        let Some(mode) = params.get(1).copied() else {
            return params.len();
        };

        match mode {
            5 => {
                let Some(value) = params.get(2).copied() else {
                    return params.len();
                };
                if let Ok(value) = u8::try_from(value) {
                    self.set_color(foreground, TerminalColor::Indexed(value));
                }
                3.min(params.len())
            }
            2 => {
                if params.len() < 5 {
                    return params.len();
                }
                let red = u8::try_from(params[2]);
                let green = u8::try_from(params[3]);
                let blue = u8::try_from(params[4]);
                if let (Ok(r), Ok(g), Ok(b)) = (red, green, blue) {
                    self.set_color(foreground, TerminalColor::Rgb { r, g, b });
                }
                5
            }
            _ => 2.min(params.len()),
        }
    }

    fn set_color(&mut self, foreground: bool, color: TerminalColor) {
        if foreground {
            self.style.foreground = color;
        } else {
            self.style.background = color;
        }
    }

    fn enter_alternate_screen(&mut self) {
        if self.alternate_active {
            return;
        }
        self.alternate_entry_state = self.current_cursor_state();
        let blank = self.erase_cell();
        self.alternate.fill(blank);
        self.alternate_active = true;
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.cursor = Cursor::default();
        self.presentation_modes.right_margin_graphic = false;
    }

    fn leave_alternate_screen(&mut self) {
        if !self.alternate_active {
            return;
        }
        self.alternate_active = false;
        self.scroll_top = 0;
        self.scroll_bottom = self.size.rows - 1;
        self.restore_saved_cursor_state(self.alternate_entry_state);
    }

    fn start_utf8(&mut self, byte: u8) {
        self.utf8_expected = match byte {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                self.put_char('\u{fffd}');
                return;
            }
        };
        self.utf8[0] = byte;
        self.utf8_len = 1;
    }

    fn feed_utf8_continuation(&mut self, byte: u8, respond: &mut impl FnMut(&[u8])) {
        if !(0x80..=0xbf).contains(&byte) {
            self.utf8_expected = 0;
            self.utf8_len = 0;
            self.put_char('\u{fffd}');
            self.feed_byte(byte, respond);
            return;
        }
        if self.utf8_len < self.utf8.len() {
            self.utf8[self.utf8_len] = byte;
            self.utf8_len += 1;
        }
        if self.utf8_len == self.utf8_expected {
            let character = std::str::from_utf8(&self.utf8[..self.utf8_len])
                .ok()
                .and_then(|value| value.chars().next())
                .unwrap_or('\u{fffd}');
            self.utf8_expected = 0;
            self.utf8_len = 0;
            self.put_char(character);
        }
    }
}

fn printable_character(character: char) -> char {
    if character == WIDE_CONTINUATION {
        ' '
    } else {
        character
    }
}

fn line_text(cells: &[Cell]) -> String {
    let extra = cells
        .iter()
        .map(|cell| cell.combining_marks().len())
        .sum::<usize>();
    let mut text = String::with_capacity(cells.len() + extra);
    for cell in cells {
        text.push(cell.display_character());
        text.extend(cell.combining_marks());
    }
    text
}

fn normalize_wide_grid(cells: &mut [Cell], size: EndpointSize) {
    normalize_wide_grid_with_blank(cells, size, Cell::default());
}

fn normalize_wide_grid_with_blank(cells: &mut [Cell], size: EndpointSize, blank: Cell) {
    let width = usize::from(size.cols);
    for row in cells.chunks_exact_mut(width) {
        normalize_wide_row_with_blank(row, blank);
    }
}

fn normalize_wide_row(row: &mut [Cell]) {
    normalize_wide_row_with_blank(row, Cell::default());
}

fn normalize_wide_row_with_blank(row: &mut [Cell], blank: Cell) {
    for index in 0..row.len() {
        if row[index].character == WIDE_CONTINUATION
            && (index == 0 || row[index - 1].sequence_width() != 2)
        {
            row[index] = blank;
        }
        if row[index].sequence_width() == 2
            && (index + 1 == row.len() || row[index + 1].character != WIDE_CONTINUATION)
        {
            row[index] = blank;
        }
    }
}

fn terminal_cell_width(character: char) -> u16 {
    match character.width() {
        Some(0) | None => 0,
        Some(1) => 1,
        Some(_) => 2,
    }
}

fn terminal_string_width(value: &str) -> u16 {
    match value.width() {
        0 => 0,
        1 => 1,
        _ => 2,
    }
}

fn is_variation_selector(character: char) -> bool {
    matches!(character, '\u{fe0e}' | '\u{fe0f}')
}

fn is_emoji_modifier(character: char) -> bool {
    ('\u{1f3fb}'..='\u{1f3ff}').contains(&character)
}

fn is_regional_indicator(character: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&character)
}

fn is_emoji_sequence_scalar(character: char) -> bool {
    character == ZERO_WIDTH_JOINER
        || character == '\u{20e3}'
        || is_variation_selector(character)
        || is_emoji_modifier(character)
}

fn cell_sequence_width(cell: &Cell, extra: Option<char>) -> u16 {
    if cell.character == WIDE_CONTINUATION {
        return 0;
    }

    let mut bytes = [0_u8; MAX_CELL_SEQUENCE_BYTES];
    let mut len = 0;
    for character in std::iter::once(cell.character)
        .chain(cell.combining_marks().iter().copied())
        .chain(extra)
    {
        let encoded = character.encode_utf8(&mut bytes[len..]);
        len += encoded.len();
    }
    let value = std::str::from_utf8(&bytes[..len]).unwrap_or("\u{fffd}");
    terminal_string_width(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal(cols: u16, rows: u16) -> TerminalState {
        TerminalState::new(EndpointSize::new(cols, rows).unwrap(), 2)
    }

    #[test]
    fn writes_text_and_honors_cursor_positioning() {
        let mut terminal = terminal(8, 3);
        terminal.feed(b"abc\x1b[2;3HZ");
        assert_eq!(terminal.line_text(0).unwrap(), "abc     ");
        assert_eq!(terminal.cell(1, 2).unwrap().character, 'Z');
    }

    #[test]
    fn sgr_style_is_stored_on_cells() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"\x1b[1;31mX\x1b[0mY");
        let styled = terminal.cell(0, 0).unwrap();
        assert!(styled.style.attributes.contains(CellAttributes::BOLD));
        assert_eq!(styled.style.foreground, TerminalColor::Indexed(1));
        assert_eq!(terminal.cell(0, 1).unwrap().style, CellStyle::default());
    }

    #[test]
    fn application_keypad_tracks_deckpam_and_deckpnm_independently() {
        let mut terminal = terminal(4, 2);
        assert!(!terminal.application_keypad_mode());
        assert!(!terminal.application_cursor_keys());

        terminal.feed(b"\x1b=");
        assert!(terminal.application_keypad_mode());
        assert!(!terminal.application_cursor_keys());

        terminal.feed(b"\x1b[?1h");
        assert!(terminal.application_keypad_mode());
        assert!(terminal.application_cursor_keys());

        terminal.feed(b"\x1b>");
        assert!(!terminal.application_keypad_mode());
        assert!(terminal.application_cursor_keys());
    }

    #[test]
    fn sgr_emphasis_attributes_are_stored_and_reset_independently() {
        let mut terminal = terminal(8, 2);
        terminal.feed(b"\x1b[1;3;4;7;38;5;201mA\x1b[23mB\x1b[24;27mC\x1b[22mD");

        let first = terminal.cell(0, 0).unwrap().style;
        assert!(first.attributes.contains(CellAttributes::BOLD));
        assert!(first.attributes.contains(CellAttributes::ITALIC));
        assert!(first.attributes.contains(CellAttributes::UNDERLINE));
        assert!(first.attributes.contains(CellAttributes::INVERSE));
        assert_eq!(first.foreground, TerminalColor::Indexed(201));

        let second = terminal.cell(0, 1).unwrap().style;
        assert!(second.attributes.contains(CellAttributes::BOLD));
        assert!(!second.attributes.contains(CellAttributes::ITALIC));
        assert!(second.attributes.contains(CellAttributes::UNDERLINE));
        assert!(second.attributes.contains(CellAttributes::INVERSE));
        assert_eq!(second.foreground, TerminalColor::Indexed(201));

        let third = terminal.cell(0, 2).unwrap().style;
        assert!(third.attributes.contains(CellAttributes::BOLD));
        assert!(!third.attributes.contains(CellAttributes::ITALIC));
        assert!(!third.attributes.contains(CellAttributes::UNDERLINE));
        assert!(!third.attributes.contains(CellAttributes::INVERSE));
        assert_eq!(third.foreground, TerminalColor::Indexed(201));

        let fourth = terminal.cell(0, 3).unwrap().style;
        assert_eq!(
            fourth,
            CellStyle {
                foreground: TerminalColor::Indexed(201),
                ..CellStyle::default()
            }
        );
    }

    #[test]
    fn saved_cursor_and_alternate_screen_restore_graphic_rendition() {
        let mut terminal = terminal(8, 2);
        terminal.feed(b"\x1b[3;4;7;38;5;201m\x1b7\x1b[0m\x1b8A");

        let restored = terminal.cell(0, 0).unwrap().style;
        assert!(restored.attributes.contains(CellAttributes::ITALIC));
        assert!(restored.attributes.contains(CellAttributes::UNDERLINE));
        assert!(restored.attributes.contains(CellAttributes::INVERSE));
        assert_eq!(restored.foreground, TerminalColor::Indexed(201));

        terminal.feed(b"\x1b[0;1;38;2;1;2;3m\x1b[?1049h\x1b[0mX\x1b[?1049lB");
        let alternate_restored = terminal.cell(0, 1).unwrap().style;
        assert!(alternate_restored.attributes.contains(CellAttributes::BOLD));
        assert_eq!(
            alternate_restored.foreground,
            TerminalColor::Rgb { r: 1, g: 2, b: 3 }
        );
    }

    #[test]
    fn saved_cursor_restores_origin_mode_in_both_directions() {
        let mut origin_on = terminal(8, 6);
        origin_on.feed(b"\x1b[2;5r\x1b[?6h\x1b[3;4H\x1b7\x1b[?6l\x1b[6;8H\x1b8");
        assert_eq!(origin_on.cursor(), Cursor { row: 3, col: 3 });
        origin_on.feed(b"\x1b[1;1H");
        assert_eq!(origin_on.cursor(), Cursor { row: 1, col: 0 });

        let mut origin_off = terminal(8, 6);
        origin_off.feed(b"\x1b[2;5r\x1b[4;3H\x1b7\x1b[?6h\x1b[2;2H\x1b8");
        assert_eq!(origin_off.cursor(), Cursor { row: 3, col: 2 });
        origin_off.feed(b"\x1b[1;1H");
        assert_eq!(origin_off.cursor(), Cursor { row: 0, col: 0 });
    }

    #[test]
    fn saved_origin_mode_clamps_restored_cursor_to_current_margins() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[2;6r\x1b[?6h\x1b[5;4H\x1b7\x1b[2;3r\x1b8");
        assert_eq!(terminal.cursor(), Cursor { row: 2, col: 3 });
        terminal.feed(b"\x1b[1;1H");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });
    }

    #[test]
    fn alternate_screen_restores_origin_mode_with_saved_cursor_state() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[2;5r\x1b[?6h\x1b[3;4H\x1b[?1049h\x1b[?6l\x1b[6;8H\x1b[?1049l");
        assert_eq!(terminal.cursor(), Cursor { row: 3, col: 3 });

        terminal.feed(b"\x1b[3;5r");
        assert_eq!(terminal.cursor(), Cursor { row: 2, col: 0 });
    }

    #[test]
    fn saved_cursor_restore_preserves_delayed_wrap_position() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"abcd\x1b7\x1b[H\x1b8X");

        assert_eq!(terminal.line_text(0).unwrap(), "abcd");
        assert_eq!(terminal.cell(1, 0).unwrap().character, 'X');
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 1 });
    }

    #[test]
    fn sgr_zero_resets_emphasis_and_colors() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"\x1b[3;4;7;38;2;1;2;3;48;5;42mX\x1b[0mY");
        let styled = terminal.cell(0, 0).unwrap().style;
        assert!(styled.attributes.contains(CellAttributes::ITALIC));
        assert!(styled.attributes.contains(CellAttributes::UNDERLINE));
        assert!(styled.attributes.contains(CellAttributes::INVERSE));
        assert_eq!(styled.foreground, TerminalColor::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(styled.background, TerminalColor::Indexed(42));
        assert_eq!(terminal.cell(0, 1).unwrap().style, CellStyle::default());
    }

    #[test]
    fn emphasis_is_cell_state_across_alternate_screen_and_overwrite() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"\x1b[3mA\x1b[?1049h\x1b[4;7mB");
        let alternate = terminal.cell(0, 0).unwrap().style;
        assert!(alternate.attributes.contains(CellAttributes::ITALIC));
        assert!(alternate.attributes.contains(CellAttributes::UNDERLINE));
        assert!(alternate.attributes.contains(CellAttributes::INVERSE));

        terminal.feed(b"\x1b[?1049l\x1b[H\x1b[23;24;27mC");
        let overwritten = terminal.cell(0, 0).unwrap().style;
        assert!(!overwritten.attributes.contains(CellAttributes::ITALIC));
        assert!(!overwritten.attributes.contains(CellAttributes::UNDERLINE));
        assert!(!overwritten.attributes.contains(CellAttributes::INVERSE));
    }

    #[test]
    fn sgr_bright_indexed_colors_are_stored_without_bold_inference() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"\x1b[91;104mX");
        let styled = terminal.cell(0, 0).unwrap();
        assert!(!styled.style.attributes.contains(CellAttributes::BOLD));
        assert_eq!(styled.style.foreground, TerminalColor::Indexed(9));
        assert_eq!(styled.style.background, TerminalColor::Indexed(12));
    }

    #[test]
    fn sgr_extended_indexed_and_rgb_colors_are_stored_and_reset() {
        let mut terminal = terminal(5, 2);
        terminal.feed(b"\x1b[38;5;196;48;2;1;2;3mX\x1b[39;49mY");

        let styled = terminal.cell(0, 0).unwrap();
        assert_eq!(styled.style.foreground, TerminalColor::Indexed(196));
        assert_eq!(
            styled.style.background,
            TerminalColor::Rgb { r: 1, g: 2, b: 3 }
        );
        assert_eq!(
            terminal.cell(0, 1).unwrap().style.foreground,
            TerminalColor::Default
        );
        assert_eq!(
            terminal.cell(0, 1).unwrap().style.background,
            TerminalColor::Default
        );
    }

    #[test]
    fn malformed_extended_colors_do_not_reinterpret_channels_as_attributes() {
        let mut terminal = terminal(6, 2);
        terminal.feed(b"\x1b[1;38;2;255;0mA\x1b[38;5;999;31mB");

        let first = terminal.cell(0, 0).unwrap();
        assert!(first.style.attributes.contains(CellAttributes::BOLD));
        assert_eq!(first.style.foreground, TerminalColor::Default);

        let second = terminal.cell(0, 1).unwrap();
        assert!(second.style.attributes.contains(CellAttributes::BOLD));
        assert_eq!(second.style.foreground, TerminalColor::Indexed(1));
    }

    #[test]
    fn mixed_extended_colors_preserve_following_attributes() {
        let mut terminal = terminal(5, 2);
        terminal.feed(b"\x1b[1;38;5;201;48;2;4;5;6;22mX");
        let styled = terminal.cell(0, 0).unwrap();
        assert!(!styled.style.attributes.contains(CellAttributes::BOLD));
        assert_eq!(styled.style.foreground, TerminalColor::Indexed(201));
        assert_eq!(
            styled.style.background,
            TerminalColor::Rgb { r: 4, g: 5, b: 6 }
        );
    }

    #[test]
    fn background_color_erase_keeps_only_the_current_background() {
        let mut terminal = terminal(6, 3);
        terminal.feed(b"abcdef\x1b[2;1Hghijkl\x1b[3;1Hmnopqr");
        terminal.feed(b"\x1b[1;3;4;7;31;48;5;42m\x1b[2;3H\x1b[K");

        let erased = terminal.cell(1, 2).unwrap();
        assert_eq!(erased.character, ' ');
        assert_eq!(
            erased.style,
            CellStyle {
                background: TerminalColor::Indexed(42),
                ..CellStyle::default()
            }
        );
        assert_eq!(terminal.cell(1, 1).unwrap().character, 'h');

        terminal.feed(b"\x1b[48;2;1;2;3m\x1b[2;2H\x1b[1J");
        let rgb_erased = terminal.cell(0, 0).unwrap();
        assert_eq!(
            rgb_erased.style,
            CellStyle {
                background: TerminalColor::Rgb { r: 1, g: 2, b: 3 },
                ..CellStyle::default()
            }
        );
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 1 });
    }

    #[test]
    fn sgr_49_restores_default_background_for_erasure() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"xxxx\x1b[48;5;99m\x1b[H\x1b[2K");
        assert_eq!(
            terminal.cell(0, 0).unwrap().style.background,
            TerminalColor::Indexed(99)
        );

        terminal.feed(b"\x1b[49m\x1b[2;1H\x1b[2K");
        assert_eq!(terminal.cell(1, 0).unwrap(), Cell::default());
    }

    #[test]
    fn background_erase_applies_to_character_and_line_edits_and_scrolling() {
        let mut terminal = terminal(5, 4);
        terminal.feed(b"aaaaa\x1b[2;1Hbbbbb\x1b[3;1Hccccc\x1b[4;1Hddddd");
        terminal.feed(b"\x1b[48;5;17m\x1b[2;3H\x1b[2@\x1b[P");
        assert_eq!(
            terminal.cell(1, 2).unwrap().style.background,
            TerminalColor::Indexed(17)
        );
        assert_eq!(
            terminal.cell(1, 4).unwrap().style.background,
            TerminalColor::Indexed(17)
        );

        terminal.feed(b"\x1b[2;4r\x1b[3;1H\x1b[L");
        assert_eq!(
            terminal.cell(2, 0).unwrap().style.background,
            TerminalColor::Indexed(17)
        );
        assert_eq!(terminal.line_text(0).unwrap(), "aaaaa");

        terminal.feed(b"\x1b[4;1H\n");
        assert_eq!(
            terminal.cell(3, 0).unwrap().style.background,
            TerminalColor::Indexed(17)
        );
        assert_eq!(terminal.line_text(0).unwrap(), "aaaaa");
    }

    #[test]
    fn background_erasure_normalizes_wide_cells_without_orphans() {
        let mut terminal = terminal(5, 2);
        terminal.feed("A界B".as_bytes());
        terminal.feed(b"\x1b[48;5;33m\x1b[1;3H\x1b[K");

        assert_eq!(terminal.cell(0, 1).unwrap().character, ' ');
        assert_eq!(terminal.cell(0, 2).unwrap().character, ' ');
        assert_eq!(
            terminal.cell(0, 1).unwrap().style.background,
            TerminalColor::Indexed(33)
        );
        assert_eq!(
            terminal.cell(0, 2).unwrap().style.background,
            TerminalColor::Indexed(33)
        );
        assert!(
            (0..terminal.size().cols)
                .all(|col| terminal.cell(0, col).unwrap().character != WIDE_CONTINUATION)
        );
    }

    #[test]
    fn background_erase_is_independent_across_primary_and_alternate_screens() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"main\x1b[48;5;21m\x1b[?1049h");
        assert!(terminal.alternate_screen_active());
        assert_eq!(
            terminal.cell(0, 0).unwrap().style.background,
            TerminalColor::Indexed(21)
        );

        terminal.feed(b"\x1b[48;2;9;8;7m\x1b[2J");
        assert_eq!(
            terminal.cell(1, 3).unwrap().style.background,
            TerminalColor::Rgb { r: 9, g: 8, b: 7 }
        );
        terminal.feed(b"\x1b[?1049l");
        assert_eq!(terminal.line_text(0).unwrap(), "main");
    }

    #[test]
    fn alternate_screen_does_not_destroy_primary_contents() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"main\x1b[?1049halt\x1b[?1049l");
        assert!(!terminal.alternate_screen_active());
        assert_eq!(terminal.line_text(0).unwrap(), "main");
    }

    #[test]
    fn input_modes_track_set_reset_and_combined_private_parameters() {
        let mut terminal = terminal(4, 2);
        assert!(!terminal.application_cursor_keys());
        assert!(!terminal.bracketed_paste_mode());

        terminal.feed(b"\x1b[?1;2004h");
        assert!(terminal.application_cursor_keys());
        assert!(terminal.bracketed_paste_mode());

        terminal.feed(b"\x1b[?1l");
        assert!(!terminal.application_cursor_keys());
        assert!(terminal.bracketed_paste_mode());

        terminal.feed(b"\x1b[?2004l");
        assert!(!terminal.bracketed_paste_mode());
    }

    #[test]
    fn auto_wrap_defaults_on_and_tracks_combined_private_modes() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"abcd\x1b[?1;7;2004lZ");

        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
        assert_eq!(terminal.line_text(0).unwrap(), "abcZ");
        assert_eq!(terminal.line_text(1).unwrap(), "    ");
        assert!(!terminal.application_cursor_keys());
        assert!(!terminal.bracketed_paste_mode());

        terminal.feed(b"\x1b[?1;7;2004hQ");
        assert!(terminal.application_cursor_keys());
        assert!(terminal.bracketed_paste_mode());
        assert_eq!(terminal.line_text(0).unwrap(), "abcQ");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 4 });

        terminal.feed(b"R");
        assert_eq!(terminal.cell(1, 0).unwrap().character, 'R');
    }

    #[test]
    fn auto_wrap_disabled_replaces_rightmost_cell_without_scrolling() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"\x1b[?7labcXYZ");

        assert_eq!(terminal.line_text(0).unwrap(), "abcZ");
        assert_eq!(terminal.line_text(1).unwrap(), "    ");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
        assert_eq!(terminal.scrollback_len(), 0);
    }

    #[test]
    fn auto_wrap_disabled_retains_combining_marks_on_the_rightmost_base() {
        let mut terminal = terminal(4, 2);
        terminal.feed("\u{1b}[?7labcX\u{0301}".as_bytes());

        assert_eq!(terminal.cell(0, 3).unwrap().character, 'X');
        assert_eq!(
            terminal.cell(0, 3).unwrap().combining_marks(),
            &['\u{0301}']
        );
        assert_eq!(terminal.cell(0, 2).unwrap().combining_marks(), &[]);
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn auto_wrap_disabled_combining_after_near_margin_write_uses_actual_base() {
        let mut terminal = terminal(4, 2);
        terminal.feed("\u{1b}[?7labX\u{0301}".as_bytes());

        assert_eq!(terminal.cell(0, 2).unwrap().character, 'X');
        assert_eq!(
            terminal.cell(0, 2).unwrap().combining_marks(),
            &['\u{0301}']
        );
        assert_eq!(terminal.cell(0, 3).unwrap().combining_marks(), &[]);
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn auto_wrap_disabled_retains_combining_marks_in_one_column_terminal() {
        let mut terminal = terminal(1, 1);
        terminal.feed("\u{1b}[?7lX\u{0301}".as_bytes());

        assert_eq!(terminal.cell(0, 0).unwrap().character, 'X');
        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{0301}']
        );
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 0 });
    }

    #[test]
    fn explicit_cursor_motion_clears_right_margin_combining_anchor() {
        let mut terminal = terminal(4, 2);
        terminal.feed("\u{1b}[?7labcX\u{1b}[D\u{0301}".as_bytes());

        assert_eq!(terminal.cell(0, 3).unwrap().combining_marks(), &[]);
        assert_eq!(
            terminal.cell(0, 1).unwrap().combining_marks(),
            &['\u{0301}']
        );
    }

    #[test]
    fn disabling_auto_wrap_clamps_saved_cursor_before_restore_and_reporting() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"abcd\x1b[?1049h\x1b[?7l\x1b[?1049l");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });

        let mut replies = Vec::new();
        terminal.feed_with_responses(b"\x1b[6n", |reply| replies.push(reply.to_vec()));
        assert_eq!(replies, vec![b"\x1b[1;4R".to_vec()]);
    }

    #[test]
    fn auto_wrap_disabled_does_not_scroll_at_scroll_region_bottom() {
        let mut terminal = terminal(4, 3);
        terminal.feed(b"top\x1b[2;3r\x1b[3;4H\x1b[?7lXYZ");

        assert_eq!(terminal.line_text(0).unwrap(), "top ");
        assert_eq!(terminal.line_text(1).unwrap(), "    ");
        assert_eq!(terminal.line_text(2).unwrap(), "   Z");
        assert_eq!(terminal.cursor(), Cursor { row: 2, col: 3 });
        assert_eq!(terminal.scrollback_len(), 0);
    }

    #[test]
    fn auto_wrap_disabled_handles_wide_characters_without_orphan_cells() {
        let mut terminal = terminal(4, 2);
        terminal.feed("\u{1b}[?7lab界".as_bytes());
        assert_eq!(terminal.cell(0, 2).unwrap().character, '界');
        assert_eq!(terminal.cell(0, 3).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });

        terminal.feed("界".as_bytes());
        assert_eq!(terminal.cell(0, 2).unwrap(), Cell::default());
        assert_eq!(terminal.cell(0, 3).unwrap().character, '\u{fffd}');
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
        assert_eq!(terminal.line_text(1).unwrap(), "    ");
    }

    #[test]
    fn auto_wrap_mode_survives_resize_and_alternate_screen_transitions() {
        let mut terminal = terminal(5, 2);
        terminal.feed(b"\x1b[?7l\x1b[?1049h12345X");
        assert_eq!(terminal.line_text(0).unwrap(), "1234X");

        terminal.resize(EndpointSize::new(3, 2).unwrap());
        terminal.feed(b"Y");
        assert_eq!(terminal.line_text(0).unwrap(), "12Y");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 2 });

        terminal.feed(b"\x1b[?1049l\x1b[1;3HZ");
        assert_eq!(terminal.line_text(0).unwrap(), "  Z");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn alternate_screen_does_not_fabricate_input_mode_changes() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"\x1b=\x1b[?1;2004h\x1b[?1049h");
        assert!(terminal.application_keypad_mode());
        assert!(terminal.application_cursor_keys());
        assert!(terminal.bracketed_paste_mode());
        assert!(terminal.alternate_screen_active());

        terminal.feed(b"\x1b[?1049l");
        assert!(terminal.application_keypad_mode());
        assert!(terminal.application_cursor_keys());
        assert!(terminal.bracketed_paste_mode());
        assert!(!terminal.alternate_screen_active());
    }

    #[test]
    fn scrolling_is_bounded_and_alt_screen_does_not_enter_scrollback() {
        let mut terminal = terminal(3, 2);
        terminal.feed(b"a\r\nb\r\nc\r\nd");
        assert_eq!(terminal.scrollback_len(), 2);
        terminal.feed(b"\x1b[?1049hx\r\ny\r\nz");
        assert_eq!(terminal.scrollback_len(), 2);
    }

    #[test]
    fn scroll_region_preserves_rows_outside_margins() {
        let mut terminal = terminal(4, 4);
        terminal.feed(b"top\x1b[2;1Hmid1\x1b[3;1Hmid2\x1b[4;1Hbot");
        terminal.feed(b"\x1b[2;3r\x1b[3;1H\n");

        assert_eq!(terminal.line_text(0).unwrap(), "top ");
        assert_eq!(terminal.line_text(1).unwrap(), "mid2");
        assert_eq!(terminal.line_text(2).unwrap(), "    ");
        assert_eq!(terminal.line_text(3).unwrap(), "bot ");
        assert_eq!(terminal.scrollback_len(), 0);
    }

    #[test]
    fn line_feed_below_scroll_region_at_last_row_does_not_scroll_outside_margin() {
        // Regression test: a scroll region smaller than the full screen, combined
        // with the cursor parked below it (via CUP) on the physical last row, used
        // to trigger a full-screen scroll on line feed -- corrupting rows outside
        // the margin and even pushing an unrelated row into scrollback. Per
        // DECSTBM semantics (and consistent with insert_lines/delete_lines, which
        // already no-op when the cursor is outside the region), this must be a
        // no-op: the cursor is already on the last row and outside the region.
        let mut terminal = terminal(4, 4);
        terminal.feed(b"top\x1b[2;1Hmid1\x1b[3;1Hmid2\x1b[4;1Hbot");
        terminal.feed(b"\x1b[1;2r\x1b[4;1H\n");

        assert_eq!(terminal.line_text(0).unwrap(), "top ");
        assert_eq!(terminal.line_text(1).unwrap(), "mid1");
        assert_eq!(terminal.line_text(2).unwrap(), "mid2");
        assert_eq!(terminal.line_text(3).unwrap(), "bot ");
        assert_eq!(terminal.cursor(), Cursor { row: 3, col: 0 });
        assert_eq!(terminal.scrollback_len(), 0);
    }

    #[test]
    fn origin_mode_homes_and_addresses_relative_to_scroll_region() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[2;5r");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 0 });

        terminal.feed(b"\x1b[6;8H\x1b[?6h");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });

        terminal.feed(b"\x1b[3;4H");
        assert_eq!(terminal.cursor(), Cursor { row: 3, col: 3 });
        terminal.feed(b"\x1b[9;2f");
        assert_eq!(terminal.cursor(), Cursor { row: 4, col: 1 });
        terminal.feed(b"\x1b[2d");
        assert_eq!(terminal.cursor(), Cursor { row: 2, col: 1 });

        terminal.feed(b"\x1b[?6l");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 0 });
        terminal.feed(b"\x1b[6d");
        assert_eq!(terminal.cursor(), Cursor { row: 5, col: 0 });
    }

    #[test]
    fn origin_mode_clamps_relative_vertical_cursor_motion() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[2;5r\x1b[?6h\x1b[99B");
        assert_eq!(terminal.cursor(), Cursor { row: 4, col: 0 });
        terminal.feed(b"\x1b[99A");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });

        terminal.feed(b"\x1b[3;4H\x1b[99E");
        assert_eq!(terminal.cursor(), Cursor { row: 4, col: 0 });
        terminal.feed(b"\x1b[99F");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });
    }

    #[test]
    fn scroll_region_homes_according_to_origin_mode_and_combined_modes() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[2;5r\x1b[?1;6;7h");
        assert!(terminal.application_cursor_keys());
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });

        terminal.feed(b"\x1b[3;6r");
        assert_eq!(terminal.cursor(), Cursor { row: 2, col: 0 });
        terminal.feed(b"\x1b[?6l\x1b[2;4r");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 0 });
    }

    #[test]
    fn origin_mode_survives_resize_and_alternate_screen_transitions() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[2;5r\x1b[?6h\x1b[?1049h\x1b[2;3H");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 2 });

        terminal.feed(b"\x1b[?1049l");
        terminal.resize(EndpointSize::new(6, 5).unwrap());
        terminal.feed(b"\x1b[2;4r");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 0 });
        terminal.feed(b"\x1b[99;6H");
        assert_eq!(terminal.cursor(), Cursor { row: 3, col: 5 });
    }

    #[test]
    fn cursor_reports_use_origin_relative_rows() {
        let mut terminal = terminal(8, 6);
        terminal.feed(b"\x1b[3;6r\x1b[?6h\x1b[2;4H");
        assert_eq!(terminal.cursor(), Cursor { row: 3, col: 3 });

        let mut replies = Vec::new();
        terminal.feed_with_responses(b"\x1b[6n\x1b[?6n", |reply| replies.push(reply.to_vec()));
        assert_eq!(
            replies,
            vec![b"\x1b[2;4R".to_vec(), b"\x1b[?2;4R".to_vec()]
        );
    }

    #[test]
    fn erase_display_and_line_variants_do_not_move_cursor() {
        let mut terminal = terminal(5, 3);
        terminal.feed(b"abcde\x1b[2;1Hfghij\x1b[3;1Hklmno\x1b[2;3H\x1b[1K");
        assert_eq!(terminal.line_text(1).unwrap(), "   ij");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 2 });

        terminal.feed(b"\x1b[0J");
        assert_eq!(terminal.line_text(1).unwrap(), "     ");
        assert_eq!(terminal.line_text(2).unwrap(), "     ");
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 2 });
    }

    #[test]
    fn insert_and_delete_character_sequences_edit_current_row() {
        let mut terminal = terminal(6, 2);
        terminal.feed(b"abcdef\x1b[1;3H\x1b[2@XY");
        assert_eq!(terminal.line_text(0).unwrap(), "abXYcd");
        terminal.feed(b"\x1b[1;3H\x1b[2P");
        assert_eq!(terminal.line_text(0).unwrap(), "abcd  ");
    }

    #[test]
    fn insert_and_delete_lines_respect_scroll_region() {
        let mut terminal = terminal(4, 4);
        terminal.feed(b"aaaa\x1b[2;1Hbbbb\x1b[3;1Hcccc\x1b[4;1Hdddd");
        terminal.feed(b"\x1b[2;4r\x1b[3;1H\x1b[L");
        assert_eq!(terminal.line_text(0).unwrap(), "aaaa");
        assert_eq!(terminal.line_text(1).unwrap(), "bbbb");
        assert_eq!(terminal.line_text(2).unwrap(), "    ");
        assert_eq!(terminal.line_text(3).unwrap(), "cccc");
        terminal.feed(b"\x1b[M");
        assert_eq!(terminal.line_text(2).unwrap(), "cccc");
    }

    #[test]
    fn cursor_visibility_tracks_dectcem() {
        let mut terminal = terminal(4, 2);
        assert!(terminal.cursor_visible());
        terminal.feed(b"\x1b[?25l");
        assert!(!terminal.cursor_visible());
        terminal.feed(b"\x1b[?25h");
        assert!(terminal.cursor_visible());
    }

    #[test]
    fn primary_device_attributes_are_answered_directly() {
        let mut terminal = terminal(4, 2);
        let mut replies = Vec::new();
        terminal.feed_with_responses(b"\x1b[c", |reply| replies.push(reply.to_vec()));
        assert_eq!(replies, vec![b"\x1b[?1;2c".to_vec()]);
    }

    #[test]
    fn csi_parameter_storage_is_hard_bounded() {
        let mut terminal = terminal(4, 2);
        let mut sequence = b"\x1b[".to_vec();
        sequence.extend(std::iter::repeat_n(b';', 10_000));
        sequence.push(b'm');
        terminal.feed(&sequence);
        assert!(matches!(terminal.parser, ParserState::Ground));
    }

    #[test]
    fn osc_payloads_are_consumed_for_bel_and_st_terminators() {
        let mut terminal = terminal(8, 2);
        terminal.feed(b"A\x1b]0;secret\x07B\x1b]2;hidden\x1b\\C");

        assert_eq!(terminal.line_text(0).unwrap(), "ABC     ");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn control_strings_are_consumed_across_chunk_boundaries() {
        let mut terminal = terminal(8, 2);
        terminal.feed(b"A\x1b]0;title\x1b");
        terminal.feed(b"\\B\x1bPpayload\x1b");
        terminal.feed(b"\\C\x1bXignored\x1b\\D\x1b^private\x1b\\E\x1b_command\x1b\\F");

        assert_eq!(terminal.line_text(0).unwrap(), "ABCDEF  ");
    }

    #[test]
    fn unterminated_control_strings_do_not_buffer_or_render_payloads() {
        let mut terminal = terminal(8, 2);
        terminal.feed(b"A\x1b]");
        terminal.feed(&vec![b'x'; 100_000]);

        assert!(matches!(terminal.parser, ParserState::ControlString { .. }));
        assert_eq!(terminal.line_text(0).unwrap(), "A       ");

        terminal.feed(b"\x1b\\B");
        assert_eq!(terminal.line_text(0).unwrap(), "AB      ");
    }

    #[test]
    fn horizontal_tab_uses_default_eight_column_stops() {
        let mut terminal = terminal(20, 2);
        terminal.feed(b"A\tB\tC");

        assert_eq!(terminal.cell(0, 0).unwrap().character, 'A');
        assert_eq!(terminal.cell(0, 8).unwrap().character, 'B');
        assert_eq!(terminal.cell(0, 16).unwrap().character, 'C');
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 17 });
    }

    #[test]
    fn tabulation_clamps_to_the_last_column_without_wrapping() {
        let mut terminal = terminal(10, 2);
        terminal.feed(b"\x1b[1;10H\t");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 9 });

        terminal.feed(b"\x1b[1;1H\x1b[2I");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 9 });
    }

    #[test]
    fn utf8_survives_chunk_boundaries() {
        let mut terminal = terminal(4, 2);
        terminal.feed(&[0xe2, 0x98]);
        terminal.feed(&[0x83]);
        assert_eq!(terminal.cell(0, 0).unwrap().character, '☃');
    }

    #[test]
    fn width_policy_is_pinned_to_generated_unicode_data() {
        assert_eq!(unicode_width::UNICODE_VERSION, (17, 0, 0));
        assert_eq!(terminal_cell_width('·'), 1);
        assert_eq!(terminal_cell_width('界'), 2);
        assert_eq!(terminal_string_width("👩‍👩‍👧‍👦"), 2);
        assert_eq!(terminal_string_width("1️⃣"), 2);
    }

    #[test]
    fn generated_width_data_retains_arabic_combining_marks() {
        let mut terminal = terminal(4, 2);
        terminal.feed("a\u{0898}b".as_bytes());

        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 2 });
        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{0898}']
        );
        assert_eq!(terminal.cell(0, 1).unwrap().character, 'b');
    }

    #[test]
    fn wide_unicode_characters_occupy_two_cells() {
        let mut terminal = terminal(6, 2);
        terminal.feed("A界B".as_bytes());

        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 4 });
        assert_eq!(terminal.cell(0, 0).unwrap().character, 'A');
        assert_eq!(terminal.cell(0, 1).unwrap().character, '界');
        assert_eq!(terminal.cell(0, 2).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(terminal.cell(0, 3).unwrap().character, 'B');
        assert_eq!(terminal.line_text(0).unwrap(), "A界 B  ");
    }

    #[test]
    fn wide_unicode_wraps_before_the_last_column() {
        let mut terminal = terminal(4, 2);
        terminal.feed("abc界".as_bytes());

        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 2 });
        assert_eq!(terminal.line_text(0).unwrap(), "abc ");
        assert_eq!(terminal.cell(1, 0).unwrap().character, '界');
        assert_eq!(terminal.cell(1, 1).unwrap().character, WIDE_CONTINUATION);
    }

    #[test]
    fn combining_marks_are_retained_without_advancing_columns() {
        let mut terminal = terminal(4, 2);
        terminal.feed("e\u{0301}x".as_bytes());

        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 2 });
        assert_eq!(terminal.cell(0, 0).unwrap().character, 'e');
        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{0301}']
        );
        assert_eq!(terminal.cell(0, 1).unwrap().character, 'x');
        assert_eq!(terminal.line_text(0).unwrap(), "e\u{0301}x  ");
    }

    #[test]
    fn combining_marks_are_hard_bounded_per_cell() {
        let mut terminal = terminal(4, 2);
        terminal.feed("e\u{0301}\u{0302}\u{0303}\u{0304}x".as_bytes());

        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{0301}', '\u{0302}', '\u{0303}']
        );
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 2 });
    }

    #[test]
    fn combining_marks_attach_to_wide_base_cells() {
        let mut terminal = terminal(5, 2);
        terminal.feed("界\u{0301}x".as_bytes());

        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{0301}']
        );
        assert_eq!(terminal.cell(0, 1).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn emoji_zwj_sequences_are_one_bounded_wide_cell() {
        let mut terminal = terminal(6, 2);
        terminal.feed("👩‍👩‍👧‍👦X".as_bytes());

        let family = terminal.cell(0, 0).unwrap();
        assert_eq!(family.character, '👩');
        assert_eq!(
            family.combining_marks(),
            &['\u{200d}', '👩', '\u{200d}', '👧', '\u{200d}', '👦']
        );
        assert_eq!(family.sequence_width(), 2);
        assert_eq!(terminal.cell(0, 1).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(terminal.cell(0, 2).unwrap().character, 'X');
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn emoji_modifier_flag_and_keycap_sequences_do_not_false_advance() {
        for (sequence, expected_suffix) in [
            ("👍🏽", vec!['🏽']),
            ("🇺🇸", vec!['🇸']),
            ("1️⃣", vec!['\u{fe0f}', '\u{20e3}']),
        ] {
            let mut terminal = terminal(5, 2);
            terminal.feed(sequence.as_bytes());
            terminal.feed(b"X");
            assert_eq!(
                terminal.cell(0, 0).unwrap().combining_marks(),
                expected_suffix
            );
            assert_eq!(terminal.cell(0, 1).unwrap().character, WIDE_CONTINUATION);
            assert_eq!(terminal.cell(0, 2).unwrap().character, 'X');
            assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
        }
    }

    #[test]
    fn variation_selectors_can_contract_and_expand_cell_width() {
        let mut text_terminal = terminal(5, 2);
        text_terminal.feed("❤\u{fe0e}X".as_bytes());
        assert_eq!(text_terminal.cell(0, 0).unwrap().sequence_width(), 1);
        assert_ne!(text_terminal.cell(0, 1).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(text_terminal.cell(0, 1).unwrap().character, 'X');
        assert_eq!(text_terminal.cursor(), Cursor { row: 0, col: 2 });

        let mut emoji_terminal = terminal(5, 2);
        emoji_terminal.feed("♥\u{fe0f}X".as_bytes());
        assert_eq!(emoji_terminal.cell(0, 0).unwrap().sequence_width(), 2);
        assert_eq!(emoji_terminal.cell(0, 1).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(emoji_terminal.cell(0, 2).unwrap().character, 'X');
        assert_eq!(emoji_terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn emoji_sequences_survive_utf8_and_zwj_chunk_boundaries() {
        let mut terminal = terminal(6, 2);
        let bytes = "👩‍💻X".as_bytes();
        terminal.feed(&bytes[..3]);
        terminal.feed(&bytes[3..7]);
        terminal.feed(&bytes[7..]);

        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{200d}', '💻']
        );
        assert_eq!(terminal.cell(0, 1).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(terminal.cell(0, 2).unwrap().character, 'X');
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 3 });
    }

    #[test]
    fn widening_sequence_at_right_margin_wraps_the_whole_grapheme() {
        let mut terminal = terminal(4, 2);
        terminal.feed("abc♥\u{fe0f}".as_bytes());

        assert_eq!(terminal.line_text(0).unwrap(), "abc ");
        assert_eq!(terminal.cell(1, 0).unwrap().character, '♥');
        assert_eq!(terminal.cell(1, 0).unwrap().combining_marks(), &['\u{fe0f}']);
        assert_eq!(terminal.cell(1, 1).unwrap().character, WIDE_CONTINUATION);
        assert_eq!(terminal.cursor(), Cursor { row: 1, col: 2 });
    }

    #[test]
    fn grapheme_suffix_storage_is_hard_bounded() {
        let mut terminal = terminal(8, 2);
        let mut sequence = String::from("👩");
        for _ in 0..32 {
            sequence.push(ZERO_WIDTH_JOINER);
            sequence.push('👩');
        }
        terminal.feed(sequence.as_bytes());

        assert!(
            terminal.cell(0, 0).unwrap().combining_marks().len() <= MAX_GRAPHEME_SUFFIX_SCALARS
        );
        assert!(terminal.cursor().row < terminal.size().rows);
        assert!(terminal.cursor().col <= terminal.size().cols);
    }

    #[test]
    fn grapheme_wide_cells_remain_normalized_after_resize_and_erase() {
        let mut terminal = terminal(5, 2);
        terminal.feed("A👩‍💻B".as_bytes());
        terminal.resize(EndpointSize::new(3, 2).unwrap());
        assert_eq!(terminal.cell(0, 1).unwrap().character, '👩');
        assert_eq!(terminal.cell(0, 2).unwrap().character, WIDE_CONTINUATION);

        terminal.feed(b"\x1b[1;3H\x1b[K");
        assert_eq!(terminal.cell(0, 1).unwrap(), Cell::default());
        assert_eq!(terminal.cell(0, 2).unwrap(), Cell::default());
    }

    #[test]
    fn overwriting_wide_cells_clears_the_other_half() {
        let mut terminal = terminal(5, 2);
        terminal.feed("界".as_bytes());
        terminal.feed(b"\x1b[1;2HX");

        assert_eq!(terminal.cell(0, 0).unwrap(), Cell::default());
        assert_eq!(terminal.cell(0, 1).unwrap().character, 'X');
    }

    #[test]
    fn device_status_and_cursor_reports_are_emitted_without_buffering() {
        let mut terminal = terminal(12, 4);
        terminal.feed(b"\x1b[3;5H");
        let mut replies = Vec::new();
        terminal.feed_with_responses(b"\x1b[5n\x1b[6n\x1b[?6n", |reply| {
            replies.push(reply.to_vec());
        });

        assert_eq!(
            replies,
            vec![
                b"\x1b[0n".to_vec(),
                b"\x1b[3;5R".to_vec(),
                b"\x1b[?3;5R".to_vec(),
            ]
        );
    }

    #[test]
    fn resize_preserves_overlap_and_clamps_cursor() {
        let mut terminal = terminal(4, 2);
        terminal.feed(b"abcd\r\nwxyz\x1b[2;4H");
        terminal.resize(EndpointSize::new(3, 1).unwrap());

        assert_eq!(terminal.size(), EndpointSize::new(3, 1).unwrap());
        assert_eq!(terminal.line_text(0).unwrap(), "abc");
        assert_eq!(terminal.cursor(), Cursor { row: 0, col: 2 });

        terminal.resize(EndpointSize::new(5, 3).unwrap());
        assert_eq!(terminal.line_text(0).unwrap(), "abc  ");
        assert_eq!(terminal.line_text(1).unwrap(), "     ");
    }

    #[test]
    fn resize_preserves_combining_marks_in_overlap() {
        let mut terminal = terminal(4, 2);
        terminal.feed("e\u{0301}x".as_bytes());
        terminal.resize(EndpointSize::new(3, 1).unwrap());

        assert_eq!(
            terminal.cell(0, 0).unwrap().combining_marks(),
            &['\u{0301}']
        );
        assert_eq!(terminal.line_text(0).unwrap(), "e\u{0301}x ");
    }

    #[test]
    fn resize_normalizes_scrollback_width_without_reflow() {
        let mut terminal = terminal(3, 2);
        terminal.feed(b"abc\r\ndef\r\nghi");
        assert_eq!(terminal.scrollback_line_text(0).unwrap(), "abc");

        terminal.resize(EndpointSize::new(5, 2).unwrap());
        assert_eq!(terminal.scrollback_line_text(0).unwrap(), "abc  ");

        terminal.resize(EndpointSize::new(2, 2).unwrap());
        assert_eq!(terminal.scrollback_line_text(0).unwrap(), "ab");
    }

    #[test]
    fn resize_drops_orphaned_wide_character_halves() {
        let mut terminal = terminal(4, 2);
        terminal.feed("ab界".as_bytes());
        terminal.resize(EndpointSize::new(3, 2).unwrap());

        assert_eq!(terminal.line_text(0).unwrap(), "ab ");
        assert_eq!(terminal.cell(0, 2).unwrap(), Cell::default());
    }
}
