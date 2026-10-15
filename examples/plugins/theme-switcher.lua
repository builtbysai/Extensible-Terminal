-- Example plugin: quick theme switcher
--
-- Place this file in your config's plugins/ directory.
-- It adds commands to switch between themes via the palette.

local themes = {
    dark = {
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
    },
    light = {
        background = "#f5f5f5",
        foreground = "#2d2d2d",
        tab_active_bg = "#e0e0e0",
        tab_active_fg = "#000000",
        tab_inactive_bg = "#f5f5f5",
        tab_inactive_fg = "#808080",
        status_bg = "#e8e8e8",
        status_fg = "#606060",
        palette_bg = "#ffffff",
        palette_fg = "#2d2d2d",
        palette_selected_bg = "#d0d0d0",
        palette_selected_fg = "#000000",
        border = "#c0c0c0",
        accent = "#0066cc",
    },
    -- Warm sunset theme
    sunset = {
        background = "#1e1a16",
        foreground = "#e8d8c8",
        tab_active_bg = "#3d2d22",
        tab_active_fg = "#ffffff",
        tab_inactive_bg = "#1e1a16",
        tab_inactive_fg = "#a08060",
        status_bg = "#2d231c",
        status_fg = "#c0a080",
        palette_bg = "#241c16",
        palette_fg = "#e8d8c8",
        palette_selected_bg = "#4d3d2d",
        palette_selected_fg = "#ffffff",
        border = "#4a3a2d",
        accent = "#ff9a5a",
    },
}

for name, theme in pairs(themes) do
    terminal.command(
        "theme:" .. name,
        "Theme: " .. name,
        function()
            terminal.set_theme(theme)
            terminal.set_status("theme: " .. name)
        end
    )
end

-- Keybinding to cycle themes
local theme_names = { "dark", "light", "sunset" }
local current = 1
terminal.bind("ctrl+shift+t", function()
    current = current % #theme_names + 1
    local name = theme_names[current]
    terminal.set_theme(themes[name])
    terminal.set_status("theme: " .. name)
end)

terminal.log("theme-switcher plugin loaded")
