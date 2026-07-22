use std::fmt;

use nmt_config::ConfigError;

#[derive(Clone, Copy, PartialEq)]
pub enum TerminalErrorLevel {
    Warning,
    Error,
}

#[derive(Clone)]
pub struct TerminalError {
    pub report: TerminalErrorType,
    pub level: TerminalErrorLevel,
}

impl TerminalError {
    pub fn configuration_not_found() -> Self {
        TerminalError {
            level: TerminalErrorLevel::Warning,
            report: TerminalErrorType::ConfigurationNotFound,
        }
    }
}

impl From<ConfigError> for TerminalError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::ErrLoadingConfig(message) => TerminalError {
                report: TerminalErrorType::InvalidConfigurationFormat(message),
                level: TerminalErrorLevel::Warning,
            },
            ConfigError::ErrLoadingTheme(message) => TerminalError {
                report: TerminalErrorType::InvalidConfigurationTheme(message),
                level: TerminalErrorLevel::Warning,
            },
            ConfigError::PathNotFound => TerminalError {
                report: TerminalErrorType::ConfigurationNotFound,
                level: TerminalErrorLevel::Warning,
            },
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum TerminalErrorType {
    InitializationError(String),

    // configuration file was not found
    ConfigurationNotFound,
    // configuration file has an invalid format
    InvalidConfigurationFormat(String),
    // configuration invalid theme
    InvalidConfigurationTheme(String),

    // background image referenced in config could not be loaded
    BackgroundImageLoadFailure(String),

    // reports that are ignored by TerminalErrorType
    IgnoredReport,
}

impl fmt::Display for TerminalErrorType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            TerminalErrorType::ConfigurationNotFound => {
                write!(f, "Configuration file was not found")
            }
            TerminalErrorType::InitializationError(message) => {
                write!(f, "Error initializing NiumaTerm terminal:\n{message}")
            }
            TerminalErrorType::IgnoredReport => write!(f, ""),
            TerminalErrorType::InvalidConfigurationFormat(message) => {
                write!(
                    f,
                    "Found an issue loading the configuration file:\n\n{message}\n\nNiumaTerm will proceed with the default configuration"
                )
            }
            TerminalErrorType::InvalidConfigurationTheme(message) => {
                write!(f, "Found an issue in the configured theme:\n\n{message}")
            }
            TerminalErrorType::BackgroundImageLoadFailure(message) => {
                write!(
                    f,
                    "Could not load the configured background image:\n\n{message}\n\nCheck `window.background-image.path` in your config."
                )
            }
        }
    }
}
