//! Portable, host-side classification of filesystem errors, ahead of any
//! actual filesystem I/O (a later milestone). Mirrors the shape of
//! `aster.io.IOErrorKind`/`IOError` (`stdlib/aster/io/io.aster`) without
//! referencing them: this crate never depends on ASTER source or types, and
//! no ASTER memory is constructed here. A future filesystem operation uses
//! [`classify_io_error`] to decide which `IOErrorKind` case and `OsCode` to
//! write into a real `Result<T, IOError>` value; this milestone only
//! prepares that classification, since there is no filesystem call yet to
//! classify.
//!
//! Never stores a `std::io::Error` (no message, no allocation, no reference
//! to the original error) and never touches [`crate::ExecutionContext`]:
//! filesystem errors are ordinary values (a future `Result<T, IOError>`),
//! not the internal-corruption channel `ExecutionContext::fail` guards.

use std::io;

/// Portable category of a filesystem error. Mirrors `aster.io.IOErrorKind`'s
/// cases one-to-one; kept independent so this crate never depends on ASTER
/// source, but any renumbering here must be mirrored there by hand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortableIoErrorKind {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    InvalidPath,
    InvalidUtf8,
    NotFile,
    NotDirectory,
    LimitExceeded,
    Other,
}

/// Portable filesystem error: a stable category plus the native OS code when
/// one exists (`0` otherwise). Carries no message, path, or handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortableIoError {
    pub kind: PortableIoErrorKind,
    pub os_code: i32,
}

/// Classify a real `std::io::Error` into the portable category a future
/// filesystem operation will report. Only the `ErrorKind`s Rust's stdlib
/// reliably distinguishes today are mapped explicitly; every other kind is
/// `Other`. `InvalidPath`/`InvalidUtf8`/`NotFile`/`NotDirectory`/
/// `LimitExceeded` are produced deliberately by future operations that know
/// their own context, never inferred from an arbitrary `std::io::Error` or
/// its message text.
#[must_use]
pub fn classify_io_error(error: &io::Error) -> PortableIoError {
    let kind = match error.kind() {
        io::ErrorKind::NotFound => PortableIoErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => PortableIoErrorKind::PermissionDenied,
        io::ErrorKind::AlreadyExists => PortableIoErrorKind::AlreadyExists,
        _ => PortableIoErrorKind::Other,
    };
    // `raw_os_error()` is `Option<i32>` on every Rust-supported platform;
    // `0` is the documented "no native code" sentinel `aster.io.IOError`
    // reserves for exactly this case.
    let os_code = error.raw_os_error().unwrap_or(0);
    PortableIoError { kind, os_code }
}

#[cfg(test)]
mod tests {
    use super::{PortableIoErrorKind, classify_io_error};
    use std::io;

    #[test]
    fn classifies_not_found() {
        let error = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(
            classify_io_error(&error).kind,
            PortableIoErrorKind::NotFound
        );
    }

    #[test]
    fn classifies_permission_denied() {
        let error = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(
            classify_io_error(&error).kind,
            PortableIoErrorKind::PermissionDenied
        );
    }

    #[test]
    fn classifies_already_exists() {
        let error = io::Error::from(io::ErrorKind::AlreadyExists);
        assert_eq!(
            classify_io_error(&error).kind,
            PortableIoErrorKind::AlreadyExists
        );
    }

    #[test]
    fn classifies_unmapped_kinds_as_other() {
        for kind in [
            io::ErrorKind::Interrupted,
            io::ErrorKind::UnexpectedEof,
            io::ErrorKind::TimedOut,
            io::ErrorKind::WriteZero,
        ] {
            let error = io::Error::from(kind);
            assert_eq!(
                classify_io_error(&error).kind,
                PortableIoErrorKind::Other,
                "{kind:?} should classify as Other"
            );
        }
    }

    #[test]
    fn preserves_the_raw_os_error_code_when_present() {
        let error = io::Error::from_raw_os_error(2);
        assert_eq!(classify_io_error(&error).os_code, 2);
    }

    #[test]
    fn absent_raw_os_error_classifies_as_zero() {
        let error = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(error.raw_os_error(), None, "test assumes a synthetic error");
        assert_eq!(classify_io_error(&error).os_code, 0);
    }

    #[test]
    fn classification_allocates_nothing_across_thousands_of_calls() {
        // No arena/context is involved at all; this documents that fact by
        // exercising the function repeatedly with no `ExecutionContext` in
        // scope, matching the "no ASTER allocation" contract.
        for code in 0..5000 {
            let error = io::Error::from_raw_os_error(code % 50);
            let classified = classify_io_error(&error);
            assert_eq!(classified.os_code, code % 50);
        }
    }
}
