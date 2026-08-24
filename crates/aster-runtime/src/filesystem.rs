//! Filesystem I/O backing `aster.io.ReadAllText`/`WriteAllText`.
//!
//! [`FileSystemBackend`] is the injectable seam, mirroring [`crate::io::ConsoleBackend`]:
//! the real filesystem ([`StdFileSystemBackend`], the default) and an
//! in-memory backend ([`MemoryFileSystemBackend`]) implement the same trait,
//! so tests never touch the developer's or CI's real filesystem. Each
//! [`crate::ExecutionContext`] owns its backend independently -- there is no
//! global, singleton, or registry.
//!
//! The backend does not represent a resource owned by the Aster program: the
//! host opens, reads/writes, and closes the file entirely within one
//! operation. There is no `File`, `OpenFile`, `Close`, handle, or ownership
//! transfer across this ABI -- matching M2A's decision that ASTER 1.0 never
//! exposes an owned file handle.

use std::io::{self, Read as _, Write as _};
use std::mem::size_of;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use crate::ExecutionContext;
use crate::io_error::{PortableIoErrorKind, classify_io_error};
use crate::string::{AsterStrHeader, view};

/// The fixed per-operation size limit for `ReadAllText` and `WriteAllText`.
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Fixed, host-enforced limits for one `ListFiles` operation. They cap only
/// the returned direct regular-file paths; neither value is user-configurable.
pub const MAX_LIST_FILES: usize = 100_000;
pub const MAX_LIST_PATH_BYTES: usize = 67_108_864;

/// A filesystem operation's outcome, ahead of classification into
/// [`PortableIoErrorKind`]. `NotFile` is synthesized by the backend (a
/// directory, or another incompatible type, used where a regular file was
/// required) rather than inferred from a real [`io::Error`].
#[derive(Debug)]
pub enum FileSystemError {
    Io(io::Error),
    NotFile,
    NotDirectory,
    InvalidPath,
    LimitExceeded,
}

/// Injectable filesystem backend for `aster.io.ReadAllText`/`WriteAllText`.
/// Implementations own whatever host resource they read from/write to; this
/// trait never assumes a shared or global filesystem.
pub trait FileSystemBackend: Send {
    /// Read the file at `path`, capped at `probe_limit` bytes (never more).
    /// Returning exactly `probe_limit` bytes tells the caller the real
    /// content may exceed the limit, without this backend needing to know
    /// what that limit means -- the caller decides whether that is
    /// `LimitExceeded`. An implementation may return `LimitExceeded` early
    /// when ordinary-file metadata already proves the same threshold was
    /// reached. Returns [`FileSystemError::NotFile`] for a directory or other
    /// non-regular-file target.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying host read fails or the target is
    /// not a regular file.
    fn read_all(&mut self, path: &str, probe_limit: u64) -> Result<Vec<u8>, FileSystemError>;

    /// Create or truncate the file at `path` and write `content` in full,
    /// then flush. Never appends, never creates parent directories. Returns
    /// [`FileSystemError::NotFile`] if `path` already names a directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying host write or flush fails, or the
    /// destination is not a regular file.
    fn write_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError>;

    /// Append bytes to a regular file, creating it when absent.
    ///
    /// # Errors
    ///
    /// Returns a classified host error or `NotFile` for a wrong-kind target.
    fn append_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError> {
        let _ = (path, content);
        Err(FileSystemError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem backend does not implement append",
        )))
    }

    /// List direct, regular, non-symlink files below `directory`. The
    /// returned paths must already be full relative to `directory`, valid
    /// UTF-8, ordinally sorted, and entirely validated against both limits.
    /// A backend returns no partial list on an opening or iteration failure.
    ///
    /// # Errors
    ///
    /// Returns a classified host error, `NotDirectory`, `InvalidPath`, or
    /// `LimitExceeded`; never a partial result.
    ///
    /// The default preserves source compatibility for host test backends that
    /// intentionally only implement read/write; it is not reachable from the
    /// standard or in-memory backend used by ASTER executions.
    fn list_files(
        &mut self,
        _directory: &str,
        _max_entries: usize,
        _max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        Err(FileSystemError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem backend does not implement directory enumeration",
        )))
    }

    /// List direct non-symlink directory children in ordinal order.
    ///
    /// # Errors
    ///
    /// Returns a classified host error or `LimitExceeded`; never a partial list.
    fn list_directories(
        &mut self,
        _directory: &str,
        _max_entries: usize,
        _max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        Err(FileSystemError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem backend does not implement directory enumeration",
        )))
    }

    /// Determine whether a path is an existing regular file.
    ///
    /// # Errors
    ///
    /// Returns meaningful host errors other than not-found or wrong-kind.
    fn file_exists(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }
    /// Determine whether a path is an existing directory.
    ///
    /// # Errors
    ///
    /// Returns meaningful host errors other than not-found or wrong-kind.
    fn directory_exists(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }
    /// Create exactly one directory and report whether it was created.
    ///
    /// # Errors
    ///
    /// Returns a classified host error; missing parents are not created.
    fn create_directory(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }
    /// Delete one regular file and report whether it existed.
    ///
    /// # Errors
    ///
    /// Returns a classified host error for wrong-kind or failed deletion.
    fn delete_file(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }
    /// Delete one empty directory and report whether it existed.
    ///
    /// # Errors
    ///
    /// Returns a classified host error for non-empty, wrong-kind, or failed deletion.
    fn delete_directory(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(
            io::ErrorKind::Unsupported,
        )))
    }
}

/// Default backend: the process's real filesystem.
#[derive(Default)]
pub struct StdFileSystemBackend {
    _private: (),
}

fn read_bounded_regular_file<R: io::Read>(
    reader: R,
    known_regular_len: Option<u64>,
    probe_limit: u64,
) -> Result<Vec<u8>, FileSystemError> {
    if known_regular_len.is_some_and(|length| length >= probe_limit) {
        return Err(FileSystemError::LimitExceeded);
    }
    let mut buffer = Vec::new();
    reader
        .take(probe_limit)
        .read_to_end(&mut buffer)
        .map_err(FileSystemError::Io)?;
    Ok(buffer)
}

impl FileSystemBackend for StdFileSystemBackend {
    fn read_all(&mut self, path: &str, probe_limit: u64) -> Result<Vec<u8>, FileSystemError> {
        if let Ok(metadata) = std::fs::metadata(path) {
            if metadata.is_dir() {
                return Err(FileSystemError::NotFile);
            }
        }
        let file = std::fs::File::open(path).map_err(FileSystemError::Io)?;
        let opened_metadata = file.metadata().ok();
        if opened_metadata
            .as_ref()
            .is_some_and(std::fs::Metadata::is_dir)
        {
            return Err(FileSystemError::NotFile);
        }
        let known_regular_len = opened_metadata
            .filter(std::fs::Metadata::is_file)
            .map(|metadata| metadata.len());
        read_bounded_regular_file(file, known_regular_len, probe_limit)
    }

