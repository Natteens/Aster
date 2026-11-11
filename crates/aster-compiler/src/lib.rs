//! Public compiler pipeline for Aster.

mod application;
mod array_loop_optimization;
mod constexpr;
mod declaration_key;
mod escape_analysis;
mod generics;
mod git_source;
mod hir_lowering;
mod lifetime_analysis;
mod local_object_elimination;
mod lockfile;
mod loop_string_concat_rewrite;
mod manifest;
mod mir_lowering;
mod mir_optimizer;
mod owned_regions;
mod primitives;
mod project;
mod semantic;
mod source_declaration_index;
mod standard_library;
mod temporary_subregions;
mod type_names;

use std::{any::Any, path::Path};

use aster_diagnostics::{Diagnostic, Severity, Span};
use aster_mir::validate::ValidationError;
use aster_mir::validate::executable::Phase;
use aster_syntax::{Module, Token, lex, parse};

const COMPILER_STACK_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum CompilerWorkerError {
    Spawn(std::io::Error),
    Panic(String),
}

impl std::fmt::Display for CompilerWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "could not start the compiler: {error}"),
            Self::Panic(message) => write!(formatter, "the compiler panicked: {message}"),
        }
    }
}

/// Run one complete compiler entry point on the single centrally configured
/// compiler stack. Callers must pass the inner implementation, never another
/// public entry point, so project compilation cannot nest compiler workers.
pub(crate) fn run_on_compiler_stack<T, F>(operation: F) -> Result<T, CompilerWorkerError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let worker = std::thread::Builder::new()
        .name("aster-compiler".to_owned())
        .stack_size(COMPILER_STACK_BYTES)
        .spawn(operation)
        .map_err(CompilerWorkerError::Spawn)?;
    worker
        .join()
        .map_err(|payload| CompilerWorkerError::Panic(panic_payload_text(payload)))
}

fn panic_payload_text(payload: Box<dyn Any + Send>) -> String {
    match payload.downcast::<String>() {
        Ok(message) => *message,
        Err(payload) => match payload.downcast::<&'static str>() {
            Ok(message) => (*message).to_owned(),
            Err(_) => "non-string panic payload".to_owned(),
        },
    }
}

pub use application::{ApplicationDiagnostic, ApplicationEntry, select_application_entry};
pub use aster_hir as hir;
pub use aster_mir as mir;
pub use manifest::{find_manifest_path, find_manifest_path_from_directory};
pub use project::{
    FetchSummary, ProjectCompilation, ProjectDiagnostic, ProjectSource, ProjectSourceOrigin,
    TestDescriptor, compile_project, compile_project_for_tests, fetch_dependencies,
};
pub use standard_library::StandardLibrary;
#[doc(hidden)]
pub use temporary_subregions::{
    AarmTemporarySubregionCoalescingOpportunity, AarmTemporarySubregionCostEstimate,
    AarmTemporarySubregionLoweringError, AarmTemporarySubregionLoweringReport,
    AarmTemporarySubregionProfitabilityPolicy, estimate_aarm_coalescing_opportunities_for_research,
    estimate_aarm_temporary_subregion_costs_for_research,
    lower_aarm_temporary_subregions_for_research,
    lower_aarm_temporary_subregions_with_policy_for_research,
};

/// Compile a project using a custom standard library (e.g. loaded from an
/// installed location via [`StandardLibrary::from_path`]).
///
/// # Errors
///
/// Returns sourced diagnostics for any compilation failure.
pub fn compile_project_with_stdlib(
    path: &Path,
    stdlib: StandardLibrary,
) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    project::compile_project_with_standard_library(path, stdlib)
}

/// Compile a project and its root-package `tests/` directory with a custom
/// standard library. This is the CLI test-runner seam; normal commands keep
/// using [`compile_project_with_stdlib`].
///
/// # Errors
///
/// Returns sourced diagnostics for project loading, test discovery, or any
/// compilation failure.
pub fn compile_project_for_tests_with_stdlib(
    path: &Path,
    stdlib: StandardLibrary,
) -> Result<ProjectCompilation, Vec<ProjectDiagnostic>> {
    project::compile_project_with_standard_library_for_tests(path, stdlib)
}

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
    compile_with_options(source, true, true, true)
}

