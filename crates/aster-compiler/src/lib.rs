//! Public compiler pipeline for Aster.

mod application;
mod constexpr;
mod generics;
mod hir_lowering;
mod mir_lowering;
mod primitives;
mod project;
mod semantic;
mod standard_library;
mod type_names;

use aster_diagnostics::{Diagnostic, Severity};
use aster_syntax::{Module, Token, lex, parse};

pub use application::{
    ApplicationDiagnostic, ApplicationEntry, find_manifest_path, select_application_entry,
};
pub use aster_hir as hir;
pub use aster_mir as mir;
pub use project::{
    ProjectCompilation, ProjectDiagnostic, ProjectSource, ProjectSourceOrigin, compile_project,
};

/// Successful output of validation plus HIR and MIR lowering.
#[derive(Clone, Debug, PartialEq)]
pub struct Compilation {
    pub tokens: Vec<Token>,
    pub module: Module,
    pub hir: hir::Module,
    pub mir: mir::Module,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex, parse, semantically validate, and lower one Aster source file to HIR and MIR.
///
/// # Errors
///
/// Returns positioned diagnostics for lexical, syntactic, or semantic errors.
pub fn compile(source: &str) -> Result<Compilation, Vec<Diagnostic>> {
    let tokens = lex(source)?;
    let module = parse(tokens.clone())?;
    compile_module(tokens, module, &std::collections::HashMap::new())
}

fn compile_module(
    tokens: Vec<Token>,
    mut module: Module,
    intrinsic_bindings: &std::collections::HashMap<String, hir::Intrinsic>,
) -> Result<Compilation, Vec<Diagnostic>> {
    let generic_diagnostics = generics::monomorphize(&mut module);
    if !generic_diagnostics.is_empty() {
        return Err(generic_diagnostics);
    }
    let (diagnostics, semantic_model) = semantic::validate(&module);
    if diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        Err(diagnostics)
    } else {
        let hir = hir_lowering::lower(&module, &semantic_model, intrinsic_bindings);
        let mir = mir_lowering::lower(&hir);
        Ok(Compilation {
            tokens,
            module,
            hir,
            mir,
            diagnostics,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::compile;

    #[test]
    fn old_ecs_syntax_is_a_controlled_diagnostic_not_a_silent_drop() {
        // `component`/`system`/`foreach` are no longer language constructs; source
        // using the old syntax must fail with an ordinary diagnostic, not be
        // silently accepted or dropped during lowering.
        let source = "component Position { float x; } system Bad(Position read) { foreach (position) { position.x += position.x; } }";
        assert!(compile(source).is_err());
    }
}