    fn write_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError> {
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.is_dir()
        {
            return Err(FileSystemError::NotFile);
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(FileSystemError::Io)?;
        file.write_all(content).map_err(FileSystemError::Io)?;
        file.flush().map_err(FileSystemError::Io)?;
        Ok(())
    }

    fn append_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError> {
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.is_dir()
        {
            return Err(FileSystemError::NotFile);
        }
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)
            .map_err(FileSystemError::Io)?;
        file.write_all(content).map_err(FileSystemError::Io)?;
        file.flush().map_err(FileSystemError::Io)
    }

    fn list_files(
        &mut self,
        directory: &str,
        max_entries: usize,
        max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        if !std::fs::metadata(directory)
            .map_err(FileSystemError::Io)?
            .is_dir()
        {
            return Err(FileSystemError::NotDirectory);
        }

        let mut paths = Vec::new();
        let mut total_bytes = 0_usize;
        for entry in std::fs::read_dir(directory).map_err(FileSystemError::Io)? {
            let entry = entry.map_err(FileSystemError::Io)?;
            let file_type = entry.file_type().map_err(FileSystemError::Io)?;
            if file_type.is_symlink() || !file_type.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(path) = path.to_str() else {
                return Err(FileSystemError::InvalidPath);
            };
            collect_list_path(
                &mut paths,
                &mut total_bytes,
                path,
                max_entries,
                max_total_bytes,
            )?;
        }
        paths.sort();
        Ok(paths)
    }

    fn list_directories(
        &mut self,
        directory: &str,
        max_entries: usize,
        max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        if !std::fs::metadata(directory)
            .map_err(FileSystemError::Io)?
            .is_dir()
        {
            return Err(FileSystemError::NotDirectory);
        }
        let mut paths = Vec::new();
        let mut total_bytes = 0;
        for entry in std::fs::read_dir(directory).map_err(FileSystemError::Io)? {
            let entry = entry.map_err(FileSystemError::Io)?;
            let kind = entry.file_type().map_err(FileSystemError::Io)?;
            if kind.is_symlink() || !kind.is_dir() {
                continue;
            }
            let path = entry.path();
            let Some(path) = path.to_str() else {
                return Err(FileSystemError::InvalidPath);
            };
            collect_list_path(
                &mut paths,
                &mut total_bytes,
                path,
                max_entries,
                max_total_bytes,
            )?;
        }
        paths.sort();
        Ok(paths)
    }

    fn file_exists(&mut self, path: &str) -> Result<bool, FileSystemError> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_file()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(FileSystemError::Io(error)),
        }
    }

    fn directory_exists(&mut self, path: &str) -> Result<bool, FileSystemError> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(FileSystemError::Io(error)),
        }
    }

    fn create_directory(&mut self, path: &str) -> Result<bool, FileSystemError> {
        match std::fs::create_dir(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::metadata(path)
                    .map_err(FileSystemError::Io)?
                    .is_dir()
                {
                    Ok(false)
                } else {
                    Err(FileSystemError::NotDirectory)
                }
            }
            Err(error) => Err(FileSystemError::Io(error)),
        }
    }

    fn delete_file(&mut self, path: &str) -> Result<bool, FileSystemError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(_error) if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) => {
                Err(FileSystemError::NotFile)
            }
            Err(error) => Err(FileSystemError::Io(error)),
        }
    }

    fn delete_directory(&mut self, path: &str) -> Result<bool, FileSystemError> {
        match std::fs::remove_dir(path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(_error) if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) => {
                Err(FileSystemError::NotDirectory)
            }
            Err(error) => Err(FileSystemError::Io(error)),
        }
    }
}

enum MemoryEntry {
    File(Vec<u8>),
    Directory,
    Symlink,
    Other,
}

/// In-memory backend for tests: files live in a shared map, so tests never
/// touch the real filesystem and can construct a "file" whose stored content
/// exceeds any limit under test (simulating a file that grows past `MAX`).
#[derive(Clone, Default)]
pub struct MemoryFileSystemBackend {
    entries: Arc<Mutex<std::collections::HashMap<String, MemoryEntry>>>,
}

impl MemoryFileSystemBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a file with `content`, replacing any prior entry at `path`.
    #[must_use]
    pub fn with_file(self, path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.into(), MemoryEntry::File(content.into()));
        self
    }

    /// Seed a directory entry, so operations against `path` observe
    /// `NotFile` exactly as they would against a real directory.
    #[must_use]
    pub fn with_directory(self, path: impl Into<String>) -> Self {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.into(), MemoryEntry::Directory);
        self
    }

    /// Seed a symlink-like entry. Directory enumeration intentionally ignores
    /// it without following its target.
    #[must_use]
    pub fn with_symlink(self, path: impl Into<String>) -> Self {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.into(), MemoryEntry::Symlink);
        self
    }

    /// Seed an entry that is neither a file nor a directory (for example a
    /// device or pipe in a host filesystem). Enumeration ignores it.
    #[must_use]
    pub fn with_other(self, path: impl Into<String>) -> Self {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(path.into(), MemoryEntry::Other);
        self
    }

    /// The bytes currently stored at `path`, if any (for assertions after a
    /// `WriteAllText` call through a clone sharing the same backing map).
    #[must_use]
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        match self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
        {
            Some(MemoryEntry::File(bytes)) => Some(bytes.clone()),
            _ => None,
        }
    }
}

impl FileSystemBackend for MemoryFileSystemBackend {
    fn read_all(&mut self, path: &str, probe_limit: u64) -> Result<Vec<u8>, FileSystemError> {
        let guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(path) {
            Some(MemoryEntry::File(bytes)) => {
                let limit = usize::try_from(probe_limit).unwrap_or(usize::MAX);
                Ok(bytes.iter().take(limit).copied().collect())
            }
            Some(MemoryEntry::Directory | MemoryEntry::Symlink | MemoryEntry::Other) => {
                Err(FileSystemError::NotFile)
            }
            None => Err(FileSystemError::Io(io::Error::from(
                io::ErrorKind::NotFound,
            ))),
        }
    }