/// Compile one source file with the loop-carried concat rewrite disabled.
///
/// This is a benchmark/test seam only; normal ASTER compilation always uses
/// the optimization when its narrow structural proof succeeds.
#[doc(hidden)]
pub fn compile_without_loop_string_concat_rewrite_for_research(
    source: &str,
) -> Result<Compilation, Vec<Diagnostic>> {
    compile_with_options(source, false, true, true)
}

/// Compile one source file with the general MIR optimizer disabled.
///
/// This is a benchmark/test seam only; normal ASTER compilation always runs
/// the fixed backend-neutral optimizer stage.
#[doc(hidden)]
pub fn compile_without_mir_optimizer_for_research(
    source: &str,
) -> Result<Compilation, Vec<Diagnostic>> {
    compile_with_options(source, true, false, false)
}

/// Compile one source file with canonical array-loop optimization disabled.
///
/// This is a benchmark/test seam only; normal ASTER compilation always runs
/// the conservative backend-neutral proof pass.
#[doc(hidden)]
pub fn compile_without_array_loop_optimization_for_research(
    source: &str,
) -> Result<Compilation, Vec<Diagnostic>> {
    compile_with_options(source, true, true, false)
}

fn compile_with_options(
    source: &str,
    rewrite_loop_string_concat: bool,
    optimize_mir: bool,
    optimize_array_loops: bool,
) -> Result<Compilation, Vec<Diagnostic>> {
    let source = source.to_owned();
    run_on_compiler_stack(move || {
        compile_with_options_inner(
            &source,
            rewrite_loop_string_concat,
            optimize_mir,
            optimize_array_loops,
        )
    })
    .map_err(|error| vec![Diagnostic::error(error.to_string(), Span::default())])?
}

fn compile_with_options_inner(
    source: &str,
    rewrite_loop_string_concat: bool,
    optimize_mir: bool,
    optimize_array_loops: bool,
) -> Result<Compilation, Vec<Diagnostic>> {
    let tokens = lex(source)?;
    let module = parse(tokens.clone())?;
    compile_module(
        tokens,
        module,
        &std::collections::HashMap::new(),
        rewrite_loop_string_concat,
        optimize_mir,
        optimize_array_loops,
    )
}

