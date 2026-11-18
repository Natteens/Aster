//! Backend result and error types shared by the scheduler and its providers.

use std::{error::Error, fmt};

use aster_diagnostics::{DiagnosticCode, codes};
use aster_mir as mir;

#[derive(Clone, Debug, PartialEq)]
pub enum ExecutionValue {
    SByte(i8),
    Byte(u8),
    Short(i16),
    UShort(u16),
    Int(i32),
    UInt(u32),
    Long(i64),
    ULong(u64),
    Float(f32),
    Double(f64),
    Bool(bool),
    Char(char),
    String(String),
    /// A value that crosses a task boundary by deep copy (strings, arrays,
    /// lists, dictionaries, structs and enums of those): serialized bytes with
    /// no pointer into the producing context.
    Transfer(aster_runtime::TransferWire),
    Void,
}

impl ExecutionValue {
    #[must_use]
    pub fn float(value: f32) -> Self {
        Self::Float(value)
    }

    #[must_use]
    pub fn double(value: f64) -> Self {
        Self::Double(value)
    }
}

impl fmt::Display for ExecutionValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SByte(value) => write!(formatter, "{value}"),
            Self::Byte(value) => write!(formatter, "{value}"),
            Self::Short(value) => write!(formatter, "{value}"),
            Self::UShort(value) => write!(formatter, "{value}"),
            Self::Int(value) => write!(formatter, "{value}"),
            Self::UInt(value) => write!(formatter, "{value}"),
            Self::Long(value) => write!(formatter, "{value}"),
            Self::ULong(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{value}"),
            Self::Double(value) => write!(formatter, "{value}"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Char(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
            Self::Transfer(_) => formatter.write_str("<transferred value>"),
            Self::Void => formatter.write_str("function completed successfully (void)"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendError {
    code: DiagnosticCode,
    message: String,
    cancellation: bool,
}

impl BackendError {
    #[must_use]
    pub fn new(code: DiagnosticCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            cancellation: false,
        }
    }

    #[must_use]
    pub fn cancellation() -> Self {
        Self {
            code: codes::EXEC_TASK_CANCELLED,
            message: "task was cancelled".to_owned(),
            cancellation: true,
        }
    }

    #[must_use]
    pub const fn code(&self) -> DiagnosticCode {
        self.code
    }

    #[must_use]
    pub fn is_cancellation(&self) -> bool {
        self.cancellation
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for BackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for BackendError {}

impl From<mir::validate::ValidationError> for BackendError {
    fn from(error: mir::validate::ValidationError) -> Self {
        let code = error.code();
        match error {
            mir::validate::ValidationError::Invalid { message, .. }
            | mir::validate::ValidationError::Rejected { message, .. } => Self::new(code, message),
            mir::validate::ValidationError::Unsupported {
                function, feature, ..
            } => Self::new(
                code,
                format!("Cranelift JIT does not yet support {feature} in function `{function}`"),
            ),
        }
    }
}
