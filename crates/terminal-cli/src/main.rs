#![forbid(unsafe_code)]

mod mouse;

use std::env;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
};
use crossterm::terminal::{
    self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
};
use crossterm::{execute, queue};
use local_pty::{
    LocalProcessSpec, LocalPtyEndpoint, SshPtyEndpoint, SshPtySpec, TmuxPtyEndpoint, TmuxPtySpec,
};
use terminal_core::{
    Cell, CellAttributes, CellStyle, Cursor, Endpoint, EndpointEvent, EndpointId, EndpointSize,
    TerminalColor, TerminalState,
};
use user_config::TrustedUserConfig;

const SCROLLBACK_LIMIT: usize = 10_000;
/// Fallback size used when the host terminal cannot report its own
/// dimensions at startup (e.g. `TIOCGWINSZ` returns 0x0 because no window
/// size has been set yet on the controlling tty). This mirrors the classic
/// VT100 default that other terminal tools (vim, tmux, less, ...) fall back
/// to in the same situation, so we start rendering instead of aborting; a
/// real `Event::Resize` from the host corrects it as soon as one arrives.
const DEFAULT_HOST_SIZE: (u16, u16) = (80, 24);
const LOOP_IDLE: Duration = Duration::from_millis(5);
const BRACKETED_PASTE_BEGIN: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

#[derive(Debug)]
struct Options {
    config_path: Option<PathBuf>,
    ssh_target: Option<String>,
    tmux_session: Option<String>,
    command: Vec<OsString>,
}

struct Frontend {
    keyboard_enhancement: bool,
    mouse_capture: bool,
}

impl Frontend {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            Hide,
            Clear(ClearType::All)
        ) {
            let _ = execute!(stdout, DisableBracketedPaste, Show, LeaveAlternateScreen);
            let _ = terminal::disable_raw_mode();
            return Err(error);
        }

        let keyboard_enhancement = match execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        ) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::Unsupported => false,
            Err(error) => {
                let _ = execute!(stdout, DisableBracketedPaste, Show, LeaveAlternateScreen);
                let _ = terminal::disable_raw_mode();
                return Err(error);
            }
        };
        Ok(Self {
            keyboard_enhancement,
            mouse_capture: false,
        })
    }

    fn sync_mouse_capture(&mut self, stdout: &mut impl Write, requested: bool) -> io::Result<()> {
        let Some(enabled) = mouse::capture_transition(self.mouse_capture, requested) else {
            return Ok(());
        };
        if enabled {
            execute!(stdout, EnableMouseCapture)?;
        } else {
            execute!(stdout, DisableMouseCapture)?;
        }
        self.mouse_capture = enabled;
        Ok(())
    }
}

impl Drop for Frontend {
    fn drop(&mut self) {
        let mut stdout = io::stdout();
        if self.mouse_capture {
            let _ = execute!(stdout, DisableMouseCapture);
        }
        if self.keyboard_enhancement {
            let _ = execute!(stdout, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(
            stdout,
            DisableBracketedPaste,
            SetAttribute(Attribute::Reset),
            ResetColor,
            Show,
            LeaveAlternateScreen
        );
        let _ = terminal::disable_raw_mode();
    }
}

fn parse_options_from(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let mut args = args.into_iter();
    let mut config_path = None;
    let mut ssh_target = None;
    let mut tmux_session = None;
    let mut command = Vec::new();

    while let Some(arg) = args.next() {
        if arg == "--config" {
            config_path = Some(PathBuf::from(
                args.next().ok_or("--config requires a path")?,
            ));
        } else if arg == "--ssh" {
            if ssh_target.is_some() {
                return Err("--ssh may only be specified once".to_owned());
            }
            let target = args.next().ok_or("--ssh requires a target")?;
            ssh_target = Some(
                target
                    .into_string()
                    .map_err(|_| "--ssh target must be valid Unicode")?,
            );
        } else if arg == "--tmux" {
            if tmux_session.is_some() {
                return Err("--tmux may only be specified once".to_owned());
            }
            let session = args.next().ok_or("--tmux requires a session name")?;
            tmux_session = Some(
                session
                    .into_string()
                    .map_err(|_| "--tmux session name must be valid Unicode")?,
            );
        } else if arg == "--" {
            command.extend(args);
            break;
        } else if arg == "--help" || arg == "-h" {
            return Err(
                "usage: terminal-cli [--config PATH] [--ssh TARGET | --tmux SESSION | -- PROGRAM [ARGS...]]"
                    .to_owned(),
            );
        } else {
            return Err(format!("unknown option: {}", arg.to_string_lossy()));
        }
    }

    if ssh_target.is_some() && tmux_session.is_some() {
        return Err("--ssh and --tmux cannot be combined".to_owned());
    }
    if ssh_target.is_some() && !command.is_empty() {
        return Err("--ssh cannot be combined with -- PROGRAM [ARGS...]".to_owned());
    }
    if tmux_session.is_some() && !command.is_empty() {
        return Err("--tmux cannot be combined with -- PROGRAM [ARGS...]".to_owned());
    }

    Ok(Options {
        config_path,
        ssh_target,
        tmux_session,
        command,
    })
}

fn parse_options() -> Result<Options, String> {
    parse_options_from(env::args_os().skip(1))
}

fn default_shell() -> OsString {
    env::var_os("SHELL")
        .or_else(|| env::var_os("COMSPEC"))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "cmd.exe".into()
            } else {
                "/bin/sh".into()
            }
        })
}

