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

/// Parses a hex color like "#1a1a1e" into a crossterm Color.
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
/// Each tab shows as " name " with active tab highlighted.
pub fn render_tab_bar(
    stdout: &mut impl Write,
    theme: &Theme,
    tabs: &[String],
    active: usize,
    cols: u16,
) -> io::Result<()> {
    let bg = hex_color(&theme.tab_inactive_bg, Color::Black);
    let active_bg = hex_color(&theme.tab_active_bg, Color::DarkGrey);
    let fg = hex_color(&theme.tab_inactive_fg, Color::Grey);
    let active_fg_color = hex_color(&theme.tab_active_fg, Color::White);

    queue!(
        stdout,
        MoveTo(0, 0),
        SetBackgroundColor(bg),
        SetForegroundColor(fg),
    )?;

    let mut col: u16 = 0;
    for (i, name) in tabs.iter().enumerate() {
        let label = format!(" {name} ");
        let label_len = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);

        if col + label_len > cols {
            break;
        }

        if i == active {
            queue!(
                stdout,
                SetBackgroundColor(active_bg),
                SetForegroundColor(active_fg_color),
                SetAttribute(Attribute::Bold),
            )?;
        } else {
            queue!(
                stdout,
                SetBackgroundColor(bg),
                SetForegroundColor(fg),
                SetAttribute(Attribute::Reset),
            )?;
        }

        queue!(stdout, Print(&label))?;
        col += label_len;
    }

    // Fill the rest of the row
    queue!(stdout, SetBackgroundColor(bg))?;
    for _ in col..cols {
        queue!(stdout, Print(" "))?;
    }

    queue!(
        stdout,
        SetAttribute(Attribute::Reset),
        ResetColor,
    )?;
    Ok(())
}

/// Renders the status bar at the last row.
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
    let fg = hex_color(&theme.status_fg, Color::Grey);
    let accent = hex_color(&theme.accent, Color::Blue);

    queue!(
        stdout,
        MoveTo(0, row),
        SetBackgroundColor(bg),
        SetForegroundColor(fg),
    )?;

    let left = format!(" {tab_name} ");
    let right = message.unwrap_or("");
    
    queue!(stdout, SetForegroundColor(accent), Print(&left))?;
    queue!(stdout, SetForegroundColor(fg))?;

    let left_len = u16::try_from(left.chars().count()).unwrap_or(u16::MAX);
    let right_len = u16::try_from(right.chars().count().min(cols as usize)).unwrap_or(u16::MAX);
    
    // Fill middle
    let middle = cols.saturating_sub(left_len).saturating_sub(right_len);
    for _ in 0..middle {
        queue!(stdout, Print(" "))?;
    }
    
    if !right.is_empty() {
        let truncated: String = right.chars().take(cols as usize).collect();
        queue!(stdout, Print(truncated))?;
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
    let fg = hex_color(&theme.palette_fg, Color::White);
    let sel_bg = hex_color(&theme.palette_selected_bg, Color::DarkGrey);
    let sel_fg_color = hex_color(&theme.palette_selected_fg, Color::White);
    let border = hex_color(&theme.border, Color::DarkGrey);
    let accent = hex_color(&theme.accent, Color::Blue);

    let width = (cols * 2 / 3).clamp(20, 60);
    let max_items = 10usize;
    let filtered = palette.filter(commands);
    let height = u16::try_from(
        (filtered.len().min(max_items) + 3).min(rows as usize - 4)
    ).unwrap_or(u16::MAX);
    
    let start_col = cols.saturating_sub(width) / 2;
    let start_row = rows.saturating_sub(height) / 3; // Upper third

    // Draw box
    for row in 0..height {
        queue!(stdout, MoveTo(start_col, start_row + row))?;
        if row == 0 || row == height - 1 {
            queue!(
                stdout,
                SetBackgroundColor(bg),
                SetForegroundColor(border),
            )?;
            queue!(stdout, Print("─".repeat(width as usize)))?;
        } else if row == 1 {
            // Input line
            queue!(
                stdout,
                SetBackgroundColor(bg),
                SetForegroundColor(accent),
            )?;
            queue!(stdout, Print("> "))?;
            queue!(stdout, SetForegroundColor(fg))?;
            let input_display: String = palette.input.chars().take((width - 4) as usize).collect();
            queue!(stdout, Print(&input_display))?;
            let remaining = width.saturating_sub(4 + u16::try_from(input_display.chars().count()).unwrap_or(u16::MAX));
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
                    )?;
                } else {
                    queue!(
                        stdout,
                        SetBackgroundColor(bg),
                        SetForegroundColor(fg),
                    )?;
                }
                queue!(stdout, Print("  "))?;
                let title: String = cmd.title.chars().take((width - 4) as usize).collect();
                queue!(stdout, Print(&title))?;
                let remaining = width.saturating_sub(4 + u16::try_from(title.chars().count()).unwrap_or(u16::MAX));
                for _ in 0..remaining {
                    queue!(stdout, Print(" "))?;
                }
            } else {
                queue!(stdout, SetBackgroundColor(bg))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_color_parses_valid() {
        assert_eq!(
            hex_color("#ff0000", Color::Black),
            Color::Rgb { r: 255, g: 0, b: 0 }
        );
        assert_eq!(
            hex_color("#1a1a1e", Color::Black),
            Color::Rgb { r: 26, g: 26, b: 30 }
        );
    }

    #[test]
    fn hex_color_falls_back_on_invalid() {
        assert_eq!(hex_color("notacolor", Color::Black), Color::Black);
        assert_eq!(hex_color("#fff", Color::Red), Color::Red);
    }

    #[test]
    fn palette_filters_commands() {
        let mut palette = PaletteState::new();
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
}