    fn write_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if matches!(guard.get(path), Some(MemoryEntry::Directory)) {
            return Err(FileSystemError::NotFile);
        }
        guard.insert(path.to_owned(), MemoryEntry::File(content.to_vec()));
        Ok(())
    }

    fn append_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get_mut(path) {
            Some(MemoryEntry::File(bytes)) => bytes.extend_from_slice(content),
            Some(MemoryEntry::Directory | MemoryEntry::Symlink | MemoryEntry::Other) => {
                return Err(FileSystemError::NotFile);
            }
            None => {
                guard.insert(path.to_owned(), MemoryEntry::File(content.to_vec()));
            }
        }
        Ok(())
    }

    fn list_files(
        &mut self,
        directory: &str,
        max_entries: usize,
        max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        let guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(directory) {
            Some(MemoryEntry::Directory) => {}
            Some(_) => return Err(FileSystemError::NotDirectory),
            None => {
                return Err(FileSystemError::Io(io::Error::from(
                    io::ErrorKind::NotFound,
                )));
            }
        }

        let directory = Path::new(directory);
        let mut paths = Vec::new();
        let mut total_bytes = 0_usize;
        for (path, entry) in guard.iter() {
            if !matches!(entry, MemoryEntry::File(_)) || Path::new(path).parent() != Some(directory)
            {
                continue;
            }
            collect_list_path(
                &mut paths,
                &mut total_bytes,
                path,
                max_entries,
                max_total_bytes,
            )?;
        }
        paths.sort();
        Ok(paths)
    }

    fn list_directories(
        &mut self,
        directory: &str,
        max_entries: usize,
        max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        let guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(directory) {
            Some(MemoryEntry::Directory) => {}
            Some(_) => return Err(FileSystemError::NotDirectory),
            None => {
                return Err(FileSystemError::Io(io::Error::from(
                    io::ErrorKind::NotFound,
                )));
            }
        }
        let directory = Path::new(directory);
        let mut paths = Vec::new();
        let mut total_bytes = 0;
        for (path, entry) in guard.iter() {
            if !matches!(entry, MemoryEntry::Directory)
                || Path::new(path).parent() != Some(directory)
            {
                continue;
            }
            collect_list_path(
                &mut paths,
                &mut total_bytes,
                path,
                max_entries,
                max_total_bytes,
            )?;
        }
        paths.sort();
        Ok(paths)
    }

    fn file_exists(&mut self, path: &str) -> Result<bool, FileSystemError> {
        Ok(matches!(
            self.entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path),
            Some(MemoryEntry::File(_))
        ))
    }

    fn directory_exists(&mut self, path: &str) -> Result<bool, FileSystemError> {
        Ok(matches!(
            self.entries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(path),
            Some(MemoryEntry::Directory)
        ))
    }

    fn create_directory(&mut self, path: &str) -> Result<bool, FileSystemError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(path) {
            Some(MemoryEntry::Directory) => Ok(false),
            Some(_) => Err(FileSystemError::NotDirectory),
            None => {
                guard.insert(path.to_owned(), MemoryEntry::Directory);
                Ok(true)
            }
        }
    }

    fn delete_file(&mut self, path: &str) -> Result<bool, FileSystemError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(path) {
            None => Ok(false),
            Some(MemoryEntry::File(_)) => {
                guard.remove(path);
                Ok(true)
            }
            Some(_) => Err(FileSystemError::NotFile),
        }
    }

    fn delete_directory(&mut self, path: &str) -> Result<bool, FileSystemError> {
        let mut guard = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        match guard.get(path) {
            None => return Ok(false),
            Some(MemoryEntry::Directory) => {}
            Some(_) => return Err(FileSystemError::NotDirectory),
        }
        let directory = Path::new(path);
        if guard
            .keys()
            .any(|candidate| Path::new(candidate).parent() == Some(directory))
        {
            return Err(FileSystemError::Io(io::Error::new(
                io::ErrorKind::DirectoryNotEmpty,
                "directory is not empty",
            )));
        }
        guard.remove(path);
        Ok(true)
    }
}

/// Test backend that always fails with a chosen [`io::ErrorKind`], simulating
/// a real host I/O error (e.g. a permission failure or a flush failure)
/// independent of any real filesystem state.
pub struct FailingFileSystemBackend {
    kind: io::ErrorKind,
}

impl FailingFileSystemBackend {
    #[must_use]
    pub fn new(kind: io::ErrorKind) -> Self {
        Self { kind }
    }
}

impl FileSystemBackend for FailingFileSystemBackend {
    fn read_all(&mut self, _path: &str, _probe_limit: u64) -> Result<Vec<u8>, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }

    fn write_all(&mut self, _path: &str, _content: &[u8]) -> Result<(), FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }

    fn list_files(
        &mut self,
        _directory: &str,
        _max_entries: usize,
        _max_total_bytes: usize,
    ) -> Result<Vec<String>, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }

    fn file_exists(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }
    fn directory_exists(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }
    fn create_directory(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }
    fn delete_file(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }
    fn delete_directory(&mut self, _path: &str) -> Result<bool, FileSystemError> {
        Err(FileSystemError::Io(io::Error::from(self.kind)))
    }
}

/// Test backend simulating a partial failure: the content is actually stored
/// (as a real partial write might leave some bytes on disk), but the
/// operation still reports failure -- exactly the "flush failed after the
/// bytes were written" and "failure after truncation" contracts `WriteAllText`
/// documents as possible, without promising atomicity.
#[derive(Clone, Default)]
pub struct PartialWriteFailureFileSystemBackend {
    inner: MemoryFileSystemBackend,
}

impl PartialWriteFailureFileSystemBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.inner.read(path)
    }
}

impl FileSystemBackend for PartialWriteFailureFileSystemBackend {
    fn read_all(&mut self, path: &str, probe_limit: u64) -> Result<Vec<u8>, FileSystemError> {
        self.inner.read_all(path, probe_limit)
    }

    fn write_all(&mut self, path: &str, content: &[u8]) -> Result<(), FileSystemError> {
        // The store itself succeeds (as a real partial write might leave
        // bytes behind), but the reported outcome is still failure.
        let _ = self.inner.write_all(path, content);
        Err(FileSystemError::Io(io::Error::other(
            "simulated flush failure after content was written",
        )))
    }

    fn file_exists(&mut self, path: &str) -> Result<bool, FileSystemError> {
        self.inner.file_exists(path)
    }
    fn directory_exists(&mut self, path: &str) -> Result<bool, FileSystemError> {
        self.inner.directory_exists(path)
    }
    fn create_directory(&mut self, path: &str) -> Result<bool, FileSystemError> {
        self.inner.create_directory(path)
    }
    fn delete_file(&mut self, path: &str) -> Result<bool, FileSystemError> {
        self.inner.delete_file(path)
    }
    fn delete_directory(&mut self, path: &str) -> Result<bool, FileSystemError> {
        self.inner.delete_directory(path)
    }
}

/// Classify a [`FileSystemError`] into the portable category and OS code the
/// generated ABI writes into `IOError`. `NotFile` never has a native OS code
/// (`0`, the documented synthetic-category sentinel); a real [`io::Error`]
/// uses [`classify_io_error`], exactly like every other host-side error this
/// runtime reports.
fn classify(error: FileSystemError) -> (PortableIoErrorKind, i32) {
    match error {
        FileSystemError::NotFile => (PortableIoErrorKind::NotFile, 0),
        FileSystemError::NotDirectory => (PortableIoErrorKind::NotDirectory, 0),
        FileSystemError::InvalidPath => (PortableIoErrorKind::InvalidPath, 0),
        FileSystemError::LimitExceeded => (PortableIoErrorKind::LimitExceeded, 0),
        FileSystemError::Io(error) => {
            let classified = classify_io_error(&error);
            (classified.kind, classified.os_code)
        }
    }
}

