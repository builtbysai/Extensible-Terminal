#![forbid(unsafe_code)]

use std::fmt;
use std::fs;
use std::path::Path;

use mlua::{Lua, Table, Value};
use terminal_core::EndpointSize;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedUserConfig {
    pub shell: Option<String>,
    pub shell_args: Vec<String>,
    pub term: Option<String>,
    pub colorterm: Option<String>,
    pub initial_size: EndpointSize,
}

impl Default for TrustedUserConfig {
    fn default() -> Self {
        Self {
            shell: None,
            shell_args: Vec::new(),
            term: None,
            colorterm: None,
            initial_size: EndpointSize::new(80, 24).expect("default size is valid"),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    Lua(mlua::Error),
    InvalidRoot,
    InvalidSize { cols: i64, rows: i64 },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "cannot read trusted Lua config: {error}"),
            Self::Lua(error) => write!(f, "trusted Lua config failed: {error}"),
            Self::InvalidRoot => write!(f, "trusted Lua config must return a table"),
            Self::InvalidSize { cols, rows } => {
                write!(f, "terminal size must be between 1 and 65535, got {cols}x{rows}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<mlua::Error> for ConfigError {
    fn from(value: mlua::Error) -> Self {
        Self::Lua(value)
    }
}

impl TrustedUserConfig {
    /// Executes user-owned Lua and reads the returned declarative terminal settings.
    ///
    /// Trusted configuration is intentionally distinct from third-party extensions:
    /// it may execute normal Lua standard-library operations and therefore must never
    /// be used to load untrusted plugin code.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Lua`] when the source cannot be evaluated, [`ConfigError::InvalidRoot`]
    /// when it does not return a table, or [`ConfigError::InvalidSize`] for invalid dimensions.
    pub fn from_source(source: &str) -> Result<Self, ConfigError> {
        let lua = Lua::new();
        let value: Value = lua.load(source).set_name("trusted-user-config").eval()?;
        let Value::Table(table) = value else {
            return Err(ConfigError::InvalidRoot);
        };
        Self::from_table(&table)
    }

    /// Loads and executes trusted user configuration from a UTF-8 file.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::Io`] when the file cannot be read, or propagates the validation and
    /// Lua evaluation errors documented by [`Self::from_source`].
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let source = fs::read_to_string(path)?;
        Self::from_source(&source)
    }

    fn from_table(table: &Table) -> Result<Self, ConfigError> {
        let shell = table.get::<Option<String>>("shell")?;
        let term = table.get::<Option<String>>("term")?;
        let colorterm = table.get::<Option<String>>("colorterm")?;
        let shell_args = match table.get::<Option<Table>>("shell_args")? {
            Some(args) => args.sequence_values::<String>().collect::<Result<Vec<_>, _>>()?,
            None => Vec::new(),
        };
        let cols = table.get::<Option<i64>>("cols")?.unwrap_or(80);
        let rows = table.get::<Option<i64>>("rows")?.unwrap_or(24);
        let (Ok(cols), Ok(rows)) = (u16::try_from(cols), u16::try_from(rows)) else {
            return Err(ConfigError::InvalidSize { cols, rows });
        };
        let initial_size = EndpointSize::new(cols, rows)
            .map_err(|_| ConfigError::InvalidSize { cols: i64::from(cols), rows: i64::from(rows) })?;

        Ok(Self { shell, shell_args, term, colorterm, initial_size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_table_uses_safe_defaults() {
        let config = TrustedUserConfig::from_source("return {}").expect("valid config");
        assert_eq!(config, TrustedUserConfig::default());
    }

    #[test]
    fn parses_shell_environment_and_size() {
        let config = TrustedUserConfig::from_source(
            r#"return {
                shell = "/bin/sh",
                shell_args = { "-l" },
                term = "xterm-256color",
                colorterm = "truecolor",
                cols = 120,
                rows = 40,
            }"#,
        )
        .expect("valid config");

        assert_eq!(config.shell.as_deref(), Some("/bin/sh"));
        assert_eq!(config.shell_args, ["-l"]);
        assert_eq!(config.term.as_deref(), Some("xterm-256color"));
        assert_eq!(config.initial_size, EndpointSize::new(120, 40).unwrap());
    }

    #[test]
    fn rejects_non_table_root() {
        assert!(matches!(TrustedUserConfig::from_source("return 42"), Err(ConfigError::InvalidRoot)));
    }

    #[test]
    fn rejects_zero_or_out_of_range_size() {
        assert!(matches!(
            TrustedUserConfig::from_source("return { cols = 0, rows = 24 }") ,
            Err(ConfigError::InvalidSize { .. })
        ));
        assert!(matches!(
            TrustedUserConfig::from_source("return { cols = 70000, rows = 24 }") ,
            Err(ConfigError::InvalidSize { .. })
        ));
    }
}
