//! Standard-library logging surface: `Log`, `Log.Warning`, and `Log.Error`.
//!
//! Format: `[log] message` on stdout for normal messages, `[warning] message`
//! and `[error] message` on stderr. Messages have no timestamps. `Log.Error`
//! never terminates the program; logging is not error handling.

use crate::string::{AsterStrHeader, view};

/// Severity accepted across the ABI as an `i32`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum LogLevel {
    Log = 0,
    Warning = 1,
    Error = 2,
}

impl LogLevel {
    #[must_use]
    pub fn from_abi(value: i32) -> Option<Self> {
        match value {
            0 => Some(Self::Log),
            1 => Some(Self::Warning),
            2 => Some(Self::Error),
            _ => None,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Write one log message. Exported to generated code as `aster_rt_log`.
///
/// Malformed input (unknown level, null pointer, invalid UTF-8) produces a
/// controlled `[error]` line instead of a panic, because panicking across the
/// `extern "C"` boundary would abort the process.
// Called only from generated code that upholds the ABI contract; marking the
// symbol `unsafe` would not change the JIT call site.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_log(level: i32, message: *const AsterStrHeader) {
    let Some(level) = LogLevel::from_abi(level) else {
        eprintln!("[error] aster-runtime: invalid log level {level}");
        return;
    };
    // SAFETY: the pointer originates from JIT string data that stays alive
    // for the duration of this call; `view` rejects null and invalid UTF-8.
    #[allow(unsafe_code)]
    let text = unsafe { view(message) };
    let Some(text) = text else {
        eprintln!("[error] aster-runtime: log message is not a valid ABI string");
        return;
    };
    match level {
        LogLevel::Log => println!("[log] {text}"),
        LogLevel::Warning => eprintln!("[warning] {text}"),
        LogLevel::Error => eprintln!("[error] {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::LogLevel;

    #[test]
    fn maps_abi_levels() {
        assert_eq!(LogLevel::from_abi(0), Some(LogLevel::Log));
        assert_eq!(LogLevel::from_abi(1), Some(LogLevel::Warning));
        assert_eq!(LogLevel::from_abi(2), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_abi(3), None);
        assert_eq!(LogLevel::from_abi(-1), None);
    }

    #[test]
    fn labels_are_stable() {
        assert_eq!(LogLevel::Log.label(), "log");
        assert_eq!(LogLevel::Warning.label(), "warning");
        assert_eq!(LogLevel::Error.label(), "error");
    }
}
