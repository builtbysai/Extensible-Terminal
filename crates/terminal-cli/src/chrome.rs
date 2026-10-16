//! Terminal chrome: tab bar, status bar, and command palette rendering.
//!
//! This module handles the lite-inspired UI chrome that surrounds the terminal
//! content: a minimal tab bar at the top, a status bar at the bottom, and a
//! centered command palette overlay. All colors come from the Lua-defined theme.

#![forbid(unsafe_code)]

use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::style::{Color, Print, ResetColor, SetBackgroundColor, SetForegroundColor};
use crossterm::{queue, style::Attribute, style::SetAttribute};
use user_config::Theme;

/// Parses a hex color like "#2e2e32" into a crossterm Color.
/// Falls back to a sensible default for malformed input.
fn hex_color(hex: &str, fallback: Color) -> Color {
    let hex = hex.strip_prefix('#').unwrap_or(hex);
    if hex.len() != 6 {
        return fallback;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
    Color::Rgb { r, g, b }
}

/// Renders the tab bar at row 0.
///
/// lite's exact logic (rootview.lua draw_tabs):
/// - Bar background: background2 (#252529)
/// - 1px divider line at bottom of bar: divider (#202024)
/// - Inactive tab text: dim (#525257)
/// - Active tab: background becomes main bg (#2e2e32) so it visually connects
///   to the content below, text becomes text (#97979c), 1px dividers flank it.
/// - Text is centered in each tab. No bold anywhere.
pub fn render_tab_bar(
    stdout: &mut impl Write,
    theme: &Theme,
    tabs: &[String],
    active: usize,
    cols: u16,
) -> io::Result<()> {
    let bar_bg = hex_color(&theme.tab_inactive_bg, Color::Black);
    let active_bg = hex_color(&theme.background, Color::Black);
    let fg = hex_color(&theme.tab_inactive_fg, Color::DarkGrey);
    let active_fg_color = hex_color(&theme.tab_active_fg, Color::Grey);
    let divider = hex_color(&theme.border, Color::DarkGrey);

    // Bar background across the full row.
    queue!(
        stdout,
        MoveTo(0, 0),
        SetBackgroundColor(bar_bg),
        SetForegroundColor(fg),
        SetAttribute(Attribute::Reset),
    )?;
    for _ in 0..cols {
        queue!(stdout, Print(" "))?;
    }

    // 1px divider at the bottom edge of the bar (lite draws a 1px rect;
    // in a cell grid we tint the last cell row of the bar instead).
    // Actually: draw the divider as the bottom of the bar background by
    // leaving the bar 1 cell tall and relying on the bg change. We render
    // the divider as a subtle line using the divider color on the bar bg.
    // Simplest faithful adaptation: bar bg row, then content starts.

    let mut col: u16 = 0;
    let tab_count = tabs.len().max(1);
    // lite: tw = min(tab_width, size.x / #views). In cells, divide evenly.
    let tw = (cols / tab_count as u16).max(8);
    for (i, name) in tabs.iter().enumerate() {
        if col + tw > cols {
            break;
        }
        let is_active = i == active;

        // Center the name in the tab slot, lite-style.
        let name_len = name.chars().count() as u16;
        let inner = tw.saturating_sub(2); // leave room for side dividers on active
        let text_w = name_len.min(inner);
        let pad_left = inner.saturating_sub(text_w) / 2;
        let pad_right = inner.saturating_sub(text_w).saturating_sub(pad_left);
        let display: String = name.chars().take(text_w as usize).collect();

        queue!(stdout, MoveTo(col, 0))?;
        if is_active {
            // Active tab: main background, text color, divider chars flanking.
            queue!(
                stdout,
                SetBackgroundColor(active_bg),
                SetForegroundColor(divider),
            )?;
            queue!(stdout, Print("│"))?;
            queue!(
                stdout,
                SetBackgroundColor(active_bg),
                SetForegroundColor(active_fg_color),
                SetAttribute(Attribute::Reset),
            )?;
            for _ in 0..pad_left {
                queue!(stdout, Print(" "))?;
            }
            queue!(stdout, Print(&display))?;
            for _ in 0..pad_right {
                queue!(stdout, Print(" "))?;
            }
            queue!(
                stdout,
                SetBackgroundColor(active_bg),
                SetForegroundColor(divider),
            )?;
            queue!(stdout, Print("│"))?;
        } else {
            // Inactive: dim text on the bar background, no adornment.
            queue!(
                stdout,
                SetBackgroundColor(bar_bg),
                SetForegroundColor(fg),
                SetAttribute(Attribute::Reset),
            )?;
            queue!(stdout, Print(" "))?;
            for _ in 0..pad_left {
                queue!(stdout, Print(" "))?;
            }
            queue!(stdout, Print(&display))?;
            for _ in 0..pad_right {
                queue!(stdout, Print(" "))?;
            }
            queue!(stdout, Print(" "))?;
        }
        col += tw;
    }

    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

/// Renders the status bar at the last row.
///
/// lite's exact logic (statusview.lua):
/// - Background: background2 (#252529)
/// - Values in text (#97979c), separators and labels in dim (#525257)
/// - Separator strings: "      " and "   |   ", all dim.
/// - Nothing bright. The filename is the brightest thing at #97979c.
pub fn render_status_bar(
    stdout: &mut impl Write,
    theme: &Theme,
    message: Option<&str>,
    tab_name: &str,
    cols: u16,
    rows: u16,
) -> io::Result<()> {
    if rows == 0 {
        return Ok(());
    }
    let row = rows - 1;
    let bg = hex_color(&theme.status_bg, Color::Black);
    let text = hex_color(&theme.status_fg, Color::Grey);
    let dim = hex_color(&theme.tab_inactive_fg, Color::DarkGrey);

    queue!(
        stdout,
        MoveTo(0, row),
        SetBackgroundColor(bg),
        SetAttribute(Attribute::Reset),
    )?;
    // Fill entire row with bg first.
    for _ in 0..cols {
        queue!(stdout, Print(" "))?;
    }
    queue!(stdout, MoveTo(0, row))?;

    // Left: tab name in text color, then dim separator, then message in dim.
    queue!(stdout, SetForegroundColor(text))?;
    let left = format!(" {tab_name}");
    let left_len = left.chars().count() as u16;
    queue!(stdout, Print(&left))?;

    let mut col = left_len;
    if let Some(msg) = message {
        if !msg.is_empty() {
            queue!(stdout, SetForegroundColor(dim))?;
            let sep = "   |   ";
            queue!(stdout, Print(sep))?;
            col += 7;
            queue!(stdout, SetForegroundColor(text))?;
            let avail = cols.saturating_sub(col).saturating_sub(1) as usize;
            let truncated: String = msg.chars().take(avail).collect();
            queue!(stdout, Print(&truncated))?;
        }
    }

    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

/// State for the command palette overlay.
#[derive(Debug, Default)]
pub struct PaletteState {
    pub open: bool,
    pub input: String,
    pub selected: usize,
}

impl PaletteState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn open(&mut self) {
        self.open = true;
        self.input.clear();
        self.selected = 0;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.input.clear();
        self.selected = 0;
    }

    /// Filters commands by fuzzy match on the input.
    #[must_use]
    pub fn filter<'a>(&self, commands: &'a [user_config::PaletteCommand]) -> Vec<&'a user_config::PaletteCommand> {
        if self.input.is_empty() {
            return commands.iter().collect();
        }
        let query = self.input.to_lowercase();
        commands
            .iter()
            .filter(|cmd| {
                cmd.title.to_lowercase().contains(&query) || cmd.id.to_lowercase().contains(&query)
            })
            .collect()
    }
}

