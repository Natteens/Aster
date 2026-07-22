//! Terminal I/O backing `aster.io.Write`/`WriteLine`/`ReadLine`.
//!
//! [`ConsoleBackend`] is the injectable seam: the real terminal
//! ([`StdConsoleBackend`], the default) and an in-memory backend
//! ([`MemoryConsoleBackend`]) implement the same trait, so tests never touch
//! the developer's or CI's real stdin/stdout. Each [`ExecutionContext`] owns
//! its backend independently -- there is no global, singleton, or registry,
//! so output from one context never appears in another and input from one is
//! never consumed by another.
//!
//! The backend does not represent a resource owned by the Aster program: the
//! console handles it wraps belong to the host process, so there is no
//! `Close`, no `using`/defer, and no ownership transfer across this ABI.

use std::io::{self, BufRead, Write as _};

use crate::ExecutionContext;
use crate::string::{AsterStrHeader, view, write_option_result};

/// One line read from the console, or `None` on end-of-input. The bytes
/// include the terminator (if any); callers strip it.
pub type ReadLineResult = io::Result<Option<Vec<u8>>>;

/// Injectable terminal backend for `aster.io`. Implementations own whatever
/// host resource they read from/write to; this trait never assumes a shared
/// or global stream.
pub trait ConsoleBackend: Send {
    /// Write raw bytes verbatim (no newline, no encoding conversion).
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying host write fails.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Make prior writes visible to whatever this backend represents.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying host flush fails.
    fn flush(&mut self) -> io::Result<()>;

    /// Read one line, including its terminator. `Ok(None)` means EOF with no
    /// more bytes available.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying host read fails.
    fn read_line(&mut self) -> ReadLineResult;
}

/// Default backend: the process's real stdin/stdout.
#[derive(Default)]
pub struct StdConsoleBackend {
    _private: (),
}

impl ConsoleBackend for StdConsoleBackend {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        io::stdout().lock().write_all(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().lock().flush()
    }

    fn read_line(&mut self) -> ReadLineResult {
        let mut buffer = Vec::new();
        let read = io::stdin().lock().read_until(b'\n', &mut buffer)?;
        if read == 0 {
            Ok(None)
        } else {
            Ok(Some(buffer))
        }
    }
}

