-- Extensible-Terminal default configuration
-- lite-inspired: minimal, beautiful, hackable.
--
-- Edit this file and changes take effect immediately (hot reload).
-- No rebuild needed.

-- Theme: lite's EXACT palette from data/core/style.lua.
-- background #2e2e32, background2 #252529, text #97979c,
-- accent #e1e1e6, dim #525257, divider #202024,
-- line_highlight #343438, caret #93DDFA.
terminal.set_theme({
    background = "#2e2e32",
    foreground = "#97979c",
    tab_active_bg = "#2e2e32",
    tab_active_fg = "#97979c",
    tab_inactive_bg = "#252529",
    tab_inactive_fg = "#525257",
    status_bg = "#252529",
    status_fg = "#97979c",
    palette_bg = "#252529",
    palette_fg = "#97979c",
    palette_selected_bg = "#343438",
    palette_selected_fg = "#e1e1e6",
    border = "#202024",
    accent = "#93DDFA",
})

-- Keybindings
terminal.bind("ctrl+t", function() terminal.new_tab() end)
terminal.bind("ctrl+w", function() terminal.close_tab() end)
terminal.bind("ctrl+tab", function() terminal.next_tab() end)
terminal.bind("ctrl+shift+tab", function() terminal.prev_tab() end)
terminal.bind("ctrl+shift+p", function() terminal.palette() end)
terminal.bind("ctrl+q", function() terminal.quit() end)

-- Split panes: keyboard-driven, no chrome. Focus follows the new pane.
terminal.bind("ctrl+shift+e", function() terminal.split_horizontal() end)
terminal.bind("ctrl+shift+o", function() terminal.split_vertical() end)
terminal.bind("ctrl+shift+w", function() terminal.close_pane() end)
terminal.bind("ctrl+shift+]", function() terminal.next_pane() end)
terminal.bind("ctrl+shift+[", function() terminal.prev_pane() end)

-- Commands for the palette (ctrl+shift+p)
terminal.command("tab:new", "New Tab", function() terminal.new_tab() end)
terminal.command("tab:close", "Close Tab", function() terminal.close_tab() end)
terminal.command("tab:next", "Next Tab", function() terminal.next_tab() end)
terminal.command("tab:prev", "Previous Tab", function() terminal.prev_tab() end)
terminal.command("pane:split-horizontal", "Split Pane Horizontally", function() terminal.split_horizontal() end)
terminal.command("pane:split-vertical", "Split Pane Vertically", function() terminal.split_vertical() end)
terminal.command("pane:close", "Close Pane", function() terminal.close_pane() end)
terminal.command("pane:next", "Next Pane", function() terminal.next_pane() end)
terminal.command("pane:prev", "Previous Pane", function() terminal.prev_pane() end)
terminal.command("app:quit", "Quit", function() terminal.quit() end)

terminal.log("default config loaded")

-- Trusted config values (returned to the Rust host)
return {
    term = "xterm-256color",
    colorterm = "truecolor",
    cols = 100,
    rows = 30,
}