fn collect_list_path(
    paths: &mut Vec<String>,
    total_bytes: &mut usize,
    path: &str,
    max_entries: usize,
    max_total_bytes: usize,
) -> Result<(), FileSystemError> {
    let next_count = paths
        .len()
        .checked_add(1)
        .ok_or(FileSystemError::LimitExceeded)?;
    if next_count > max_entries {
        return Err(FileSystemError::LimitExceeded);
    }
    let next_total = total_bytes
        .checked_add(path.len())
        .ok_or(FileSystemError::LimitExceeded)?;
    if next_total > max_total_bytes {
        return Err(FileSystemError::LimitExceeded);
    }
    paths.push(path.to_owned());
    *total_bytes = next_total;
    Ok(())
}

/// Read the 9 `IOErrorKind` tag values the compiler computed, in the fixed
/// order [`PortableIoErrorKind`]'s variants declare (mirrored by hand against
/// `aster.io.IOErrorKind`; see that enum's own doc comment). Returns `None`
/// for a null pointer, which callers treat as a controlled ABI-shape error,
/// never a dereference.
///
/// # Safety
///
/// `kind_tags`, when non-null, must point to 9 readable, correctly-aligned
/// `i32` values.
#[allow(unsafe_code)]
unsafe fn read_kind_tags(kind_tags: *const i32) -> Option<[i32; 9]> {
    if kind_tags.is_null() {
        return None;
    }
    // SAFETY: forwarded from the caller.
    let slice = unsafe { std::slice::from_raw_parts(kind_tags, 9) };
    slice.try_into().ok()
}

fn kind_tag(tags: &[i32; 9], kind: PortableIoErrorKind) -> i32 {
    tags[kind as usize]
}

/// Writes the complete `Result<T, IOError>` representation directly into
/// `destination`: zeroed first (so every case, including any enum padding,
/// is always fully initialized, never partial), then the tag, then either
/// the `Ok` payload or a fully constructed `IOError` (`Kind` and `OsCode`
/// written at their compiler-computed offsets, `Kind`'s own tag looked up
/// from `kind_tags` by name, never assumed). `total_size`/`ok_tag`/
/// `error_tag`/`ok_offset`/`error_offset`/`kind_offset`/`oscode_offset` are
/// compiler-computed facts about the concrete `Result<T, IOError>`
/// specialization (the same shared `Layouts` system every other aggregate
/// result uses), not assumptions this function makes on its own.
///
/// # Safety
///
/// `destination` must point to at least `total_size` writable bytes, aligned
/// for the concrete `Result<T, IOError>` layout, owned exclusively by the
/// caller for the duration of this call.
#[allow(unsafe_code)]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
unsafe fn write_result_io_error<T>(
    destination: *mut u8,
    total_size: usize,
    ok_tag: i32,
    error_tag: i32,
    ok_offset: usize,
    error_offset: usize,
    kind_offset: usize,
    oscode_offset: usize,
    payload: Result<T, (PortableIoErrorKind, i32)>,
    kind_tags: &[i32; 9],
) {
    // SAFETY: forwarded from the caller. Unaligned reads/writes are used
    // deliberately, matching `string::write_option_result`.
    unsafe {
        ptr_write_bytes(destination, total_size);
        match payload {
            Ok(value) => {
                write_unaligned_i32(destination, ok_tag);
                write_unaligned(destination.add(ok_offset), value);
            }
            Err((kind, os_code)) => {
                write_unaligned_i32(destination, error_tag);
                let error_base = destination.add(error_offset);
                write_unaligned_i32(error_base.add(kind_offset), kind_tag(kind_tags, kind));
                write_unaligned_i32(error_base.add(oscode_offset), os_code);
            }
        }
    }
}

#[allow(unsafe_code)]
unsafe fn ptr_write_bytes(destination: *mut u8, total_size: usize) {
    // SAFETY: forwarded from `write_result_io_error`'s caller.
    unsafe {
        std::ptr::write_bytes(destination, 0, total_size);
    }
}

#[allow(unsafe_code)]
unsafe fn write_unaligned_i32(destination: *mut u8, value: i32) {
    // SAFETY: forwarded from `write_result_io_error`'s caller.
    unsafe {
        std::ptr::write_unaligned(destination.cast::<i32>(), value);
    }
}

#[allow(unsafe_code)]
unsafe fn write_unaligned<T>(destination: *mut u8, value: T) {
    // SAFETY: forwarded from `write_result_io_error`'s caller.
    unsafe {
        std::ptr::write_unaligned(destination.cast::<T>(), value);
    }
}

fn result_io_error_layout_fits<T>(
    total_size: usize,
    ok_offset: usize,
    error_offset: usize,
    kind_offset: usize,
    oscode_offset: usize,
) -> bool {
    let ok_end = ok_offset.checked_add(size_of::<T>());
    let kind_end = error_offset
        .checked_add(kind_offset)
        .and_then(|offset| offset.checked_add(size_of::<i32>()));
    let oscode_end = error_offset
        .checked_add(oscode_offset)
        .and_then(|offset| offset.checked_add(size_of::<i32>()));
    total_size >= size_of::<i32>()
        && ok_end.is_some_and(|end| end <= total_size)
        && kind_end.is_some_and(|end| end <= total_size)
        && oscode_end.is_some_and(|end| end <= total_size)
}

