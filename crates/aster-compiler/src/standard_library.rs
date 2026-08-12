//! Standard library sources for official `aster.*` namespaces.
//!
//! The embedded sources (via `include_str!`) are the source of truth for
//! development builds and always serve as the fallback. At runtime, the CLI
//! can load an external stdlib from disk (`ASTER_STDLIB` or exe-relative path);
//! `StandardLibrary::from_path` validates the layout before use.

use std::{
    borrow::Cow,
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use aster_hir::{FileIoResultLayout, Intrinsic, RuntimeErrorKind};

const MATH_SOURCE: &str = include_str!("../../../stdlib/aster/math.aster");
const TEXT_SOURCE: &str = include_str!("../../../stdlib/aster/text/text.aster");
const CORE_SOURCE: &str = include_str!("../../../stdlib/aster/core/core.aster");
const IO_SOURCE: &str = include_str!("../../../stdlib/aster/io/io.aster");
const COLLECTIONS_SOURCE: &str =
    include_str!("../../../stdlib/aster/collections/collections.aster");

/// All stdlib modules with their on-disk paths relative to the stdlib root.
const STDLIB_MODULES: &[(&str, &str)] = &[
    ("aster.math", "aster/math.aster"),
    ("aster.text", "aster/text/text.aster"),
    ("aster.core", "aster/core/core.aster"),
    ("aster.io", "aster/io/io.aster"),
    ("aster.collections", "aster/collections/collections.aster"),
];

/// Namespace of the official core standard library, and the single source of
/// truth for where the official `Result`/`Option` declarations live. Passes that
/// need the identity of an official type derive it from this constant during
/// bootstrap and then compare resolved identities, never raw type spellings.
pub(crate) const CORE_NAMESPACE: &str = "aster.core";
pub(crate) const COLLECTIONS_NAMESPACE: &str = "aster.collections";
pub(crate) const STRING_BUILDER_NAME: &str = "aster.core::StringBuilder";

fn canonical_generic_argument(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect()
}

/// The concrete, namespace-qualified `Option<target>` specialization name
/// (e.g. `aster.core::Option<int>`) that `string.TryParse*()` targets.
/// `target` is one of the primitive names `StringOperation::parse_target_name`
/// returns (`"bool"`, `"int"`, `"uint"`, `"long"`, `"ulong"`). The single
/// place every layer (generic-type discovery, semantic analysis, HIR
/// lowering) derives this spelling from, so it can never drift between them.
pub(crate) fn option_specialization_name(target: &str) -> String {
    format!(
        "{CORE_NAMESPACE}::Option<{}>",
        canonical_generic_argument(target)
    )
}

pub(crate) fn dictionary_entry_specialization_name(key: &str, value: &str) -> String {
    format!(
        "{COLLECTIONS_NAMESPACE}::DictionaryEntry<{},{}>",
        canonical_generic_argument(key),
        canonical_generic_argument(value)
    )
}

/// Stdlib module sources, either embedded at compile time or loaded from disk.
///
/// Use [`StandardLibrary::embedded()`] for the embedded dev fallback, or
/// [`StandardLibrary::from_path()`] to load from an installed location.
#[derive(Clone)]
pub struct StandardLibrary {
    modules: HashMap<&'static str, Cow<'static, str>>,
}

impl StandardLibrary {
    /// Create a stdlib from the sources embedded in the binary at compile time.
    /// Always succeeds; used as the development fallback.
    #[must_use]
    pub fn embedded() -> Self {
        Self {
            modules: HashMap::from([
                ("aster.math", Cow::Borrowed(MATH_SOURCE)),
                ("aster.text", Cow::Borrowed(TEXT_SOURCE)),
                ("aster.core", Cow::Borrowed(CORE_SOURCE)),
                ("aster.io", Cow::Borrowed(IO_SOURCE)),
                ("aster.collections", Cow::Borrowed(COLLECTIONS_SOURCE)),
            ]),
        }
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    /// Load stdlib sources from a directory on disk.
    ///
    /// `path` must be the stdlib root: a directory containing `aster/math.aster`,
    /// `aster/core/core.aster`, etc. Returns a descriptive error string if the
    /// directory is missing, is a file, or any required source file cannot be read.
    ///
    /// # Errors
    ///
    /// Returns an error string describing what was wrong with the stdlib path.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let metadata = fs::metadata(path).map_err(|error| {
            format!(
                "stdlib path `{}` is not accessible: {error}",
                path.display()
            )
        })?;
        if !metadata.is_dir() {
            return Err(format!(
                "stdlib path `{}` is a file, not a directory",
                path.display()
            ));
        }
        let mut modules = HashMap::new();
        for (name, relative) in STDLIB_MODULES {
            let file_path = path.join(relative);
            let source = fs::read_to_string(&file_path).map_err(|error| {
                format!(
                    "stdlib file `{}` could not be read: {error}",
                    file_path.display()
                )
            })?;
            modules.insert(*name, Cow::Owned(source));
        }
        Ok(Self { modules })
    }

    pub(crate) fn source(&self, module: &str) -> Option<&str> {
        self.modules.get(module).map(Cow::as_ref)
    }

    pub(crate) fn intrinsic_bindings(&self) -> HashMap<String, Intrinsic> {
        let mut bindings = HashMap::new();
        if self.modules.contains_key("aster.math") {
            bindings.extend([
                (
                    "aster.math::__AbsIntOverflow".to_owned(),
                    Intrinsic::ReportRuntimeError(RuntimeErrorKind::MathAbsIntOverflow),
                ),
                (
                    "aster.math::__AbsLongOverflow".to_owned(),
                    Intrinsic::ReportRuntimeError(RuntimeErrorKind::MathAbsLongOverflow),
                ),
                (
                    "aster.math::__ClampInvalidRange".to_owned(),
                    Intrinsic::ReportRuntimeError(RuntimeErrorKind::MathClampInvalidRange),
                ),
            ]);
        }
        if self.modules.contains_key("aster.io") {
            bindings.extend([
                ("aster.io::Write".to_owned(), Intrinsic::ConsoleWrite),
                (
                    "aster.io::WriteLine".to_owned(),
                    Intrinsic::ConsoleWriteLine,
                ),
                ("aster.io::ReadLine".to_owned(), Intrinsic::ConsoleReadLine),
                (
                    // Marker only: `hir_lowering::declarations::function`
                    // replaces this placeholder payload with the real,
                    // symbol-resolved `FileIoResultLayout` once it can
                    // resolve `Result<string, IOError>`'s cases/fields (this
                    // point, during `StandardLibrary` bootstrap, is too early
                    // -- no symbols exist yet).
                    "aster.io::ReadAllText".to_owned(),
                    Intrinsic::FileReadAllText(FileIoResultLayout::UNRESOLVED),
                ),
                (
                    "aster.io::WriteAllText".to_owned(),
                    Intrinsic::FileWriteAllText(FileIoResultLayout::UNRESOLVED),
                ),
                (
                    "aster.io::ListFiles".to_owned(),
                    Intrinsic::FileListFiles(FileIoResultLayout::UNRESOLVED),
                ),
            ]);
        }
        bindings
    }

    pub(crate) fn display_path(module: &str) -> PathBuf {
        let mut path = PathBuf::from("<stdlib>");
        for segment in module.split('.') {
            path.push(segment);
        }
        if matches!(
            module,
            "aster.text" | "aster.core" | "aster.io" | "aster.collections"
        ) {
            path.push(
                module
                    .rsplit('.')
                    .next()
                    .expect("official namespace segment"),
            );
        }
        path.set_extension("aster");
        path
    }
}

pub(crate) fn is_official_name(name: &str) -> bool {
    name == "aster" || name.starts_with("aster.")
}
