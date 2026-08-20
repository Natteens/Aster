//! Standard-library logging surface: `Log`, `Log.Warning`, and `Log.Error`.
//!
//! Format: `[log] message` on stdout for normal messages, `[warning] message`
//! and `[error] message` on stderr. Messages have no timestamps. `Log.Error`
//! never terminates the program; logging is not error handling.

use crate::ExecutionContext;
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
/// Log output uses the execution context's terminal backend, so tests can
/// capture it alongside ordinary terminal output. Malformed input produces a
/// controlled diagnostic line instead of a panic, because panicking across
/// the `extern "C"` boundary would abort the process.
// Called only from generated code that upholds the ABI contract; marking the
// symbol `unsafe` would not change the JIT call site.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_log(
    context: *mut ExecutionContext,
    level: i32,
    message: *const AsterStrHeader,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let Some(level) = LogLevel::from_abi(level) else {
        let _ = context
            .console_backend()
            .write_error(format!("[error] aster-runtime: invalid log level {level}\n").as_bytes());
        return;
    };
    // SAFETY: the pointer originates from JIT string data that stays alive
    // for the duration of this call; `view` rejects null and invalid UTF-8.
    #[allow(unsafe_code)]
    let text = unsafe { view(message) };
    let Some(text) = text else {
        let _ = context
            .console_backend()
            .write_error(b"[error] aster-runtime: log message is not a valid ABI string\n");
        return;
    };
    let line = format!("[{}] {text}\n", level.label());
    let backend = context.console_backend();
    let _ = match level {
        LogLevel::Log => backend
            .write(line.as_bytes())
            .and_then(|()| backend.flush()),
        LogLevel::Warning | LogLevel::Error => backend
            .write_error(line.as_bytes())
            .and_then(|()| backend.flush_error()),
    };
}

#[cfg(test)]
mod tests {
    use super::{LogLevel, aster_rt_log};
    use crate::string::{AsterStrHeader, encode_str};
    use crate::{ExecutionContext, MemoryConsoleBackend};

    fn aligned(value: &str) -> Vec<u64> {
        let bytes = encode_str(value);
        let mut buffer = vec![0_u64; bytes.len().div_ceil(8)];
        // SAFETY: u64 slices are validly viewable as bytes.
        #[allow(unsafe_code)]
        let target = unsafe {
            std::slice::from_raw_parts_mut(buffer.as_mut_ptr().cast::<u8>(), bytes.len())
        };
        target.copy_from_slice(&bytes);
        buffer
    }

    fn pointer(buffer: &[u64]) -> *const AsterStrHeader {
        buffer.as_ptr().cast::<AsterStrHeader>()
    }

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

    #[test]
    fn logs_use_the_execution_context_console() {
        let mut context = ExecutionContext::new();
        let console = MemoryConsoleBackend::default();
        context.set_console_backend(Box::new(console.clone()));
        let message = aligned("captured");

        aster_rt_log(&raw mut context, LogLevel::Log as i32, pointer(&message));
        aster_rt_log(&raw mut context, LogLevel::Error as i32, pointer(&message));

        assert_eq!(console.output(), b"[log] captured\n[error] captured\n");
    }
}