#[allow(clippy::too_many_arguments)]
fn read_all_text(
    context: *mut ExecutionContext,
    path: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
    temporary: bool,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if destination.is_null() {
        context.fail("aster.io.ReadAllText received a null destination");
        return;
    }
    let (
        Ok(total_size),
        Ok(ok_payload_offset),
        Ok(error_payload_offset),
        Ok(kind_offset),
        Ok(oscode_offset),
    ) = (
        usize::try_from(total_size),
        usize::try_from(ok_payload_offset),
        usize::try_from(error_payload_offset),
        usize::try_from(kind_offset),
        usize::try_from(oscode_offset),
    )
    else {
        context.fail("aster.io.ReadAllText received a negative layout size");
        return;
    };
    if !result_io_error_layout_fits::<*const AsterStrHeader>(
        total_size,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
    ) {
        context.fail("aster.io.ReadAllText received a malformed Result layout");
        return;
    }
    // SAFETY: generated code passes a pointer to 9 compiler-computed tag
    // constants (or null on a malformed layout, handled below).
    #[allow(unsafe_code)]
    let Some(kind_tags) = (unsafe { read_kind_tags(kind_tags) }) else {
        context.fail("aster.io.ReadAllText received a malformed IOErrorKind tag layout");
        return;
    };
    // SAFETY: generated code passes its live hidden ExecutionContext and a
    // string reference owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let path_text = unsafe { view(path) };
    let Some(path_text) = path_text else {
        context.fail("aster.io.ReadAllText received an invalid UTF-8 string reference");
        return;
    };

    let outcome: Result<Vec<u8>, (PortableIoErrorKind, i32)> =
        if path_text.is_empty() || path_text.contains('\0') {
            Err((PortableIoErrorKind::InvalidPath, 0))
        } else {
            match context
                .filesystem_backend()
                .read_all(path_text, MAX_FILE_BYTES + 1)
            {
                Ok(bytes) if bytes.len() as u64 > MAX_FILE_BYTES => {
                    Err((PortableIoErrorKind::LimitExceeded, 0))
                }
                Ok(bytes) => Ok(bytes),
                Err(error) => Err(classify(error)),
            }
        };
    let payload: Result<*const AsterStrHeader, (PortableIoErrorKind, i32)> = match outcome {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Ok(if temporary {
                context.allocate_temporary_string_parts(&[&text])
            } else {
                context.allocate_string_parts(&[&text])
            }),
            Err(_) => Err((PortableIoErrorKind::InvalidUtf8, 0)),
        },
        Err(error) => Err(error),
    };
    // SAFETY: `destination` is caller-owned for the duration of this call,
    // sized by the same `Layouts` system that produced every offset here;
    // `total_size` was bounds-implied by the compiler, matching every other
    // aggregate-result ABI in this crate.
    #[allow(unsafe_code)]
    unsafe {
        write_result_io_error(
            destination,
            total_size,
            ok_tag,
            error_tag,
            ok_payload_offset,
            error_payload_offset,
            kind_offset,
            oscode_offset,
            payload,
            &kind_tags,
        );
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn list_paths(
    context: *mut ExecutionContext,
    directory: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
    temporary: bool,
    directories: bool,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if destination.is_null() {
        context.fail("aster.io directory listing received a null destination");
        return;
    }
    let (
        Ok(total_size),
        Ok(ok_payload_offset),
        Ok(error_payload_offset),
        Ok(kind_offset),
        Ok(oscode_offset),
    ) = (
        usize::try_from(total_size),
        usize::try_from(ok_payload_offset),
        usize::try_from(error_payload_offset),
        usize::try_from(kind_offset),
        usize::try_from(oscode_offset),
    )
    else {
        context.fail("aster.io directory listing received a negative layout size");
        return;
    };
    if !result_io_error_layout_fits::<*mut AsterStrHeader>(
        total_size,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
    ) {
        context.fail("aster.io directory listing received a malformed Result layout");
        return;
    }
    // SAFETY: generated code passes a pointer to 9 compiler-computed tag
    // constants (or null on a malformed layout, handled below).
    #[allow(unsafe_code)]
    let Some(kind_tags) = (unsafe { read_kind_tags(kind_tags) }) else {
        context.fail("aster.io directory listing received a malformed IOErrorKind tag layout");
        return;
    };
    // SAFETY: generated code passes its live context and a string reference
    // owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let directory = unsafe { view(directory) };
    let Some(directory) = directory else {
        context.fail("aster.io directory listing received an invalid UTF-8 string reference");
        return;
    };

    let mut paths = if directory.is_empty() || directory.contains('\0') {
        Err((PortableIoErrorKind::InvalidPath, 0))
    } else {
        if directories {
            context.filesystem_backend().list_directories(
                directory,
                MAX_LIST_FILES,
                MAX_LIST_PATH_BYTES,
            )
        } else {
            context
                .filesystem_backend()
                .list_files(directory, MAX_LIST_FILES, MAX_LIST_PATH_BYTES)
        }
        .map_err(classify)
    };

    let validation_error = if let Ok(paths) = &mut paths {
        paths.sort();
        if paths.len() > MAX_LIST_FILES {
            Some(PortableIoErrorKind::LimitExceeded)
        } else {
            let mut total = 0_usize;
            let mut error = None;
            for path in paths.iter() {
                if path.is_empty() || path.contains('\0') {
                    error = Some(PortableIoErrorKind::InvalidPath);
                    break;
                }
                let Some(next_total) = total.checked_add(path.len()) else {
                    error = Some(PortableIoErrorKind::LimitExceeded);
                    break;
                };
                if next_total > MAX_LIST_PATH_BYTES {
                    error = Some(PortableIoErrorKind::LimitExceeded);
                    break;
                }
                total = next_total;
            }
            error
        }
    } else {
        None
    };
    if let Some(kind) = validation_error {
        paths = Err((kind, 0));
    }

    let paths = match paths {
        Ok(paths) => paths,
        Err(error) => {
            // SAFETY: as in `read_all_text`; no ASTER allocation has been
            // published or attempted on this filesystem error path.
            #[allow(unsafe_code)]
            unsafe {
                write_result_io_error(
                    destination,
                    total_size,
                    ok_tag,
                    error_tag,
                    ok_payload_offset,
                    error_payload_offset,
                    kind_offset,
                    oscode_offset,
                    Err::<*mut AsterStrHeader, _>(error),
                    &kind_tags,
                );
            }
            return;
        }
    };
    let Ok(length) = i32::try_from(paths.len()) else {
        // The public limit keeps this unreachable, but preserve the no-partial
        // contract if a backend is adversarial.
        #[allow(unsafe_code)]
        unsafe {
            write_result_io_error(
                destination,
                total_size,
                ok_tag,
                error_tag,
                ok_payload_offset,
                error_payload_offset,
                kind_offset,
                oscode_offset,
                Err::<*mut AsterStrHeader, _>((PortableIoErrorKind::LimitExceeded, 0)),
                &kind_tags,
            );
        }
        return;
    };
    let element_size = u32::try_from(size_of::<*const AsterStrHeader>()).unwrap_or(0);
    if element_size == 0 {
        context.fail("aster.io directory listing cannot represent string array elements");
        return;
    }
    let array = if temporary {
        context.allocate_temporary_array(length, element_size)
    } else {
        context.allocate_array(length, element_size)
    };
    if array.is_null() {
        return;
    }
    for (index, path) in paths.iter().enumerate() {
        let string = if temporary {
            context.allocate_temporary_string_parts(&[path])
        } else {
            context.allocate_string_parts(&[path])
        };
        if string.is_null() {
            return;
        }
        let Ok(index) = i32::try_from(index) else {
            context.fail("aster.io directory listing entry index exceeds the ABI range");
            return;
        };
        let element = crate::aster_rt_array_element(std::ptr::from_mut(context), array, index);
        if element.is_null() {
            context.fail("aster.io directory listing could not initialize its result array");
            return;
        }
        // SAFETY: `array` was allocated above with exactly pointer-sized
        // string elements; `index` is within its checked length; no ASTER
        // code can observe the array before the complete Result is written.
        #[allow(unsafe_code)]
        unsafe {
            std::ptr::write_unaligned(element.cast::<*const AsterStrHeader>(), string);
        }
    }
    // SAFETY: all strings and the array now belong to the same selected
    // arena. Only after fully writing every element do we publish `Ok`.
    #[allow(unsafe_code)]
    unsafe {
        write_result_io_error(
            destination,
            total_size,
            ok_tag,
            error_tag,
            ok_payload_offset,
            error_payload_offset,
            kind_offset,
            oscode_offset,
            Ok(array),
            &kind_tags,
        );
    }
}

/// List direct regular files into a persistent `Result<string[], IOError>`.
/// Exported to generated code as `aster_rt_io_list_files`.
#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_list_files(
    context: *mut ExecutionContext,
    directory: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    list_paths(
        context,
        directory,
        destination,
        total_size,
        ok_tag,
        error_tag,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
        kind_tags,
        false,
        false,
    );
}