fn indexed_color(index: u8) -> Color {
    match index {
        0 => Color::Black,
        1 => Color::DarkRed,
        2 => Color::DarkGreen,
        3 => Color::DarkYellow,
        4 => Color::DarkBlue,
        5 => Color::DarkMagenta,
        6 => Color::DarkCyan,
        7 => Color::Grey,
        value => Color::AnsiValue(value),
    }
}

fn terminal_color(color: TerminalColor) -> Option<Color> {
    match color {
        TerminalColor::Default => None,
        TerminalColor::Indexed(index) => Some(indexed_color(index)),
        TerminalColor::Rgb { r, g, b } => Some(Color::Rgb { r, g, b }),
    }
}

fn apply_style(stdout: &mut impl Write, style: CellStyle) -> io::Result<()> {
    queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    if style.attributes.contains(CellAttributes::BOLD) {
        queue!(stdout, SetAttribute(Attribute::Bold))?;
    }
    if style.attributes.contains(CellAttributes::ITALIC) {
        queue!(stdout, SetAttribute(Attribute::Italic))?;
    }
    if style.attributes.contains(CellAttributes::UNDERLINE) {
        queue!(stdout, SetAttribute(Attribute::Underlined))?;
    }
    if style.attributes.contains(CellAttributes::INVERSE) {
        queue!(stdout, SetAttribute(Attribute::Reverse))?;
    }
    if let Some(color) = terminal_color(style.foreground) {
        queue!(stdout, SetForegroundColor(color))?;
    }
    if let Some(color) = terminal_color(style.background) {
        queue!(stdout, SetBackgroundColor(color))?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct RenderCache {
    size: Option<EndpointSize>,
    cells: Vec<Cell>,
    cursor: Cursor,
    cursor_visible: bool,
    initialized: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RenderStats {
    full_redraw: bool,
    cells_emitted: usize,
}

fn render(
    stdout: &mut impl Write,
    state: &TerminalState,
    cache: &mut RenderCache,
) -> io::Result<RenderStats> {
    let size = state.size();
    let cell_count = usize::from(size.rows) * usize::from(size.cols);
    let geometry_changed = cache.size != Some(size) || cache.cells.len() != cell_count;
    let full_redraw = !cache.initialized || geometry_changed;

    if geometry_changed {
        cache.cells.clear();
        cache.cells.resize(cell_count, Cell::default());
    }

    let cursor = state.cursor();
    let cursor_changed = !cache.initialized
        || cache.cursor != cursor
        || cache.cursor_visible != state.cursor_visible();
    let mut output_started = false;
    let mut cells_emitted = 0;
    let mut last_style = None;

    if full_redraw {
        queue!(stdout, Hide, Clear(ClearType::All))?;
        output_started = true;
    }

    for row in 0..size.rows {
        for col in 0..size.cols {
            let index = usize::from(row) * usize::from(size.cols) + usize::from(col);
            let cell = state
                .cell(row, col)
                .expect("renderer iterates only within terminal geometry");
            if !full_redraw && cache.cells[index] == cell {
                continue;
            }
            cache.cells[index] = cell;

            if cell.character == '\0' {
                continue;
            }

            if !output_started {
                queue!(stdout, Hide)?;
                output_started = true;
            }
            queue!(stdout, MoveTo(col, row))?;
            if last_style != Some(cell.style) {
                apply_style(stdout, cell.style)?;
                last_style = Some(cell.style);
            }
            queue!(stdout, Print(cell.display_character()))?;
            for &mark in cell.combining_marks() {
                queue!(stdout, Print(mark))?;
            }
            cells_emitted += 1;
        }
    }

    if cells_emitted > 0 {
        queue!(stdout, SetAttribute(Attribute::Reset), ResetColor)?;
    }

    if output_started || cursor_changed {
        let col = cursor.col.min(size.cols.saturating_sub(1));
        let row = cursor.row.min(size.rows.saturating_sub(1));
        queue!(stdout, MoveTo(col, row))?;
        if state.cursor_visible() {
            queue!(stdout, Show)?;
        } else {
            queue!(stdout, Hide)?;
        }
        stdout.flush()?;
    }

    cache.size = Some(size);
    cache.cursor = cursor;
    cache.cursor_visible = state.cursor_visible();
    cache.initialized = true;

    Ok(RenderStats {
        full_redraw,
        cells_emitted,
    })
}

fn finalize_terminal_stream(
    stdout: &mut impl Write,
    state: &mut TerminalState,
    cache: &mut RenderCache,
) -> io::Result<()> {
    state.finish();
    render(stdout, state, cache)?;
    Ok(())
}

fn control_byte(character: char) -> Option<u8> {
    match character {
        '@' | ' ' => Some(0x00),
        'a'..='z' => Some((character as u8) - b'a' + 1),
        'A'..='Z' => Some((character as u8) - b'A' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn function_key_bytes(number: u8) -> Option<&'static [u8]> {
    match number {
        1 => Some(b"\x1bOP"),
        2 => Some(b"\x1bOQ"),
        3 => Some(b"\x1bOR"),
        4 => Some(b"\x1bOS"),
        5 => Some(b"\x1b[15~"),
        6 => Some(b"\x1b[17~"),
        7 => Some(b"\x1b[18~"),
        8 => Some(b"\x1b[19~"),
        9 => Some(b"\x1b[20~"),
        10 => Some(b"\x1b[21~"),
        11 => Some(b"\x1b[23~"),
        12 => Some(b"\x1b[24~"),
        _ => None,
    }
}

fn application_keypad_bytes(code: KeyCode) -> Option<&'static [u8]> {
    match code {
        KeyCode::Char('0') | KeyCode::Insert => Some(b"\x1bOp"),
        KeyCode::Char('1') | KeyCode::End => Some(b"\x1bOq"),
        KeyCode::Char('2') | KeyCode::Down => Some(b"\x1bOr"),
        KeyCode::Char('3') | KeyCode::PageDown => Some(b"\x1bOs"),
        KeyCode::Char('4') | KeyCode::Left => Some(b"\x1bOt"),
        KeyCode::Char('5') | KeyCode::KeypadBegin => Some(b"\x1bOu"),
        KeyCode::Char('6') | KeyCode::Right => Some(b"\x1bOv"),
        KeyCode::Char('7') | KeyCode::Home => Some(b"\x1bOw"),
        KeyCode::Char('8') | KeyCode::Up => Some(b"\x1bOx"),
        KeyCode::Char('9') | KeyCode::PageUp => Some(b"\x1bOy"),
        KeyCode::Char('*') => Some(b"\x1bOj"),
        KeyCode::Char('+') => Some(b"\x1bOk"),
        KeyCode::Char(',') => Some(b"\x1bOl"),
        KeyCode::Char('-') => Some(b"\x1bOm"),
        KeyCode::Char('.') | KeyCode::Delete => Some(b"\x1bOn"),
        KeyCode::Char('/') => Some(b"\x1bOo"),
        KeyCode::Char('=') => Some(b"\x1bOX"),
        KeyCode::Enter => Some(b"\x1bOM"),
        _ => None,
    }
}

fn key_bytes(
    key: KeyEvent,
    application_cursor_keys: bool,
    application_keypad: bool,
) -> Option<Vec<u8>> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    let keypad_sequence = application_keypad
        && key.state.contains(KeyEventState::KEYPAD)
        && !key.state.contains(KeyEventState::NUM_LOCK)
        && application_keypad_bytes(key.code).is_some();

    let cursor_prefix: &[u8] = if application_cursor_keys {
        b"\x1bO"
    } else {
        b"\x1b["
    };
    let mut bytes = if keypad_sequence {
        application_keypad_bytes(key.code)?.to_vec()
    } else {
        match key.code {
            KeyCode::Char(character) => {
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    control_byte(character)
                        .map_or_else(|| character.to_string().into_bytes(), |byte| vec![byte])
                } else {
                    character.to_string().into_bytes()
                }
            }
            KeyCode::Enter => vec![b'\r'],
            KeyCode::Tab => vec![b'\t'],
            KeyCode::BackTab => b"\x1b[Z".to_vec(),
            KeyCode::Backspace => vec![0x7f],
            KeyCode::Esc => vec![0x1b],
            KeyCode::Up => [cursor_prefix, &b"A"[..]].concat(),
            KeyCode::Down => [cursor_prefix, &b"B"[..]].concat(),
            KeyCode::Right => [cursor_prefix, &b"C"[..]].concat(),
            KeyCode::Left => [cursor_prefix, &b"D"[..]].concat(),
            KeyCode::Home => [cursor_prefix, &b"H"[..]].concat(),
            KeyCode::End => [cursor_prefix, &b"F"[..]].concat(),
            KeyCode::PageUp => b"\x1b[5~".to_vec(),
            KeyCode::PageDown => b"\x1b[6~".to_vec(),
            KeyCode::Insert => b"\x1b[2~".to_vec(),
            KeyCode::Delete => b"\x1b[3~".to_vec(),
            KeyCode::F(number) => function_key_bytes(number)?.to_vec(),
            _ => return None,
        }
    };

    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    Some(bytes)
}

fn paste_bytes(text: &str, bracketed: bool) -> Vec<u8> {
    if !bracketed {
        return text.as_bytes().to_vec();
    }

    let mut bytes =
        Vec::with_capacity(text.len() + BRACKETED_PASTE_BEGIN.len() + BRACKETED_PASTE_END.len());
    bytes.extend_from_slice(BRACKETED_PASTE_BEGIN);
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(BRACKETED_PASTE_END);
    bytes
}

fn host_size() -> Result<EndpointSize, Box<dyn std::error::Error>> {
    let (cols, rows) = terminal::size()?;
    if let Ok(size) = EndpointSize::new(cols, rows) {
        return Ok(size);
    }
    // A freshly-created or not-yet-sized controlling tty (some multiplexers,
    // embedded/IDE terminals, and PTYs that haven't had TIOCSWINSZ applied
    // yet all do this) reports 0x0 rather than a real size. Rather than
    // aborting before a single frame is drawn, start at a sane default; the
    // resize handler in `run()` already treats any subsequent invalid
    // `Event::Resize` as a no-op, so this just extends that same tolerance
    // to the initial size.
    let (default_cols, default_rows) = DEFAULT_HOST_SIZE;
    Ok(EndpointSize::new(default_cols, default_rows).expect("DEFAULT_HOST_SIZE is a valid non-zero size"))
}

fn build_endpoint(
    options: Options,
    config: TrustedUserConfig,
) -> Result<Box<dyn Endpoint>, Box<dyn std::error::Error>> {
    let Options {
        ssh_target,
        tmux_session,
        command,
        ..
    } = options;
    if let Some(target) = ssh_target {
        let mut spec = SshPtySpec::new(target, config.initial_size)?;
        if let Some(term) = config.term {
            spec = spec.with_env("TERM", term);
        }
        if let Some(colorterm) = config.colorterm {
            spec = spec.with_env("COLORTERM", colorterm);
        }
        return Ok(Box::new(SshPtyEndpoint::new(
            EndpointId::new("ssh:cli")?,
            spec,
        )));
    }
    if let Some(session) = tmux_session {
        let mut spec = TmuxPtySpec::new(session, config.initial_size)?;
        if let Some(term) = config.term {
            spec = spec.with_env("TERM", term);
        }
        if let Some(colorterm) = config.colorterm {
            spec = spec.with_env("COLORTERM", colorterm);
        }
        return Ok(Box::new(TmuxPtyEndpoint::new(
            EndpointId::new("tmux:cli")?,
            spec,
        )));
    }

    let explicit_command = !command.is_empty();
    let mut command = command.into_iter();
    let program = command
        .next()
        .or_else(|| config.shell.clone().map(Into::into))
        .unwrap_or_else(default_shell);
    let mut spec = LocalProcessSpec::new(program, config.initial_size)?;
    if explicit_command {
        for arg in command {
            spec = spec.arg(arg);
        }
    } else {
        for arg in config.shell_args {
            spec = spec.arg(arg);
        }
    }
    if let Some(term) = config.term {
        spec = spec.with_env("TERM", term);
    }
    if let Some(colorterm) = config.colorterm {
        spec = spec.with_env("COLORTERM", colorterm);
    }
    Ok(Box::new(LocalPtyEndpoint::new(
        EndpointId::new("local:cli")?,
        spec,
    )))
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    let options = match parse_options() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return Ok(if message.starts_with("usage:") { 0 } else { 2 });
        }
    };
    let config = options
        .config_path
        .as_ref()
        .map(TrustedUserConfig::from_path)
        .transpose()?
        .unwrap_or_default();
    let mut endpoint = build_endpoint(options, config)?;
    endpoint.attach()?;

    let mut frontend = Frontend::enter()?;
    let size = host_size()?;
    endpoint.resize(size)?;
    let mut state = TerminalState::new(size, SCROLLBACK_LIMIT);
    let mut stdout = io::stdout();
    let mut render_cache = RenderCache::default();
    render(&mut stdout, &state, &mut render_cache)?;

    loop {
        let mut did_work = false;

        if event::poll(Duration::ZERO)? {
            did_work = true;
            match event::read()? {
                Event::Key(key) => {
                    let cursor_mode = state.application_cursor_keys();
                    let keypad_mode = state.application_keypad_mode();
                    if let Some(bytes) = key_bytes(key, cursor_mode, keypad_mode) {
                        endpoint.send(&bytes)?;
                    }
                }
                Event::Paste(text) => {
                    let bytes = paste_bytes(&text, state.bracketed_paste_mode());
                    endpoint.send(&bytes)?;
                }
                Event::Resize(cols, rows) => {
                    if let Ok(size) = EndpointSize::new(cols, rows) {
                        endpoint.resize(size)?;
                        state.resize(size);
                        render(&mut stdout, &state, &mut render_cache)?;
                    }
                }
                Event::Mouse(mouse_event) => {
                    if let Some(bytes) = mouse::mouse_bytes(
                        mouse_event,
                        state.mouse_tracking_mode(),
                        state.sgr_mouse_mode(),
                    ) {
                        endpoint.send(&bytes)?;
                    }
                }
                Event::FocusGained | Event::FocusLost => {}
            }
        }

        let endpoint_event = match endpoint.poll_event() {
            Ok(event) => event,
            Err(error) => {
                finalize_terminal_stream(&mut stdout, &mut state, &mut render_cache)?;
                return Err(error.into());
            }
        };
        match endpoint_event {
            Some(EndpointEvent::Output(bytes)) => {
                did_work = true;
                let mut responses = Vec::new();
                state.feed_with_responses(&bytes, |reply| responses.extend_from_slice(reply));
                frontend.sync_mouse_capture(&mut stdout, state.mouse_reporting_enabled())?;
                if !responses.is_empty() {
                    endpoint.send(&responses)?;
                }
                render(&mut stdout, &state, &mut render_cache)?;
            }
            Some(EndpointEvent::Bell) => {
                did_work = true;
                stdout.write_all(b"\x07")?;
                stdout.flush()?;
            }
            Some(EndpointEvent::TitleChanged(title)) => {
                did_work = true;
                queue!(stdout, SetTitle(title))?;
                stdout.flush()?;
            }
            Some(EndpointEvent::Exited { code, .. }) => {
                finalize_terminal_stream(&mut stdout, &mut state, &mut render_cache)?;
                return Ok(code.unwrap_or(1));
            }
            Some(EndpointEvent::Disconnected { reason }) => {
                finalize_terminal_stream(&mut stdout, &mut state, &mut render_cache)?;
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, reason).into());
            }
            None => {}
        }

        if !did_work {
            std::thread::sleep(LOOP_IDLE);
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let exit_code = run()?;
    std::process::exit(exit_code.clamp(0, 255));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn keypad_key(code: KeyCode, state: KeyEventState) -> KeyEvent {
        KeyEvent::new_with_kind_and_state(code, KeyModifiers::NONE, KeyEventKind::Press, state)
    }

    fn options(args: &[&str]) -> Result<Options, String> {
        parse_options_from(args.iter().map(OsString::from))
    }

    #[test]
    fn parses_explicit_ssh_target_without_shell_interpolation() {
        let parsed = options(&["--config", "config.lua", "--ssh", "alice@example.test"]).unwrap();
        assert_eq!(parsed.config_path, Some(PathBuf::from("config.lua")));
        assert_eq!(parsed.ssh_target.as_deref(), Some("alice@example.test"));
        assert_eq!(parsed.tmux_session, None);
        assert!(parsed.command.is_empty());
        assert!(options(&["--ssh", "example.test", "--", "echo"]).is_err());
        assert!(options(&["--ssh"]).is_err());
    }

    #[test]
    fn parses_tmux_session_and_rejects_provider_conflicts() {
        let parsed = options(&["--config", "config.lua", "--tmux", "work"]).unwrap();
        assert_eq!(parsed.config_path, Some(PathBuf::from("config.lua")));
        assert_eq!(parsed.tmux_session.as_deref(), Some("work"));
        assert_eq!(parsed.ssh_target, None);
        assert!(parsed.command.is_empty());
        assert!(options(&["--tmux", "work", "--", "echo"]).is_err());
        assert!(options(&["--ssh", "host", "--tmux", "work"]).is_err());
        assert!(options(&["--tmux"]).is_err());
    }

    #[test]
    fn maps_core_navigation_keys_to_vt_sequences() {
        assert_eq!(
            key_bytes(key(KeyCode::Up, KeyModifiers::NONE), false, false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Home, KeyModifiers::NONE), false, false).unwrap(),
            b"\x1b[H"
        );
        assert_eq!(
            key_bytes(key(KeyCode::End, KeyModifiers::NONE), false, false).unwrap(),
            b"\x1b[F"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Delete, KeyModifiers::NONE), false, false).unwrap(),
            b"\x1b[3~"
        );
        assert_eq!(
            key_bytes(key(KeyCode::F(5), KeyModifiers::NONE), false, false).unwrap(),
            b"\x1b[15~"
        );
    }

    #[test]
    fn application_cursor_mode_uses_ss3_for_cursor_keys() {
        assert_eq!(
            key_bytes(key(KeyCode::Up, KeyModifiers::NONE), true, false).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Down, KeyModifiers::NONE), true, false).unwrap(),
            b"\x1bOB"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Right, KeyModifiers::NONE), true, false).unwrap(),
            b"\x1bOC"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Left, KeyModifiers::NONE), true, false).unwrap(),
            b"\x1bOD"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Home, KeyModifiers::NONE), true, false).unwrap(),
            b"\x1bOH"
        );
        assert_eq!(
            key_bytes(key(KeyCode::End, KeyModifiers::NONE), true, false).unwrap(),
            b"\x1bOF"
        );
    }

    #[test]
    fn application_keypad_requires_truthful_keypad_provenance() {
        let keypad = KeyEventState::KEYPAD;
        assert_eq!(
            key_bytes(keypad_key(KeyCode::Char('1'), keypad), false, true).unwrap(),
            b"\x1bOq"
        );
        assert_eq!(
            key_bytes(keypad_key(KeyCode::Enter, keypad), false, true).unwrap(),
            b"\x1bOM"
        );
        assert_eq!(
            key_bytes(keypad_key(KeyCode::Up, keypad), false, true).unwrap(),
            b"\x1bOx"
        );
        assert_eq!(
            key_bytes(keypad_key(KeyCode::Char('='), keypad), false, true).unwrap(),
            b"\x1bOX"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Char('1'), KeyModifiers::NONE), false, true).unwrap(),
            b"1"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Char('='), KeyModifiers::NONE), false, true).unwrap(),
            b"="
        );
    }

    #[test]
    fn num_lock_overrides_application_keypad_mode() {
        let state = KeyEventState::KEYPAD | KeyEventState::NUM_LOCK;
        assert_eq!(
            key_bytes(keypad_key(KeyCode::Char('7'), state), false, true).unwrap(),
            b"7"
        );
    }

    #[test]
    fn application_keypad_is_independent_from_application_cursor_mode() {
        let keypad = KeyEventState::KEYPAD;
        assert_eq!(
            key_bytes(keypad_key(KeyCode::Up, keypad), false, true).unwrap(),
            b"\x1bOx"
        );
        assert_eq!(
            key_bytes(key(KeyCode::Up, KeyModifiers::NONE), true, true).unwrap(),
            b"\x1bOA"
        );
    }

    #[test]
    fn maps_control_and_alt_character_input() {
        assert_eq!(
            key_bytes(key(KeyCode::Char('c'), KeyModifiers::CONTROL), false, false).unwrap(),
            vec![0x03]
        );
        assert_eq!(
            key_bytes(key(KeyCode::Char('x'), KeyModifiers::ALT), false, false).unwrap(),
            vec![0x1b, b'x']
        );
    }

    #[test]
    fn bracketed_paste_adds_only_fixed_delimiters() {
        assert_eq!(paste_bytes("hello\nworld", false), b"hello\nworld");
        assert_eq!(
            paste_bytes("hello\nworld", true),
            b"\x1b[200~hello\nworld\x1b[201~"
        );
        assert_eq!(paste_bytes("", true), b"\x1b[200~\x1b[201~");
    }

    #[test]
    fn renders_indexed_and_rgb_terminal_colors_without_palette_flattening() {
        assert_eq!(
            terminal_color(TerminalColor::Indexed(196)),
            Some(Color::AnsiValue(196))
        );
        assert_eq!(
            terminal_color(TerminalColor::Rgb {
                r: 12,
                g: 34,
                b: 56
            }),
            Some(Color::Rgb {
                r: 12,
                g: 34,
                b: 56
            })
        );
        assert_eq!(terminal_color(TerminalColor::Default), None);
    }

    #[test]
    fn renderer_emits_core_emphasis_without_rewriting_colors() {
        let mut output = Vec::new();
        let style = CellStyle {
            attributes: CellAttributes::BOLD
                .with(CellAttributes::ITALIC)
                .with(CellAttributes::UNDERLINE)
                .with(CellAttributes::INVERSE),
            foreground: TerminalColor::Rgb {
                r: 10,
                g: 20,
                b: 30,
            },
            background: TerminalColor::Indexed(42),
        };
        apply_style(&mut output, style).unwrap();
        for sequence in [b"\x1b[1m".as_slice(), b"\x1b[3m", b"\x1b[4m", b"\x1b[7m"] {
            assert!(
                output
                    .windows(sequence.len())
                    .any(|window| window == sequence)
            );
        }
        assert_eq!(
            style.foreground,
            TerminalColor::Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
        assert_eq!(style.background, TerminalColor::Indexed(42));
    }

    #[test]
    fn damage_renderer_skips_unchanged_cells_and_bounds_small_updates() {
        let size = EndpointSize::new(80, 24).unwrap();
        let mut state = TerminalState::new(size, 0);
        let mut cache = RenderCache::default();

        let mut full_output = Vec::new();
        let full = render(&mut full_output, &state, &mut cache).unwrap();
        assert!(full.full_redraw);
        assert_eq!(full.cells_emitted, 80 * 24);

        state.feed(b"x");
        let mut damage_output = Vec::new();
        let damage = render(&mut damage_output, &state, &mut cache).unwrap();
        assert!(!damage.full_redraw);
        assert_eq!(damage.cells_emitted, 1);
        assert!(damage_output.len() * 10 < full_output.len());

        let mut unchanged_output = Vec::new();
        let unchanged = render(&mut unchanged_output, &state, &mut cache).unwrap();
        assert_eq!(unchanged.cells_emitted, 0);
        assert!(unchanged_output.is_empty());
    }

    #[test]
    fn damage_renderer_handles_cursor_only_changes_without_repainting_cells() {
        let size = EndpointSize::new(8, 4).unwrap();
        let mut state = TerminalState::new(size, 0);
        let mut cache = RenderCache::default();
        render(&mut Vec::new(), &state, &mut cache).unwrap();

        state.feed(b"\x1b[3;4H");
        let mut output = Vec::new();
        let render_result = render(&mut output, &state, &mut cache).unwrap();
        assert_eq!(render_result.cells_emitted, 0);
        assert!(!output.is_empty());
    }

    #[test]
    fn damage_renderer_preserves_wide_cells_and_clears_old_continuations() {
        let size = EndpointSize::new(4, 1).unwrap();
        let mut state = TerminalState::new(size, 0);
        let mut cache = RenderCache::default();
        render(&mut Vec::new(), &state, &mut cache).unwrap();

        state.feed("界".as_bytes());
        let mut wide_output = Vec::new();
        let wide = render(&mut wide_output, &state, &mut cache).unwrap();
        assert_eq!(wide.cells_emitted, 1);
        assert!(
            wide_output
                .windows("界".len())
                .any(|window| window == "界".as_bytes())
        );

        state.feed(b"\rA");
        let mut narrow_output = Vec::new();
        let narrow = render(&mut narrow_output, &state, &mut cache).unwrap();
        assert_eq!(narrow.cells_emitted, 2);
        assert!(narrow_output.contains(&b'A'));
    }

    #[test]
    fn damage_renderer_forces_full_redraw_on_resize() {
        let mut state = TerminalState::new(EndpointSize::new(4, 2).unwrap(), 0);
        let mut cache = RenderCache::default();
        render(&mut Vec::new(), &state, &mut cache).unwrap();

        state.resize(EndpointSize::new(6, 3).unwrap());
        let render_result = render(&mut Vec::new(), &state, &mut cache).unwrap();
        assert!(render_result.full_redraw);
        assert_eq!(render_result.cells_emitted, 18);
    }

    #[test]
    fn damage_renderer_tracks_bce_and_alternate_screen_changes() {
        let size = EndpointSize::new(6, 2).unwrap();
        let mut state = TerminalState::new(size, 0);
        let mut cache = RenderCache::default();
        render(&mut Vec::new(), &state, &mut cache).unwrap();

        state.feed(b"\x1b[41mAB\x1b[2K");
        let bce = render(&mut Vec::new(), &state, &mut cache).unwrap();
        assert!(!bce.full_redraw);
        assert!(bce.cells_emitted >= usize::from(size.cols));

        state.feed(b"\x1b[?1049h");
        let alternate = render(&mut Vec::new(), &state, &mut cache).unwrap();
        assert!(!alternate.full_redraw);
        assert!(alternate.cells_emitted >= usize::from(size.cols));
    }

    #[test]
    fn finalization_renders_a_pending_utf8_replacement_before_teardown() {
        let size = EndpointSize::new(4, 2).unwrap();
        let mut state = TerminalState::new(size, 0);
        let mut cache = RenderCache::default();
        render(&mut Vec::new(), &state, &mut cache).unwrap();
        state.feed(b"A\xe2\x98");

        let mut output = Vec::new();
        finalize_terminal_stream(&mut output, &mut state, &mut cache).unwrap();
        assert_eq!(state.cell(0, 0).unwrap().character, 'A');
        assert_eq!(state.cell(0, 1).unwrap().character, '\u{fffd}');
        assert!(!output.is_empty());
    }

    #[test]
    fn ignores_key_release_events() {
        let mut event = key(KeyCode::Char('x'), KeyModifiers::NONE);
        event.kind = KeyEventKind::Release;
        assert!(key_bytes(event, false, false).is_none());
    }
}