fn compile_module(
    tokens: Vec<Token>,
    mut module: Module,
    intrinsic_bindings: &std::collections::HashMap<String, hir::Intrinsic>,
    rewrite_loop_string_concat: bool,
    optimize_mir: bool,
    optimize_array_loops: bool,
) -> Result<Compilation, Vec<Diagnostic>> {
    let deferred_diagnostics = semantic::validate_deferred_language_surfaces(&mut module);
    if !deferred_diagnostics.is_empty() {
        return Err(deferred_diagnostics);
    }
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
        check_mir(&mir, "MIR lowering");
        if rewrite_loop_string_concat {
            loop_string_concat_rewrite::rewrite(&mut mir);
            check_mir(&mir, "loop string-concat rewrite");
        }
        if optimize_mir {
            mir_optimizer::optimize(&mut mir);
            check_mir(&mir, "MIR optimizer");
        }
        if optimize_array_loops {
            array_loop_optimization::optimize(&mut mir);
            check_mir(&mir, "array-loop optimization");
        }
        escape_analysis::assign_allocation_regions(&mut mir);
        check_mir(&mir, "escape analysis");
        local_object_elimination::eliminate(&mut mir);
        check_mir(&mir, "local object elimination");
        temporary_subregions::lower_production_aarm_temporary_subregions(&mut mir);
        check_mir_at(
            &mir,
            "AARM temporary subregions",
            Phase::TemporarySubregions,
        );
        owned_regions::lower(&mut mir);
        check_mir_at(&mir, "owned regions", Phase::Final);
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
            is_test: false,
            is_static: false,
            is_async: false,
            is_foreign: false,
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
    use super::{CompilerWorkerError, compile, run_on_compiler_stack};

    #[test]
    fn old_ecs_syntax_is_a_controlled_diagnostic_not_a_silent_drop() {
        // `component`/`system`/`foreach` are no longer language constructs; source
        // using the old syntax must fail with an ordinary diagnostic, not be
        // silently accepted or dropped during lowering.
        let source = "component Position { float x; } system Bad(Position read) { foreach (position) { position.x += position.x; } }";
        assert!(compile(source).is_err());
    }

    #[test]
    fn compiler_worker_preserves_string_panic_payloads() {
        let error = run_on_compiler_stack(|| -> () { panic!("specific compiler failure") })
            .expect_err("worker panic should be reported");
        assert!(matches!(
            error,
            CompilerWorkerError::Panic(message) if message == "specific compiler failure"
        ));
    }

    #[test]
    fn repeated_compiler_entry_points_use_independent_workers() {
        for _ in 0..32 {
            let compilation = compile("public int Main() { return 42; }")
                .expect("small compilation should succeed repeatedly");
            assert_ne!(compilation.mir.functions, [] as [aster_mir::Function; 0]);
        }
    }

    #[test]
    fn concurrent_compiler_entry_points_are_independent() {
        let workers = (0..8)
            .map(|value| {
                std::thread::spawn(move || {
                    let source = format!("public int Main() {{ return {value}; }}");
                    compile(&source).expect("independent compilation succeeds")
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_ne!(
                worker.join().expect("test worker completes").mir.functions,
                [] as [aster_mir::Function; 0]
            );
        }
    }
}

/// Debug builds validate MIR after every compiler pass with the backend-neutral
/// validator, so a pass that breaks a MIR invariant fails where it happens
/// instead of at code generation. A well-formed construct that no backend
/// supports yet is left for the backend to reject. Release builds skip it.
fn check_mir(mir: &aster_mir::Module, pass: &str) {
    check_mir_at(mir, pass, Phase::Lowered);
}

/// [`check_mir`] for a pass after which the region checks of `phase` apply.
fn check_mir_at(mir: &aster_mir::Module, pass: &str, phase: Phase) {
    if !cfg!(debug_assertions) {
        return;
    }
    // Typing: a well-formed construct no backend supports yet (a module
    // global) is the backend's to reject.
    if let Err(ValidationError::Invalid(message)) = aster_mir::validate::validate_types(mir) {
        panic!("invalid MIR after {pass}: {message}");
    }
    // Executability: the region checks report violations as unsupported for
    // hand-built MIR, but a compiler pass never emits a region it cannot
    // execute, so anything but a worker policy rejection is a pass defect.
    match aster_mir::validate::executable::validate_module(mir, phase) {
        Ok(()) | Err(ValidationError::Rejected(_)) => {}
        Err(error) => panic!("invalid MIR after {pass}: {error}"),
    }
}

#[cfg(test)]
mod check_mir_tests {
    // A loop keeps typed assignments alive through constant propagation.
    const SOURCE: &str =
        "public int Run() { int t = 0; for (int i = 0; i < 4; i++) { t = t + i; } return t; }";

    #[test]
    fn pipeline_output_passes_the_per_pass_check() {
        let compilation = super::compile(SOURCE).expect("valid program");
        super::check_mir(&compilation.mir, "test");
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "invalid MIR after corrupting pass")
    )]
    fn a_pass_that_breaks_local_types_is_caught() {
        let mut compilation = super::compile(SOURCE).expect("valid program");
        let function = compilation
            .mir
            .functions
            .iter_mut()
            .find(|function| function.name == "Run")
            .expect("entry function");
        for local in &mut function.locals {
            local.type_ = aster_mir::Type::Bool;
        }
        super::check_mir(&compilation.mir, "corrupting pass");
    }

    #[test]
    #[cfg_attr(
        debug_assertions,
        should_panic(expected = "invalid MIR after owned regions")
    )]
    fn an_unbalanced_owned_region_is_caught_in_the_final_phase() {
        let mut compilation = super::compile(SOURCE).expect("valid program");
        let function = compilation
            .mir
            .functions
            .iter_mut()
            .find(|function| function.name == "Run")
            .expect("entry function");
        function.blocks[0].instructions.insert(
            0,
            aster_mir::Instruction::OwnedRegionExit {
                id: aster_mir::OwnedRegionId(0),
                invalidated: Vec::new(),
            },
        );
        // Not yet applicable before owned-region lowering.
        super::check_mir_at(
            &compilation.mir,
            "local object elimination",
            super::Phase::Lowered,
        );
        super::check_mir_at(&compilation.mir, "owned regions", super::Phase::Final);
    }
}