/// In-memory backend for tests: reads from a pre-loaded byte buffer, writes
/// into an owned `Vec<u8>` that the test can inspect afterward.
#[derive(Clone, Default)]
pub struct MemoryConsoleBackend {
    input: std::sync::Arc<std::sync::Mutex<io::Cursor<Vec<u8>>>>,
    output: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl MemoryConsoleBackend {
    /// `input` is consumed line-by-line by `ReadLine()`. Output written via
    /// `Write`/`WriteLine` accumulates and can be read back through
    /// [`Self::output`] -- shared through a clone taken *before* the backend
    /// is moved into [`crate::ExecutionContext::set_console_backend`], since
    /// that call takes ownership of the `Box<dyn ConsoleBackend>`.
    #[must_use]
    pub fn new(input: impl Into<Vec<u8>>) -> Self {
        Self {
            input: std::sync::Arc::new(std::sync::Mutex::new(io::Cursor::new(input.into()))),
            output: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Bytes written so far. Reflects writes made through any clone sharing
    /// the same underlying buffers.
    #[must_use]
    pub fn output(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl ConsoleBackend for MemoryConsoleBackend {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn read_line(&mut self) -> ReadLineResult {
        let mut guard = self
            .input
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut buffer = Vec::new();
        let read = guard.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            Ok(None)
        } else {
            Ok(Some(buffer))
        }
    }
}

/// Test backend that always fails, simulating a real host I/O error (as
/// opposed to a normal EOF `None`).
#[derive(Default)]
pub struct FailingConsoleBackend {
    _private: (),
}

impl ConsoleBackend for FailingConsoleBackend {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<()> {
        Err(io::Error::other("simulated console write failure"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("simulated console flush failure"))
    }

    fn read_line(&mut self) -> ReadLineResult {
        Err(io::Error::other("simulated console read failure"))
    }
}

fn write_and_flush(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
    newline: bool,
    operation: &str,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext and a
    // string reference owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (context, text) = unsafe { (&mut *context, view(value)) };
    let Some(text) = text else {
        context.fail(format!(
            "aster.io.{operation} received an invalid UTF-8 string reference"
        ));
        return;
    };
    let backend = context.console_backend();
    if let Err(error) = backend.write(text.as_bytes()) {
        context.fail(format!("aster.io.{operation} failed: {error}"));
        return;
    }
    if newline && let Err(error) = backend.write(b"\n") {
        context.fail(format!("aster.io.{operation} failed: {error}"));
        return;
    }
    if let Err(error) = backend.flush() {
        context.fail(format!("aster.io.{operation} failed to flush: {error}"));
    }
}

/// Emit `value`'s UTF-8 bytes verbatim, then flush. Exported to generated
/// code as `aster_rt_io_write`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_io_write(context: *mut ExecutionContext, value: *const AsterStrHeader) {
    write_and_flush(context, value, false, "Write");
}

/// Like [`aster_rt_io_write`], plus a trailing LF. Exported to generated code
/// as `aster_rt_io_write_line`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_io_write_line(
    context: *mut ExecutionContext,
    value: *const AsterStrHeader,
) {
    write_and_flush(context, value, true, "WriteLine");
}

/// Strip exactly one logical line terminator: a trailing `\n`, and then a
/// trailing `\r` if one remains. Never trims anything else.
fn strip_line_terminator(bytes: &[u8]) -> &[u8] {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    bytes.strip_suffix(b"\r").unwrap_or(bytes)
}

#[allow(clippy::too_many_arguments)]
fn read_line_into_option(
    context: *mut ExecutionContext,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
    temporary: bool,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if destination.is_null() {
        context.fail("aster.io.ReadLine received a null destination");
        return;
    }
    let (Ok(total_size), Ok(payload_offset)) =
        (usize::try_from(total_size), usize::try_from(payload_offset))
    else {
        context.fail("aster.io.ReadLine received a negative layout size");
        return;
    };
    let Some(payload_end) = payload_offset.checked_add(size_of::<*const AsterStrHeader>()) else {
        context.fail("aster.io.ReadLine payload offset overflows");
        return;
    };
    if payload_end > total_size {
        context.fail("aster.io.ReadLine destination is too small for its payload");
        return;
    }
    let line = context.console_backend().read_line();
    match line {
        Ok(None) => {
            // SAFETY: `destination` is `total_size` bytes, `total_size` and
            // `payload_offset` were bounds-checked above.
            #[allow(unsafe_code)]
            unsafe {
                write_option_result::<*const AsterStrHeader>(
                    destination,
                    total_size,
                    payload_offset,
                    None,
                    some_tag,
                    none_tag,
                );
            }
        }
        Ok(Some(bytes)) => {
            let trimmed = strip_line_terminator(&bytes);
            let Ok(text) = std::str::from_utf8(trimmed) else {
                context.fail("aster.io.ReadLine received invalid UTF-8 input");
                return;
            };
            let pointer = if temporary {
                context.allocate_temporary_string_parts(&[text])
            } else {
                context.allocate_string_parts(&[text])
            };
            // SAFETY: as above.
            #[allow(unsafe_code)]
            unsafe {
                write_option_result(
                    destination,
                    total_size,
                    payload_offset,
                    Some(pointer),
                    some_tag,
                    none_tag,
                );
            }
        }
        Err(error) => context.fail(format!("aster.io.ReadLine failed: {error}")),
    }
}

/// Read one line into a persistent `Option<string>`. `None` on EOF. Exported
/// to generated code as `aster_rt_io_read_line`.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_io_read_line(
    context: *mut ExecutionContext,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    read_line_into_option(
        context,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        false,
    );
}

/// Temporary-arena counterpart of [`aster_rt_io_read_line`].
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_io_read_line_temporary(
    context: *mut ExecutionContext,
    destination: *mut u8,
    total_size: i32,
    some_tag: i32,
    none_tag: i32,
    payload_offset: i32,
) {
    read_line_into_option(
        context,
        destination,
        total_size,
        some_tag,
        none_tag,
        payload_offset,
        true,
    );
}

#[cfg(test)]
mod tests {
    use super::{
        ConsoleBackend, FailingConsoleBackend, MemoryConsoleBackend, aster_rt_io_read_line,
        aster_rt_io_read_line_temporary, aster_rt_io_write, aster_rt_io_write_line,
        strip_line_terminator,
    };
    use crate::ExecutionContext;
    use crate::context::aster_rt_temporary_scope_enter;
    use crate::string::{AsterStrHeader, encode_str, view};

    /// 8-byte-aligned backing store for test strings, mirroring
    /// `string::tests::aligned` (that helper is private to its own module).
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
        buffer.as_ptr().cast()
    }

    /// `total_size = 16, payload_offset = 8`: tag (`i32`) then an 8-byte
    /// pointer payload, matching every other `Option<T>` ABI test in this
    /// crate (`string::tests::option_destination`).
    fn option_destination() -> Vec<u64> {
        vec![0_u64; 2]
    }

    #[test]
    fn strips_lf_and_crlf_but_nothing_else() {
        assert_eq!(strip_line_terminator(b"hello\n"), b"hello");
        assert_eq!(strip_line_terminator(b"hello\r\n"), b"hello");
        assert_eq!(strip_line_terminator(b"hello"), b"hello");
        assert_eq!(strip_line_terminator(b"\n"), b"");
        assert_eq!(strip_line_terminator(b"  hello  \n"), b"  hello  ");
        assert_eq!(strip_line_terminator(b"hello\r\r\n"), b"hello\r");
    }

    #[test]
    fn memory_backend_reads_lines_and_reports_eof() {
        let mut backend = MemoryConsoleBackend::new("a\nb\r\nc".as_bytes());
        assert_eq!(backend.read_line().unwrap(), Some(b"a\n".to_vec()));
        assert_eq!(backend.read_line().unwrap(), Some(b"b\r\n".to_vec()));
        assert_eq!(backend.read_line().unwrap(), Some(b"c".to_vec()));
        assert_eq!(backend.read_line().unwrap(), None);
        assert_eq!(backend.read_line().unwrap(), None);
    }

    #[test]
    fn write_emits_exact_utf8_bytes_including_embedded_nul() {
        for text in ["abc", "héllo ✓", "", "a\0b"] {
            let mut context = ExecutionContext::new();
            let backend = MemoryConsoleBackend::default();
            context.set_console_backend(Box::new(backend.clone()));
            let input = aligned(text);
            aster_rt_io_write(&raw mut context, pointer(&input));
            assert!(context.take_error().is_none());
            assert_eq!(backend.output(), text.as_bytes());
        }
    }

    #[test]
    fn write_line_emits_exactly_one_trailing_lf() {
        let mut context = ExecutionContext::new();
        let backend = MemoryConsoleBackend::default();
        context.set_console_backend(Box::new(backend.clone()));
        let input = aligned("abc");
        aster_rt_io_write_line(&raw mut context, pointer(&input));
        assert!(context.take_error().is_none());
        assert_eq!(backend.output(), b"abc\n");
    }

    #[test]
    fn write_line_of_empty_string_emits_only_lf() {
        let mut context = ExecutionContext::new();
        let backend = MemoryConsoleBackend::default();
        context.set_console_backend(Box::new(backend.clone()));
        let input = aligned("");
        aster_rt_io_write_line(&raw mut context, pointer(&input));
        assert!(context.take_error().is_none());
        assert_eq!(backend.output(), b"\n");
    }

    #[test]
    fn several_writes_accumulate_in_call_order() {
        let mut context = ExecutionContext::new();
        let backend = MemoryConsoleBackend::default();
        context.set_console_backend(Box::new(backend.clone()));
        let a = aligned("a");
        let b = aligned("b");
        let c = aligned("c");
        aster_rt_io_write(&raw mut context, pointer(&a));
        aster_rt_io_write_line(&raw mut context, pointer(&b));
        aster_rt_io_write(&raw mut context, pointer(&c));
        assert!(context.take_error().is_none());
        assert_eq!(backend.output(), b"ab\nc");
    }

    #[test]
    fn write_reports_a_controlled_error_on_backend_failure() {
        let mut context = ExecutionContext::new();
        context.set_console_backend(Box::new(FailingConsoleBackend::default()));
        let input = aligned("abc");
        aster_rt_io_write(&raw mut context, pointer(&input));
        assert!(context.take_error().is_some());
    }

    #[test]
    fn read_line_strips_lf_and_crlf_exactly() {
        for (input, expected) in [
            ("hello\n", "hello"),
            ("hello\r\n", "hello"),
            ("\n", ""),
            ("hello", "hello"),
        ] {
            let mut context = ExecutionContext::new();
            context.set_console_backend(Box::new(MemoryConsoleBackend::new(input.as_bytes())));
            let mut destination = option_destination();
            let destination_ptr = destination.as_mut_ptr().cast::<u8>();
            aster_rt_io_read_line(&raw mut context, destination_ptr, 16, 1, 0, 8);
            assert!(context.take_error().is_none());
            // SAFETY: just written above.
            #[allow(unsafe_code)]
            unsafe {
                assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
                let string_pointer = std::ptr::read_unaligned(
                    destination_ptr.add(8).cast::<*const AsterStrHeader>(),
                );
                assert_eq!(view(string_pointer), Some(expected));
            }
        }
    }

    #[test]
    fn read_line_returns_none_on_eof_without_erroring() {
        let mut context = ExecutionContext::new();
        context.set_console_backend(Box::new(MemoryConsoleBackend::new(Vec::new())));
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_io_read_line(&raw mut context, destination_ptr, 16, 1, 0, 8);
        assert!(context.take_error().is_none());
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 0);
        }
        // Calling again after EOF stays `None`, never an error.
        aster_rt_io_read_line(&raw mut context, destination_ptr, 16, 1, 0, 8);
        assert!(context.take_error().is_none());
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 0);
        }
    }

    #[test]
    fn read_line_preserves_an_embedded_nul_within_the_line() {
        let mut context = ExecutionContext::new();
        context.set_console_backend(Box::new(MemoryConsoleBackend::new(b"a\0b\n".to_vec())));
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_io_read_line(&raw mut context, destination_ptr, 16, 1, 0, 8);
        assert!(context.take_error().is_none());
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
            let string_pointer =
                std::ptr::read_unaligned(destination_ptr.add(8).cast::<*const AsterStrHeader>());
            assert_eq!(view(string_pointer), Some("a\0b"));
        }
    }

    #[test]
    fn read_line_rejects_invalid_utf8_in_a_controlled_buffer() {
        let mut context = ExecutionContext::new();
        context.set_console_backend(Box::new(MemoryConsoleBackend::new(vec![0xFF, b'\n'])));
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_io_read_line(&raw mut context, destination_ptr, 16, 1, 0, 8);
        assert!(context.take_error().is_some());
    }

    #[test]
    fn read_line_reports_a_controlled_error_on_backend_failure() {
        let mut context = ExecutionContext::new();
        context.set_console_backend(Box::new(FailingConsoleBackend::default()));
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_io_read_line(&raw mut context, destination_ptr, 16, 1, 0, 8);
        assert!(context.take_error().is_some());
    }

    #[test]
    fn read_line_temporary_writes_the_same_shape_as_persistent() {
        let mut context = ExecutionContext::new();
        context.set_console_backend(Box::new(MemoryConsoleBackend::new("hi\n".as_bytes())));
        aster_rt_temporary_scope_enter(&raw mut context);
        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_io_read_line_temporary(&raw mut context, destination_ptr, 16, 1, 0, 8);
        assert!(context.take_error().is_none());
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            assert_eq!(std::ptr::read_unaligned(destination_ptr.cast::<i32>()), 1);
            let string_pointer =
                std::ptr::read_unaligned(destination_ptr.add(8).cast::<*const AsterStrHeader>());
            assert_eq!(view(string_pointer), Some("hi"));
        }
    }

    #[test]
    fn independent_contexts_never_share_output_or_input() {
        let mut first = ExecutionContext::new();
        let first_backend = MemoryConsoleBackend::new("first-input\n".as_bytes());
        first.set_console_backend(Box::new(first_backend.clone()));
        let mut second = ExecutionContext::new();
        let second_backend = MemoryConsoleBackend::new("second-input\n".as_bytes());
        second.set_console_backend(Box::new(second_backend.clone()));

        let hello = aligned("hello-from-first");
        aster_rt_io_write(&raw mut first, pointer(&hello));
        assert_eq!(first_backend.output(), b"hello-from-first");
        assert!(second_backend.output().is_empty());

        let mut destination = option_destination();
        let destination_ptr = destination.as_mut_ptr().cast::<u8>();
        aster_rt_io_read_line(&raw mut second, destination_ptr, 16, 1, 0, 8);
        // SAFETY: just written above.
        #[allow(unsafe_code)]
        unsafe {
            let string_pointer =
                std::ptr::read_unaligned(destination_ptr.add(8).cast::<*const AsterStrHeader>());
            assert_eq!(view(string_pointer), Some("second-input"));
        }
    }

    #[test]
    fn memory_backend_captures_written_bytes() {
        let mut backend = MemoryConsoleBackend::default();
        backend.write(b"abc").unwrap();
        backend.write(b"\n").unwrap();
        assert_eq!(backend.output(), b"abc\n");
    }
}