/// Temporary-arena counterpart of [`aster_rt_io_list_files`].
#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_list_files_temporary(
    context: *mut ExecutionContext,
    directory: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    list_paths(
        context,
        directory,
        destination,
        total_size,
        ok_tag,
        error_tag,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
        kind_tags,
        true,
        false,
    );
}

#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_list_directories(
    context: *mut ExecutionContext,
    directory: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    list_paths(
        context,
        directory,
        destination,
        total_size,
        ok_tag,
        error_tag,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
        kind_tags,
        false,
        true,
    );
}

#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_list_directories_temporary(
    context: *mut ExecutionContext,
    directory: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    list_paths(
        context,
        directory,
        destination,
        total_size,
        ok_tag,
        error_tag,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
        kind_tags,
        true,
        true,
    );
}

/// Read an entire UTF-8 text file into a persistent `Result<string, IOError>`.
/// Exported to generated code as `aster_rt_io_read_all_text`.
#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_read_all_text(
    context: *mut ExecutionContext,
    path: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    read_all_text(
        context,
        path,
        destination,
        total_size,
        ok_tag,
        error_tag,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
        kind_tags,
        false,
    );
}

/// Temporary-arena counterpart of [`aster_rt_io_read_all_text`].
#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_read_all_text_temporary(
    context: *mut ExecutionContext,
    path: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    read_all_text(
        context,
        path,
        destination,
        total_size,
        ok_tag,
        error_tag,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
        kind_tags,
        true,
    );
}

/// Create or truncate a file and write UTF-8 text into it, producing a
/// persistent `Result<int, IOError>` (bytes written on success). Exported to
/// generated code as `aster_rt_io_write_all_text`.
#[allow(
    clippy::not_unsafe_ptr_arg_deref,
    clippy::too_many_arguments,
    clippy::similar_names
)]
pub extern "C" fn aster_rt_io_write_all_text(
    context: *mut ExecutionContext,
    path: *const AsterStrHeader,
    content: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code passes its live hidden ExecutionContext.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if destination.is_null() {
        context.fail("aster.io.WriteAllText received a null destination");
        return;
    }
    let (
        Ok(total_size),
        Ok(ok_payload_offset),
        Ok(error_payload_offset),
        Ok(kind_offset),
        Ok(oscode_offset),
    ) = (
        usize::try_from(total_size),
        usize::try_from(ok_payload_offset),
        usize::try_from(error_payload_offset),
        usize::try_from(kind_offset),
        usize::try_from(oscode_offset),
    )
    else {
        context.fail("aster.io.WriteAllText received a negative layout size");
        return;
    };
    if !result_io_error_layout_fits::<i32>(
        total_size,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
    ) {
        context.fail("aster.io.WriteAllText received a malformed Result layout");
        return;
    }
    // SAFETY: generated code passes a pointer to 9 compiler-computed tag
    // constants (or null on a malformed layout, handled below).
    #[allow(unsafe_code)]
    let Some(kind_tags) = (unsafe { read_kind_tags(kind_tags) }) else {
        context.fail("aster.io.WriteAllText received a malformed IOErrorKind tag layout");
        return;
    };
    // SAFETY: generated code passes its live context and string references
    // owned by that context or the live JIT module.
    #[allow(unsafe_code)]
    let (path_text, content_text) = unsafe { (view(path), view(content)) };
    let (Some(path_text), Some(content_text)) = (path_text, content_text) else {
        context.fail("aster.io.WriteAllText received an invalid UTF-8 string reference");
        return;
    };

    let outcome: Result<i32, (PortableIoErrorKind, i32)> =
        if path_text.is_empty() || path_text.contains('\0') {
            Err((PortableIoErrorKind::InvalidPath, 0))
        } else if content_text.len() as u64 > MAX_FILE_BYTES {
            Err((PortableIoErrorKind::LimitExceeded, 0))
        } else {
            match context
                .filesystem_backend()
                .write_all(path_text, content_text.as_bytes())
            {
                Ok(()) => Ok(i32::try_from(content_text.len()).unwrap_or(i32::MAX)),
                Err(error) => Err(classify(error)),
            }
        };
    // SAFETY: as in `read_all_text`.
    #[allow(unsafe_code)]
    unsafe {
        write_result_io_error(
            destination,
            total_size,
            ok_tag,
            error_tag,
            ok_payload_offset,
            error_payload_offset,
            kind_offset,
            oscode_offset,
            outcome,
            &kind_tags,
        );
    }
}

