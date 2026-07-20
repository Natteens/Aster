//! Public compiler pipeline for Aster.

mod application;
mod constexpr;
mod escape_analysis;
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
    synthesize_default_constructors(&mut module);
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
        let mut mir = mir_lowering::lower(&hir);
        escape_analysis::assign_allocation_regions(&mut mir);
        Ok(Compilation {
            tokens,
            module,
            hir,
            mir,
            diagnostics,
        })
    }
}

/// Give every non-static class without a declared constructor an implicit public
/// parameterless one, so `new Type()` works like in C#. Field initializers are
/// prepended to every constructor body during HIR lowering, including this one.
fn synthesize_default_constructors(module: &mut Module) {
    use aster_syntax::{Block, FunctionDeclaration, Item, Member, TypeRef, Visibility};

    for item in &mut module.items {
        let Item::Class(class) = item else {
            continue;
        };
        if class.is_static
            || class
                .members
                .iter()
                .any(|member| matches!(member, Member::Method(method) if method.constructor))
        {
            continue;
        }
        // An implicit constructor has an empty body, so it is only synthesized when
        // every field can pass definite initialization on its own: either through a
        // field initializer or by zero-defaulting. Only primitive value types are
        // known to zero-default at this stage; other classes keep requiring an
        // explicit constructor, exactly as before.
        let every_field_defaults = class.members.iter().all(|member| {
            let Member::Field(field) = member else {
                return true;
            };
            field.initializer.is_some() || zero_defaults(&field.type_ref.name)
        });
        if !every_field_defaults {
            continue;
        }
        class.members.push(Member::Method(FunctionDeclaration {
            constructor: true,
            is_static: false,
            type_parameters: Vec::new(),
            visibility: Visibility::Public,
            return_type: TypeRef::new("void", class.span),
            name: class.name.clone(),
            parameters: Vec::new(),
            body: Some(Block {
                statements: Vec::new(),
                span: class.span,
            }),
            span: class.span,
        }));
    }
}

/// Whether a field of this declared type is defined to zero-default, making it
/// valid without a field initializer inside an empty constructor body. Strings
/// and user-declared types are excluded: strings have no default value and a
/// user type cannot be classified before semantic analysis.
fn zero_defaults(type_name: &str) -> bool {
    aster_types::from_name(type_name)
        .is_some_and(|primitive| primitive != aster_types::Primitive::String)
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