/// Renders the command palette as a centered overlay.
///
/// lite's exact logic (commandview.lua draw_suggestions_box):
/// - Background: background3 (#252529)
/// - 1px divider line on TOP (#202024). No border box.
/// - Selected row: line_highlight bg (#343438), accent text (#e1e1e6)
/// - Normal rows: text (#97979c)
pub fn render_palette(
    stdout: &mut impl Write,
    theme: &Theme,
    palette: &PaletteState,
    commands: &[user_config::PaletteCommand],
    cols: u16,
    rows: u16,
) -> io::Result<()> {
    if !palette.open {
        return Ok(());
    }

    let bg = hex_color(&theme.palette_bg, Color::Black);
    let fg = hex_color(&theme.palette_fg, Color::Grey);
    let sel_bg = hex_color(&theme.palette_selected_bg, Color::DarkGrey);
    let sel_fg_color = hex_color(&theme.palette_selected_fg, Color::White);
    let divider = hex_color(&theme.border, Color::DarkGrey);

    let width = (cols * 2 / 3).clamp(20, 60);
    let max_items = 10usize;
    let filtered = palette.filter(commands);
    // Divider row + input line + items. No border box.
    let height = u16::try_from(
        (filtered.len().min(max_items) + 2).min(rows as usize - 4)
    ).unwrap_or(u16::MAX);

    let start_col = cols.saturating_sub(width) / 2;
    let start_row = rows.saturating_sub(height) / 3; // Upper third

    // Clear the whole area the palette could ever occupy.
    let max_height = u16::try_from((max_items + 2).min(rows as usize - 4)).unwrap_or(u16::MAX);
    let max_start_row = rows.saturating_sub(max_height) / 3;
    queue!(stdout, SetBackgroundColor(bg))?;
    for row in 0..max_height {
        queue!(stdout, MoveTo(start_col, max_start_row + row))?;
        for _ in 0..width {
            queue!(stdout, Print(" "))?;
        }
    }

    for row in 0..height {
        queue!(stdout, MoveTo(start_col, start_row + row))?;
        if row == 0 {
            // 1px divider on top, lite-style.
            queue!(
                stdout,
                SetBackgroundColor(bg),
                SetForegroundColor(divider),
                SetAttribute(Attribute::Reset),
            )?;
            for _ in 0..width {
                queue!(stdout, Print("─"))?;
            }
        } else if row == 1 {
            // Input line.
            queue!(
                stdout,
                SetBackgroundColor(bg),
                SetForegroundColor(fg),
                SetAttribute(Attribute::Reset),
            )?;
            let input_display: String = palette.input.chars().take((width - 2) as usize).collect();
            queue!(stdout, Print(" "))?;
            queue!(stdout, Print(&input_display))?;
            let remaining = width.saturating_sub(1 + u16::try_from(input_display.chars().count()).unwrap_or(u16::MAX));
            for _ in 0..remaining {
                queue!(stdout, Print(" "))?;
            }
        } else {
            // Command items
            let idx = (row - 2) as usize;
            if let Some(cmd) = filtered.get(idx) {
                let is_selected = idx == palette.selected;
                if is_selected {
                    queue!(
                        stdout,
                        SetBackgroundColor(sel_bg),
                        SetForegroundColor(sel_fg_color),
                        SetAttribute(Attribute::Reset),
                    )?;
                } else {
                    queue!(
                        stdout,
                        SetBackgroundColor(bg),
                        SetForegroundColor(fg),
                        SetAttribute(Attribute::Reset),
                    )?;
                }
                queue!(stdout, Print(" "))?;
                let title: String = cmd.title.chars().take((width - 2) as usize).collect();
                queue!(stdout, Print(&title))?;
                let remaining = width.saturating_sub(1 + u16::try_from(title.chars().count()).unwrap_or(u16::MAX));
                for _ in 0..remaining {
                    queue!(stdout, Print(" "))?;
                }
            } else {
                queue!(
                    stdout,
                    SetBackgroundColor(bg),
                    SetAttribute(Attribute::Reset),
                )?;
                for _ in 0..width {
                    queue!(stdout, Print(" "))?;
                }
            }
        }
    }

    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

/// A one-cell divider between split panes, in content-area coordinates.
/// Mirrors [`crate::panes::PaneDivider`] without depending on the pane tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChromeDivider {
    pub col: u16,
    pub row: u16,
    pub cols: u16,
    pub rows: u16,
    /// True for a vertical line, false for a horizontal line.
    pub vertical: bool,
}

