//! Lua plugin system for Extensible-Terminal.
//!
//! This module implements the lite-inspired plugin architecture: a small Rust
//! core exposes a `terminal` Lua module, and all behavior (keybindings, themes,
//! tabs, commands, status bar) is defined in Lua. Plugins live in `plugins/*.lua`
//! and the user config in `init.lua`. Files are hot-reloadable via mtime watching.

#![forbid(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::SystemTime;

use mlua::{Function, Lua, Table};

/// Actions that Lua code can request the Rust host to perform.
///
/// Lua never mutates host state directly. Instead, plugin callbacks push
/// actions onto a queue that the host drains each frame. This keeps the
/// Rust core in control of all state transitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginAction {
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    RenameTab(String),
    SetStatusMessage(String),
    ClearStatusMessage,
    OpenPalette,
    Quit,
}

/// A keybinding registered from Lua: mods + key -> Lua callback.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct KeyBinding {
    pub mods: String,
    pub key: String,
}

impl KeyBinding {
    #[must_use]
    pub fn new(mods: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            mods: mods.into().to_lowercase(),
            key: key.into().to_lowercase(),
        }
    }

    /// Parses a binding string like "ctrl+shift+t" into a normalized binding.
    /// Modifier order is normalized so "shift+ctrl+t" == "ctrl+shift+t".
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.to_lowercase();
        let mut parts: Vec<&str> = spec.split('+').collect();
        let key = parts.pop()?;
        if key.is_empty() {
            return None;
        }
        parts.sort_unstable();
        Some(Self {
            mods: parts.join("+"),
            key: key.to_string(),
        })
    }
}

/// A command registered for the command palette.
#[derive(Clone, Debug)]
pub struct PaletteCommand {
    pub id: String,
    pub title: String,
}

/// Theme colors for the terminal chrome (tab bar, status bar, palette).
/// Terminal content itself uses the PTY's own colors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Theme {
    pub background: String,
    pub foreground: String,
    pub tab_active_bg: String,
    pub tab_active_fg: String,
    pub tab_inactive_bg: String,
    pub tab_inactive_fg: String,
    pub status_bg: String,
    pub status_fg: String,
    pub palette_bg: String,
    pub palette_fg: String,
    pub palette_selected_bg: String,
    pub palette_selected_fg: String,
    pub border: String,
    pub accent: String,
}

impl Default for Theme {
    /// lite-inspired default: dark, muted, refined.
    fn default() -> Self {
        Self {
            background: "#1a1a1e".to_string(),
            foreground: "#d8d8d8".to_string(),
            tab_active_bg: "#2d2d32".to_string(),
            tab_active_fg: "#ffffff".to_string(),
            tab_inactive_bg: "#1a1a1e".to_string(),
            tab_inactive_fg: "#808080".to_string(),
            status_bg: "#252528".to_string(),
            status_fg: "#a0a0a0".to_string(),
            palette_bg: "#1e1e22".to_string(),
            palette_fg: "#d8d8d8".to_string(),
            palette_selected_bg: "#3d3d42".to_string(),
            palette_selected_fg: "#ffffff".to_string(),
            border: "#3a3a3e".to_string(),
            accent: "#7aa2f7".to_string(),
        }
    }
}

impl Theme {
    fn from_table(table: &Table) -> Result<Self, mlua::Error> {
        let mut theme = Self::default();
        macro_rules! set_field {
            ($name:literal, $field:ident) => {
                if let Some(v) = table.get::<Option<String>>($name)? {
                    theme.$field = v;
                }
            };
        }
        set_field!("background", background);
        set_field!("foreground", foreground);
        set_field!("tab_active_bg", tab_active_bg);
        set_field!("tab_active_fg", tab_active_fg);
        set_field!("tab_inactive_bg", tab_inactive_bg);
        set_field!("tab_inactive_fg", tab_inactive_fg);
        set_field!("status_bg", status_bg);
        set_field!("status_fg", status_fg);
        set_field!("palette_bg", palette_bg);
        set_field!("palette_fg", palette_fg);
        set_field!("palette_selected_bg", palette_selected_bg);
        set_field!("palette_selected_fg", palette_selected_fg);
        set_field!("border", border);
        set_field!("accent", accent);
        Ok(theme)
    }
}

