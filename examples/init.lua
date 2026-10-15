-- Extensible-Terminal default configuration
-- lite-inspired: minimal, beautiful, hackable.
--
-- Edit this file and changes take effect immediately (hot reload).
-- No rebuild needed.

-- Theme: dark, muted, refined. Inspired by lite's aesthetic.
terminal.set_theme({
    background = "#1a1a1e",
    foreground = "#d8d8d8",
    tab_active_bg = "#2d2d32",
    tab_active_fg = "#ffffff",
    tab_inactive_bg = "#1a1a1e",
    tab_inactive_fg = "#808080",
    status_bg = "#252528",
    status_fg = "#a0a0a0",
    palette_bg = "#1e1e22",
    palette_fg = "#d8d8d8",
    palette_selected_bg = "#3d3d42",
    palette_selected_fg = "#ffffff",
    border = "#3a3a3e",
    accent = "#7aa2f7",
})

-- Keybindings
terminal.bind("ctrl+t", function() terminal.new_tab() end)
terminal.bind("ctrl+w", function() terminal.close_tab() end)
terminal.bind("ctrl+tab", function() terminal.next_tab() end)
terminal.bind("ctrl+shift+tab", function() terminal.prev_tab() end)
terminal.bind("ctrl+shift+p", function() terminal.palette() end)
terminal.bind("ctrl+q", function() terminal.quit() end)

-- Commands for the palette (ctrl+shift+p)
terminal.command("tab:new", "New Tab", function() terminal.new_tab() end)
terminal.command("tab:close", "Close Tab", function() terminal.close_tab() end)
terminal.command("tab:next", "Next Tab", function() terminal.next_tab() end)
terminal.command("tab:prev", "Previous Tab", function() terminal.prev_tab() end)
terminal.command("app:quit", "Quit", function() terminal.quit() end)

terminal.log("default config loaded")

-- Trusted config values (returned to the Rust host)
return {
    term = "xterm-256color",
    colorterm = "truecolor",
    cols = 100,
    rows = 30,
}
