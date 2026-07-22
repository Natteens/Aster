//! Embedded sources for official `aster.*` namespaces.
//!
//! The checked-in files are the public source of truth. Embedding them keeps
//! namespace discovery independent of the process working directory and prevents
//! user projects from shadowing official namespaces.

use std::{collections::HashMap, path::PathBuf};

use aster_hir::{Intrinsic, RuntimeErrorKind};

const MATH_SOURCE: &str = include_str!("../../../stdlib/aster/math.aster");
const TEXT_SOURCE: &str = include_str!("../../../stdlib/aster/text/text.aster");
const CORE_SOURCE: &str = include_str!("../../../stdlib/aster/core/core.aster");
const IO_SOURCE: &str = include_str!("../../../stdlib/aster/io/io.aster");

/// Namespace of the official core standard library, and the single source of
/// truth for where the official `Result`/`Option` declarations live. Passes that
/// need the identity of an official type derive it from this constant during
/// bootstrap and then compare resolved identities, never raw type spellings.
pub(crate) const CORE_NAMESPACE: &str = "aster.core";

/// The concrete, namespace-qualified `Option<target>` specialization name
/// (e.g. `aster.core::Option<int>`) that `string.TryParse*()` targets.
/// `target` is one of the primitive names `StringOperation::parse_target_name`
/// returns (`"bool"`, `"int"`, `"uint"`, `"long"`, `"ulong"`). The single
/// place every layer (generic-type discovery, semantic analysis, HIR
/// lowering) derives this spelling from, so it can never drift between them.
pub(crate) fn option_specialization_name(target: &str) -> String {
    format!("{CORE_NAMESPACE}::Option<{target}>")
}

#[derive(Clone)]
pub(crate) struct StandardLibrary {
    modules: HashMap<&'static str, &'static str>,
}

impl StandardLibrary {
    pub(crate) fn embedded() -> Self {
        Self {
            modules: HashMap::from([
                ("aster.math", MATH_SOURCE),
                ("aster.text", TEXT_SOURCE),
                ("aster.core", CORE_SOURCE),
                ("aster.io", IO_SOURCE),
            ]),
        }
    }

    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            modules: HashMap::new(),
        }
    }

    pub(crate) fn source(&self, module: &str) -> Option<&'static str> {
        self.modules.get(module).copied()
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
            ]);
        }
        bindings
    }

    pub(crate) fn display_path(module: &str) -> PathBuf {
        let mut path = PathBuf::from("<stdlib>");
        for segment in module.split('.') {
            path.push(segment);
        }
        if matches!(module, "aster.text" | "aster.core" | "aster.io") {
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