#[allow(
    clippy::not_unsafe_ptr_arg_deref,
    clippy::too_many_arguments,
    clippy::similar_names
)]
pub extern "C" fn aster_rt_io_append_all_text(
    context: *mut ExecutionContext,
    path: *const AsterStrHeader,
    content: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    if context.is_null() {
        return;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if destination.is_null() {
        context.fail("aster.io.AppendAllText received a null destination");
        return;
    }
    let (
        Ok(total_size),
        Ok(ok_payload_offset),
        Ok(error_payload_offset),
        Ok(kind_offset),
        Ok(oscode_offset),
    ) = (
        usize::try_from(total_size),
        usize::try_from(ok_payload_offset),
        usize::try_from(error_payload_offset),
        usize::try_from(kind_offset),
        usize::try_from(oscode_offset),
    )
    else {
        context.fail("aster.io.AppendAllText received invalid layout metadata");
        return;
    };
    if !result_io_error_layout_fits::<i32>(
        total_size,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
    ) {
        context.fail("aster.io.AppendAllText received a malformed Result layout");
        return;
    }
    #[allow(unsafe_code)]
    let Some(kind_tags) = (unsafe { read_kind_tags(kind_tags) }) else {
        context.fail("aster.io.AppendAllText received invalid error metadata");
        return;
    };
    #[allow(unsafe_code)]
    let (Some(path), Some(content)) = (unsafe { view(path) }, unsafe { view(content) }) else {
        context.fail("aster.io.AppendAllText received invalid string data");
        return;
    };
    let outcome = if path.is_empty() || path.contains('\0') {
        Err((PortableIoErrorKind::InvalidPath, 0))
    } else if content.len() as u64 > MAX_FILE_BYTES {
        Err((PortableIoErrorKind::LimitExceeded, 0))
    } else {
        context
            .filesystem_backend()
            .append_all(path, content.as_bytes())
            .map(|()| i32::try_from(content.len()).unwrap_or(i32::MAX))
            .map_err(classify)
    };
    #[allow(unsafe_code)]
    unsafe {
        write_result_io_error(
            destination,
            total_size,
            ok_tag,
            error_tag,
            ok_payload_offset,
            error_payload_offset,
            kind_offset,
            oscode_offset,
            outcome,
            &kind_tags,
        );
    }
}

/// Private operation codes: 0 `FileExists`, 1 `DirectoryExists`, 2
/// `CreateDirectory`, 3 `DeleteFile`, 4 `DeleteDirectory`.
#[allow(clippy::not_unsafe_ptr_arg_deref, clippy::too_many_arguments)]
pub extern "C" fn aster_rt_io_path_bool(
    context: *mut ExecutionContext,
    operation: i32,
    path: *const AsterStrHeader,
    destination: *mut u8,
    total_size: i32,
    ok_tag: i32,
    error_tag: i32,
    ok_payload_offset: i32,
    error_payload_offset: i32,
    kind_offset: i32,
    oscode_offset: i32,
    kind_tags: *const i32,
) {
    if context.is_null() {
        return;
    }
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    if destination.is_null() {
        context.fail("aster.io path operation received a null destination");
        return;
    }
    let (
        Ok(total_size),
        Ok(ok_payload_offset),
        Ok(error_payload_offset),
        Ok(kind_offset),
        Ok(oscode_offset),
    ) = (
        usize::try_from(total_size),
        usize::try_from(ok_payload_offset),
        usize::try_from(error_payload_offset),
        usize::try_from(kind_offset),
        usize::try_from(oscode_offset),
    )
    else {
        context.fail("aster.io path operation received invalid layout metadata");
        return;
    };
    if !result_io_error_layout_fits::<i8>(
        total_size,
        ok_payload_offset,
        error_payload_offset,
        kind_offset,
        oscode_offset,
    ) {
        context.fail("aster.io path operation received a malformed Result layout");
        return;
    }
    #[allow(unsafe_code)]
    let Some(kind_tags) = (unsafe { read_kind_tags(kind_tags) }) else {
        context.fail("aster.io path operation received invalid error metadata");
        return;
    };
    #[allow(unsafe_code)]
    let Some(path) = (unsafe { view(path) }) else {
        context.fail("aster.io path operation received invalid string data");
        return;
    };
    let outcome = if path.is_empty() || path.contains('\0') {
        Err((PortableIoErrorKind::InvalidPath, 0))
    } else {
        match operation {
            0 => context.filesystem_backend().file_exists(path),
            1 => context.filesystem_backend().directory_exists(path),
            2 => context.filesystem_backend().create_directory(path),
            3 => context.filesystem_backend().delete_file(path),
            4 => context.filesystem_backend().delete_directory(path),
            _ => {
                context.fail("aster.io path operation received an invalid operation code");
                return;
            }
        }
        .map(i8::from)
        .map_err(classify)
    };
    #[allow(unsafe_code)]
    unsafe {
        write_result_io_error(
            destination,
            total_size,
            ok_tag,
            error_tag,
            ok_payload_offset,
            error_payload_offset,
            kind_offset,
            oscode_offset,
            outcome,
            &kind_tags,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FailingFileSystemBackend, FileSystemBackend, FileSystemError, MemoryFileSystemBackend,
        PartialWriteFailureFileSystemBackend, StdFileSystemBackend, classify,
        read_bounded_regular_file, result_io_error_layout_fits,
    };
    use crate::io_error::PortableIoErrorKind;
    use std::cell::Cell;
    use std::io;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_path(label: &str) -> std::path::PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("aster-fs-test-{label}-{}-{id}", std::process::id()))
    }

    #[test]
    fn result_layout_validation_rejects_every_out_of_bounds_payload_shape() {
        assert!(result_io_error_layout_fits::<usize>(32, 8, 16, 0, 4));
        assert!(!result_io_error_layout_fits::<usize>(3, 0, 0, 0, 0));
        assert!(!result_io_error_layout_fits::<usize>(12, 8, 0, 0, 4));
        assert!(!result_io_error_layout_fits::<usize>(20, 0, 16, 0, 4));
        assert!(!result_io_error_layout_fits::<usize>(20, 0, 8, 9, 4));
        assert!(!result_io_error_layout_fits::<usize>(20, 0, 8, 0, 9));
        assert!(!result_io_error_layout_fits::<usize>(
            usize::MAX,
            usize::MAX,
            0,
            0,
            0,
        ));
    }

    #[test]
    fn memory_backend_reads_back_a_seeded_file_exactly() {
        let mut backend = MemoryFileSystemBackend::new().with_file("a.txt", "hello");
        let bytes = backend.read_all("a.txt", 1024).expect("file exists");
        assert_eq!(bytes, b"hello");
    }

    #[test]
    fn memory_backend_reports_not_file_for_a_directory_entry() {
        let mut backend = MemoryFileSystemBackend::new().with_directory("dir");
        let error = backend
            .read_all("dir", 1024)
            .expect_err("directory is not a file");
        assert!(matches!(error, FileSystemError::NotFile));
        let error = backend
            .write_all("dir", b"x")
            .expect_err("cannot write over a directory");
        assert!(matches!(error, FileSystemError::NotFile));
    }

    #[test]
    fn memory_backend_reports_not_found_for_a_missing_path() {
        let mut backend = MemoryFileSystemBackend::new();
        let error = backend
            .read_all("missing.txt", 1024)
            .expect_err("no such file");
        assert!(matches!(error, FileSystemError::Io(e) if e.kind() == io::ErrorKind::NotFound));
    }

    #[test]
    fn memory_backend_lists_only_direct_regular_files_in_ordinal_order() {
        let mut backend = MemoryFileSystemBackend::new()
            .with_directory("root")
            .with_file("root/b.txt", "b")
            .with_file("root/A.txt", "a")
            .with_file("root/a.txt", "a")
            .with_file("root/Ã©.txt", "unicode")
            .with_file("root/non_text.bin", [0_u8, 255])
            .with_directory("root/Sub")
            .with_file("root/Sub/nested.txt", "nested")
            .with_symlink("root/link")
            .with_other("root/pipe");

        let paths = backend
            .list_files("root", 100, 10_000)
            .expect("directory should enumerate");
        assert_eq!(
            paths,
            vec![
                "root/A.txt",
                "root/a.txt",
                "root/b.txt",
                "root/non_text.bin",
                "root/Ã©.txt",
            ]
        );
    }

    #[test]
    fn memory_backend_list_files_rejects_wrong_kind_and_enforces_limits() {
        let mut file = MemoryFileSystemBackend::new().with_file("file", "content");
        assert!(matches!(
            file.list_files("file", 10, 100),
            Err(FileSystemError::NotDirectory)
        ));

        let mut missing = MemoryFileSystemBackend::new();
        assert!(matches!(
            missing.list_files("missing", 10, 100),
            Err(FileSystemError::Io(error)) if error.kind() == io::ErrorKind::NotFound
        ));

        let mut limited = MemoryFileSystemBackend::new()
            .with_directory("root")
            .with_file("root/a", "")
            .with_file("root/b", "");
        assert!(matches!(
            limited.list_files("root", 1, 100),
            Err(FileSystemError::LimitExceeded)
        ));
        assert!(matches!(
            limited.list_files("root", 10, 5),
            Err(FileSystemError::LimitExceeded)
        ));
    }

    #[test]
    fn memory_backend_caps_reads_at_the_probe_limit_simulating_a_growing_file() {
        let content = vec![b'x'; 100];
        let mut backend = MemoryFileSystemBackend::new().with_file("big.txt", content);
        let bytes = backend
            .read_all("big.txt", 10)
            .expect("read is capped, not rejected");
        assert_eq!(bytes.len(), 10);
    }

    #[test]
    fn memory_backend_write_creates_and_truncates() {
        let mut backend = MemoryFileSystemBackend::new().with_file("a.txt", "old-content");
        backend.write_all("a.txt", b"new").expect("write succeeds");
        assert_eq!(backend.read("a.txt"), Some(b"new".to_vec()));
        backend
            .write_all("b.txt", b"created")
            .expect("write creates a new entry");
        assert_eq!(backend.read("b.txt"), Some(b"created".to_vec()));
    }

    #[test]
    fn memory_backend_practical_file_and_directory_lifecycle_is_non_recursive() {
        let mut backend = MemoryFileSystemBackend::new().with_directory("root");
        assert!(
            backend
                .create_directory("root/a")
                .expect("create directory")
        );
        assert!(
            !backend
                .create_directory("root/a")
                .expect("existing directory")
        );
        assert!(
            backend
                .directory_exists("root/a")
                .expect("directory exists")
        );
        assert!(!backend.file_exists("root/a").expect("wrong kind is false"));

        backend
            .append_all("root/a/data.txt", b"one")
            .expect("append creates");
        backend
            .append_all("root/a/data.txt", b"two")
            .expect("append extends");
        assert_eq!(backend.read("root/a/data.txt"), Some(b"onetwo".to_vec()));
        assert_eq!(
            backend
                .list_directories("root", 10, 1_000)
                .expect("list directories"),
            vec!["root/a"]
        );
        assert!(matches!(
            backend.delete_directory("root/a"),
            Err(FileSystemError::Io(error)) if error.kind() == io::ErrorKind::DirectoryNotEmpty
        ));
        assert!(backend.delete_file("root/a/data.txt").expect("delete file"));
        assert!(
            !backend
                .delete_file("root/a/data.txt")
                .expect("missing is false")
        );
        assert!(
            backend
                .delete_directory("root/a")
                .expect("delete empty directory")
        );
        assert!(
            !backend
                .delete_directory("root/a")
                .expect("missing is false")
        );
    }

    #[test]
    fn failing_backend_reports_the_configured_error_kind_for_both_operations() {
        let mut backend = FailingFileSystemBackend::new(io::ErrorKind::PermissionDenied);
        let read_error = backend
            .read_all("x.txt", 1024)
            .expect_err("simulated failure");
        assert!(
            matches!(read_error, FileSystemError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied)
        );
        let write_error = backend
            .write_all("x.txt", b"x")
            .expect_err("simulated failure");
        assert!(
            matches!(write_error, FileSystemError::Io(e) if e.kind() == io::ErrorKind::PermissionDenied)
        );
    }

    #[test]
    fn partial_write_failure_backend_stores_content_but_still_reports_failure() {
        let mut backend = PartialWriteFailureFileSystemBackend::new();
        let error = backend
            .write_all("a.txt", b"partial")
            .expect_err("this backend always reports failure");
        assert!(matches!(error, FileSystemError::Io(_)));
        // The "partial failure" contract: content may have been written even
        // though the operation reported failure.
        assert_eq!(backend.read("a.txt"), Some(b"partial".to_vec()));
    }

    #[test]
    fn classify_maps_not_file_to_the_synthetic_zero_os_code_category() {
        let (kind, os_code) = classify(FileSystemError::NotFile);
        assert_eq!(kind, PortableIoErrorKind::NotFile);
        assert_eq!(os_code, 0);
    }

    #[test]
    fn classify_maps_a_real_io_error_through_classify_io_error() {
        let (kind, _) = classify(FileSystemError::Io(io::Error::from(
            io::ErrorKind::NotFound,
        )));
        assert_eq!(kind, PortableIoErrorKind::NotFound);
    }

    #[test]
    fn std_backend_writes_and_reads_back_a_real_file() {
        let path = unique_temp_path("roundtrip");
        let mut backend = StdFileSystemBackend::default();
        backend
            .write_all(path.to_str().unwrap(), "Olá, ASTER! 🙂".as_bytes())
            .expect("real write succeeds");
        let bytes = backend
            .read_all(path.to_str().unwrap(), 1024)
            .expect("real read succeeds");
        assert_eq!(bytes, "Olá, ASTER! 🙂".as_bytes());
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn std_backend_reports_not_file_for_a_real_directory() {
        let mut backend = StdFileSystemBackend::default();
        let dir = std::env::temp_dir();
        let error = backend
            .read_all(dir.to_str().unwrap(), 1024)
            .expect_err("a directory is not a regular file");
        assert!(matches!(error, FileSystemError::NotFile));
    }

    #[test]
    fn std_backend_reports_not_found_for_a_missing_real_file() {
        let path = unique_temp_path("missing");
        let mut backend = StdFileSystemBackend::default();
        let error = backend
            .read_all(path.to_str().unwrap(), 1024)
            .expect_err("file does not exist");
        assert!(matches!(error, FileSystemError::Io(e) if e.kind() == io::ErrorKind::NotFound));
    }

    #[test]
    fn std_backend_preflights_a_regular_file_at_the_probe_limit() {
        let path = unique_temp_path("capped");
        let mut backend = StdFileSystemBackend::default();
        let content = vec![b'y'; 200];
        backend
            .write_all(path.to_str().unwrap(), &content)
            .expect("real write succeeds");
        let error = backend
            .read_all(path.to_str().unwrap(), 50)
            .expect_err("known oversized regular file is rejected before reading");
        assert!(matches!(error, FileSystemError::LimitExceeded));
        std::fs::remove_file(&path).ok();
    }

    struct CountingReader {
        reads: Rc<Cell<usize>>,
        bytes: io::Cursor<Vec<u8>>,
    }

    impl io::Read for CountingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            self.bytes.read(buffer)
        }
    }

    #[test]
    fn regular_file_preflight_uses_the_probe_threshold_without_reading() {
        for known_length in [50, 51] {
            let reads = Rc::new(Cell::new(0));
            let reader = CountingReader {
                reads: Rc::clone(&reads),
                bytes: io::Cursor::new(vec![b'x'; 51]),
            };
            let error = read_bounded_regular_file(reader, Some(known_length), 50)
                .expect_err("metadata at or beyond the probe rejects immediately");
            assert!(matches!(error, FileSystemError::LimitExceeded));
            assert_eq!(reads.get(), 0);
        }

        let reads = Rc::new(Cell::new(0));
        let reader = CountingReader {
            reads: Rc::clone(&reads),
            bytes: io::Cursor::new(vec![b'x'; 49]),
        };
        let bytes = read_bounded_regular_file(reader, Some(49), 50)
            .expect("one byte below the probe remains allowed");
        assert_eq!(bytes.len(), 49);
        assert!(reads.get() > 0);
    }

    #[test]
    fn std_backend_write_all_text_writing_over_a_directory_is_not_file() {
        let mut backend = StdFileSystemBackend::default();
        let dir = std::env::temp_dir();
        let error = backend
            .write_all(dir.to_str().unwrap(), b"x")
            .expect_err("cannot write over a real directory");
        assert!(matches!(error, FileSystemError::NotFile));
    }
}