/// Renders split-pane dividers as single dim lines.
///
/// lite restraint: no pane titles, no borders, no adornment — just the
/// theme's divider color (`border`, lite's `#202024`) so the eye can tell
/// panes apart and nothing more.
pub fn render_pane_dividers(
    stdout: &mut impl Write,
    theme: &Theme,
    dividers: &[ChromeDivider],
    row_offset: u16,
) -> io::Result<()> {
    if dividers.is_empty() {
        return Ok(());
    }
    let color = hex_color(&theme.border, Color::DarkGrey);
    queue!(
        stdout,
        SetForegroundColor(color),
        SetAttribute(Attribute::Reset),
    )?;
    for divider in dividers {
        if divider.vertical {
            for row in 0..divider.rows {
                queue!(
                    stdout,
                    MoveTo(divider.col, divider.row + row_offset + row),
                    Print("│"),
                )?;
            }
        } else {
            queue!(
                stdout,
                MoveTo(divider.col, divider.row + row_offset),
            )?;
            for _ in 0..divider.cols {
                queue!(stdout, Print("─"))?;
            }
        }
    }
    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::assert_is_empty)]
mod tests {
    use super::*;

    #[test]
    fn hex_color_parses_valid() {
        assert_eq!(
            hex_color("#ff0000", Color::Black),
            Color::Rgb { r: 255, g: 0, b: 0 }
        );
        assert_eq!(
            hex_color("#2e2e32", Color::Black),
            Color::Rgb { r: 46, g: 46, b: 50 }
        );
    }

    #[test]
    fn hex_color_falls_back_on_invalid() {
        assert_eq!(hex_color("notacolor", Color::Black), Color::Black);
        assert_eq!(hex_color("#fff", Color::Red), Color::Red);
    }

    #[test]
    fn palette_filters_commands() {        let mut palette = PaletteState::new();
        let commands = vec![
            user_config::PaletteCommand { id: "tab:new".to_string(), title: "New Tab".to_string() },
            user_config::PaletteCommand { id: "tab:close".to_string(), title: "Close Tab".to_string() },
        ];
        
        palette.input = "new".to_string();
        let filtered = palette.filter(&commands);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "tab:new");
        
        palette.input.clear();
        let filtered = palette.filter(&commands);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn pane_dividers_draw_single_dim_lines() {
        let theme = Theme::default();
        let mut output = Vec::new();
        let dividers = vec![
            ChromeDivider { col: 40, row: 0, cols: 1, rows: 3, vertical: true },
            ChromeDivider { col: 0, row: 5, cols: 4, rows: 1, vertical: false },
        ];
        render_pane_dividers(&mut output, &theme, &dividers, 1).unwrap();
        let text = String::from_utf8_lossy(&output);
        // Vertical divider: one │ per row, shifted by row_offset.
        assert_eq!(text.matches('│').count(), 3);
        // Horizontal divider: ─ across its width.
        assert_eq!(text.matches('─').count(), 4);
        // Empty divider list emits nothing.
        let mut empty = Vec::new();
        render_pane_dividers(&mut empty, &theme, &[], 1).unwrap();
        assert!(empty.is_empty());
    }
}