/// Errors from plugin loading and execution.
#[derive(Debug)]
pub enum PluginError {
    Io(std::io::Error),
    Lua(mlua::Error),
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "plugin file error: {error}"),
            Self::Lua(error) => write!(f, "plugin Lua error: {error}"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<std::io::Error> for PluginError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<mlua::Error> for PluginError {
    fn from(value: mlua::Error) -> Self {
        Self::Lua(value)
    }
}

/// Shared mutable plugin state, accessible from Lua callbacks.
#[derive(Default)]
struct PluginState {
    bindings: HashMap<KeyBinding, String>,
    commands: HashMap<String, (String, String)>,
    theme: Theme,
    action_queue: Vec<PluginAction>,
    callback_counter: u64,
}

/// Manages Lua plugins: loading, keybindings, themes, commands, and hot reload.
///
/// The manager owns the Lua state. Plugins register callbacks that are stored
/// in the Lua registry. When the host needs to invoke a callback (e.g. a key
/// was pressed), it calls back into Lua.
pub struct PluginManager {
    lua: Lua,
    state: Rc<RefCell<PluginState>>,
    /// Tracked files for hot reload: path -> last mtime
    watched_files: HashMap<PathBuf, SystemTime>,
    config_dir: PathBuf,
}

impl PluginManager {
    /// Creates a new plugin manager with an empty Lua state.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Lua`] if the Lua state cannot be initialized.
    pub fn new(config_dir: impl Into<PathBuf>) -> Result<Self, PluginError> {
        let mut manager = Self {
            lua: Lua::new(),
            state: Rc::new(RefCell::new(PluginState::default())),
            watched_files: HashMap::new(),
            config_dir: config_dir.into(),
        };
        manager.setup_terminal_module()?;
        Ok(manager)
    }

    /// Returns the current theme.
    #[must_use]
    pub fn theme(&self) -> Theme {
        self.state.borrow().theme.clone()
    }

    /// Returns all registered palette commands.
    #[must_use]
    pub fn commands(&self) -> Vec<PaletteCommand> {
        self.state
            .borrow()
            .commands
            .iter()
            .map(|(id, (title, _))| PaletteCommand {
                id: id.clone(),
                title: title.clone(),
            })
            .collect()
    }

    /// Drains the action queue, returning all pending actions.
    pub fn drain_actions(&mut self) -> Vec<PluginAction> {
        std::mem::take(&mut self.state.borrow_mut().action_queue)
    }

    /// Checks if a keybinding exists for the given mods and key.
    #[must_use]
    pub fn has_binding(&self, mods: &str, key: &str) -> bool {
        let binding = KeyBinding::new(mods, key);
        self.state.borrow().bindings.contains_key(&binding)
    }

    /// Invokes the Lua callback for a keybinding.
    ///
    /// Returns `Ok(true)` if a binding was found and invoked, `Ok(false)` if
    /// no binding exists.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Lua`] if the callback fails.
    pub fn invoke_binding(&mut self, mods: &str, key: &str) -> Result<bool, PluginError> {
        let binding = KeyBinding::new(mods, key);
        let registry_key = match self.state.borrow().bindings.get(&binding) {
            Some(key) => key.clone(),
            None => return Ok(false),
        };

        let func: Function = self.lua.named_registry_value(&registry_key)?;
        func.call::<()>(())?;
        Ok(true)
    }

    /// Invokes a palette command by ID.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Lua`] if the callback fails.
    pub fn invoke_command(&mut self, id: &str) -> Result<bool, PluginError> {
        let registry_key = match self.state.borrow().commands.get(id) {
            Some((_, key)) => key.clone(),
            None => return Ok(false),
        };

        let func: Function = self.lua.named_registry_value(&registry_key)?;
        func.call::<()>(())?;
        Ok(true)
    }

    /// Loads `init.lua` from the config directory, then all `plugins/*.lua`.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Io`] for file errors, [`PluginError::Lua`] for
    /// Lua errors.
    pub fn load_all(&mut self) -> Result<(), PluginError> {
        let init_path = self.config_dir.join("init.lua");
        if init_path.exists() {
            self.load_file(&init_path)?;
        }

        let plugins_dir = self.config_dir.join("plugins");
        if plugins_dir.is_dir() {
            let mut plugins: Vec<PathBuf> = fs::read_dir(&plugins_dir)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "lua"))
                .collect();
            plugins.sort_unstable();
            for path in plugins {
                self.load_file(&path)?;
            }
        }

        Ok(())
    }

    /// Checks watched files for modifications and reloads if any changed.
    ///
    /// Returns true if a reload occurred.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError`] if reloading fails.
    pub fn check_hot_reload(&mut self) -> Result<bool, PluginError> {
        let mut changed = false;
        for (path, last) in &self.watched_files {
            let mtime = fs::metadata(path)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            if mtime > *last {
                changed = true;
                break;
            }
        }

        if changed {
            self.reload_all()?;
        }
        Ok(changed)
    }

    /// Clears all plugin state and reloads from disk.
    fn reload_all(&mut self) -> Result<(), PluginError> {
        self.lua = Lua::new();
        self.state = Rc::new(RefCell::new(PluginState::default()));
        self.setup_terminal_module()?;

        // Update mtimes before loading
        let paths: Vec<PathBuf> = self.watched_files.keys().cloned().collect();
        for path in paths {
            if let Ok(mtime) = fs::metadata(&path).and_then(|m| m.modified()) {
                self.watched_files.insert(path, mtime);
            }
        }
        self.load_all()?;
        Ok(())
    }

    /// Loads a single Lua file and tracks it for hot reload.
    fn load_file(&mut self, path: &Path) -> Result<(), PluginError> {
        let source = fs::read_to_string(path)?;
        let chunk_name = format!("plugin:{}", path.display());

        if let Ok(mtime) = fs::metadata(path).and_then(|m| m.modified()) {
            self.watched_files.insert(path.to_path_buf(), mtime);
        }

        self.lua.load(&source).set_name(chunk_name).exec()?;
        Ok(())
    }

    /// Sets up the `terminal` Lua module with the plugin API.
    fn setup_terminal_module(&mut self) -> Result<(), PluginError> {
        let lua = &self.lua;
        let state = Rc::clone(&self.state);
        let terminal = lua.create_table()?;

        // terminal.bind("ctrl+t", function() ... end)
        {
            let state = Rc::clone(&state);
            let lua_clone = lua.clone();
            let bind_fn = lua.create_function(
                move |_lua: &Lua, (spec, callback): (String, Function)| {
                    let binding = KeyBinding::parse(&spec).ok_or_else(|| {
                        mlua::Error::runtime(format!("invalid keybinding spec: {spec}"))
                    })?;
                    let mut state = state.borrow_mut();
                    state.callback_counter += 1;
                    let registry_key = format!("binding_{}", state.callback_counter);
                    lua_clone.set_named_registry_value(&registry_key, callback)?;
                    state.bindings.insert(binding, registry_key);
                    Ok(())
                },
            )?;
            terminal.set("bind", bind_fn)?;
        }

        // terminal.command("id", "Title", function() ... end)
        {
            let state = Rc::clone(&state);
            let lua_clone = lua.clone();
            let command_fn = lua.create_function(
                move |_lua: &Lua, (id, title, callback): (String, String, Function)| {
                    let mut state = state.borrow_mut();
                    state.callback_counter += 1;
                    let registry_key = format!("command_{}", state.callback_counter);
                    lua_clone.set_named_registry_value(&registry_key, callback)?;
                    state.commands.insert(id, (title, registry_key));
                    Ok(())
                },
            )?;
            terminal.set("command", command_fn)?;
        }

        // terminal.set_theme({...})
        {
            let state = Rc::clone(&state);
            let theme_fn = lua.create_function(move |_lua: &Lua, table: Table| {
                let mut state = state.borrow_mut();
                state.theme = Theme::from_table(&table)?;
                Ok(())
            })?;
            terminal.set("set_theme", theme_fn)?;
        }

        // Tab actions (queue for host)
        macro_rules! queue_action {
            ($lua_name:literal, $variant:expr) => {{
                let state = Rc::clone(&state);
                let action_fn = lua.create_function(move |_lua: &Lua, ()| {
                    state.borrow_mut().action_queue.push($variant);
                    Ok(())
                })?;
                terminal.set($lua_name, action_fn)?;
            }};
        }
        queue_action!("new_tab", PluginAction::NewTab);
        queue_action!("close_tab", PluginAction::CloseTab);
        queue_action!("next_tab", PluginAction::NextTab);
        queue_action!("prev_tab", PluginAction::PrevTab);
        queue_action!("quit", PluginAction::Quit);
        queue_action!("clear_status", PluginAction::ClearStatusMessage);
        queue_action!("palette", PluginAction::OpenPalette);

        // terminal.rename_tab("name")
        {
            let state = Rc::clone(&state);
            let rename_fn = lua.create_function(move |_lua: &Lua, name: String| {
                state
                    .borrow_mut()
                    .action_queue
                    .push(PluginAction::RenameTab(name));
                Ok(())
            })?;
            terminal.set("rename_tab", rename_fn)?;
        }

        // terminal.set_status("message")
        {
            let state = Rc::clone(&state);
            let status_fn = lua.create_function(move |_lua: &Lua, msg: String| {
                state
                    .borrow_mut()
                    .action_queue
                    .push(PluginAction::SetStatusMessage(msg));
                Ok(())
            })?;
            terminal.set("set_status", status_fn)?;
        }

        // terminal.log("message")
        {
            let log_fn = lua.create_function(|_lua: &Lua, msg: String| {
                eprintln!("[plugin] {msg}");
                Ok(())
            })?;
            terminal.set("log", log_fn)?;
        }

        lua.globals().set("terminal", terminal)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "plugin_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos())
                .unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn keybinding_parse_handles_mods() {
        assert_eq!(
            KeyBinding::parse("ctrl+t"),
            Some(KeyBinding::new("ctrl", "t"))
        );
        assert_eq!(
            KeyBinding::parse("ctrl+shift+p"),
            Some(KeyBinding::new("ctrl+shift", "p"))
        );
        assert_eq!(
            KeyBinding::parse("shift+ctrl+p"),
            KeyBinding::parse("ctrl+shift+p")
        );
        assert_eq!(KeyBinding::parse(""), None);
        assert_eq!(KeyBinding::parse("ctrl+"), None);
    }

    #[test]
    fn default_theme_is_dark() {
        let theme = Theme::default();
        assert_eq!(theme.background, "#1a1a1e");
        assert_eq!(theme.foreground, "#d8d8d8");
    }

    #[test]
    fn plugin_registers_keybinding() {
        let dir = temp_dir();
        let init = dir.join("init.lua");
        let mut f = fs::File::create(&init).unwrap();
        writeln!(
            f,
            r#"terminal.bind("ctrl+t", function() terminal.log("pressed") end)"#
        )
        .unwrap();

        let mut manager = PluginManager::new(&dir).unwrap();
        manager.load_all().unwrap();

        assert!(manager.has_binding("ctrl", "t"));
        assert!(!manager.has_binding("ctrl", "w"));
        assert!(manager.invoke_binding("ctrl", "t").unwrap());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_registers_command() {
        let dir = temp_dir();
        let init = dir.join("init.lua");
        let mut f = fs::File::create(&init).unwrap();
        writeln!(
            f,
            r#"terminal.command("test:hello", "Say Hello", function() terminal.set_status("hi") end)"#
        )
        .unwrap();

        let mut manager = PluginManager::new(&dir).unwrap();
        manager.load_all().unwrap();

        let commands = manager.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].id, "test:hello");

        assert!(manager.invoke_command("test:hello").unwrap());
        let actions = manager.drain_actions();
        assert_eq!(
            actions,
            vec![PluginAction::SetStatusMessage("hi".to_string())]
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_sets_theme() {
        let dir = temp_dir();
        let init = dir.join("init.lua");
        let mut f = fs::File::create(&init).unwrap();
        writeln!(
            f,
            "terminal.set_theme({{ background = \"#000000\", accent = \"#ff0000\" }})"
        )
        .unwrap();

        let mut manager = PluginManager::new(&dir).unwrap();
        manager.load_all().unwrap();

        assert_eq!(manager.theme().background, "#000000");
        assert_eq!(manager.theme().accent, "#ff0000");
        assert_eq!(manager.theme().foreground, "#d8d8d8");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_queues_tab_actions() {
        let dir = temp_dir();
        let init = dir.join("init.lua");
        let mut f = fs::File::create(&init).unwrap();
        writeln!(
            f,
            r#"terminal.bind("ctrl+t", function() terminal.new_tab() end)"#
        )
        .unwrap();

        let mut manager = PluginManager::new(&dir).unwrap();
        manager.load_all().unwrap();
        manager.invoke_binding("ctrl", "t").unwrap();

        let actions = manager.drain_actions();
        assert_eq!(actions, vec![PluginAction::NewTab]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn loads_plugins_from_subdirectory() {
        let dir = temp_dir();
        let plugins = dir.join("plugins");
        fs::create_dir_all(&plugins).unwrap();

        let mut f = fs::File::create(plugins.join("a.lua")).unwrap();
        writeln!(f, r#"terminal.command("a:cmd", "A", function() end)"#).unwrap();
        let mut f = fs::File::create(plugins.join("b.lua")).unwrap();
        writeln!(f, r#"terminal.command("b:cmd", "B", function() end)"#).unwrap();

        let mut manager = PluginManager::new(&dir).unwrap();
        manager.load_all().unwrap();

        assert_eq!(manager.commands().len(), 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_binding_spec_is_lua_error() {
        let dir = temp_dir();
        let init = dir.join("init.lua");
        let mut f = fs::File::create(&init).unwrap();
        writeln!(f, r#"terminal.bind("", function() end)"#).unwrap();

        let mut manager = PluginManager::new(&dir).unwrap();
        assert!(manager.load_all().is_err());

        let _ = fs::remove_dir_all(&dir);
    }
}
