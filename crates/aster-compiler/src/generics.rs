use std::{collections::HashMap, fmt::Write};

use aster_diagnostics::Diagnostic;
use aster_syntax::{
    BinaryOperator, Block, EnumDeclaration, Expression, ExpressionKind, FunctionDeclaration,
    InterpolatedPart, Item, Literal, Member, Module, Statement, SwitchCase, SwitchExpressionCase,
    TypeDeclaration, TypeParameter, TypeRef, VariableDeclaration, VariableKind,
    visit::{
        AstVisitorMut, walk_expression_mut, walk_switch_case_mut, walk_switch_expression_case_mut,
    },
};

use crate::type_names::TypeName;

mod discovery;
mod inference;
mod specialization;
mod substitution;
mod templates;

use inference::{infer_type, literal_type, variable_type};
use specialization::GenericTypeConcretizer;
use substitution::{TypeSubstituter, substitute_name, substitutions};
use templates::GenericTypeTemplate;

/// What a linked type declaration is, as far as a `where` constraint needs to
/// know.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeclarationKind {
    Class,
    Struct,
    Interface,
    Enum,
}

impl DeclarationKind {
    fn describe(self) -> &'static str {
        match self {
            Self::Class => "a class",
            Self::Struct => "a struct",
            Self::Interface => "an interface",
            Self::Enum => "an enum",
        }
    }
}

/// The minimum generic-layer facts about a linked declaration required to judge
/// a constraint. This is deliberately not a second `semantic::TypeInfo`:
/// semantic analysis remains the authority for whether a class actually
/// implements the members of every interface it nominally lists.
#[derive(Clone, Copy)]
struct DeclarationFacts {
    kind: DeclarationKind,
    generic: bool,
}

pub(crate) fn monomorphize(module: &mut Module) -> Vec<Diagnostic> {
    Monomorphizer::new(module).run(module)
}

struct Monomorphizer {
    templates: HashMap<String, FunctionDeclaration>,
    type_templates: HashMap<String, GenericTypeTemplate>,
    enum_templates: HashMap<String, EnumDeclaration>,
    returns: HashMap<String, String>,
    fields: HashMap<(String, String), String>,
    methods: HashMap<(String, String), String>,
    cache: HashMap<(String, Vec<String>), String>,
    active: Vec<(String, Vec<String>)>,
    generated: Vec<FunctionDeclaration>,
    type_cache: HashMap<(String, Vec<TypeName>), String>,
    type_active: Vec<(String, Vec<TypeName>)>,
    generated_types: Vec<Item>,
    /// Kind and genericity of every linked type declaration, for constraint
    /// well-formedness.
    declarations: HashMap<String, DeclarationFacts>,
    /// Linked class name to the interfaces it nominally lists, for constraint
    /// satisfaction. Generated class specializations are added as they are
    /// produced, so `Wrapper<int>` keeps `Wrapper<T>`'s interface relation.
    class_interfaces: HashMap<String, Vec<String>>,
    diagnostics: Vec<Diagnostic>,
}

impl Monomorphizer {
    /// The first-subset nominal satisfaction relation: a concrete argument
    /// satisfies a required interface when it *is* that interface, or when it
    /// is a class whose linked declaration nominally lists it. No structural
    /// member scanning happens here; semantic analysis still rejects a class
    /// that lists an interface it does not actually implement.
    fn satisfies(&self, concrete: &str, interface: &str) -> bool {
        concrete == interface
            || self
                .class_interfaces
                .get(concrete)
                .is_some_and(|implemented| implemented.iter().any(|listed| listed == interface))
    }

    /// Report every concrete argument that fails one of its parameter's
    /// constraints. Called before cache lookup and before any clone is
    /// generated, so no request path and no repeated request site can bypass
    /// the contract.
    fn check_constraints(
        &mut self,
        parameters: &[TypeParameter],
        concrete: &[String],
        span: aster_diagnostics::Span,
    ) -> bool {
        let mut satisfied = true;
        for (parameter, argument) in parameters.iter().zip(concrete) {
            for constraint in &parameter.constraints {
                if self.satisfies(argument, &constraint.name) {
                    continue;
                }
                satisfied = false;
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "type argument `{argument}` does not satisfy constraint `{}: {}`",
                            parameter.name, constraint.name
                        ),
                        span,
                    )
                    .with_help(format!(
                        "pass a class that implements `{}`, or the interface itself",
                        constraint.name
                    )),
                );
            }
        }
        satisfied
    }

    fn run(mut self, module: &mut Module) -> Vec<Diagnostic> {
        self.validate_templates();
        self.reject_reserved_task_template(module);
        // Discovery is intentionally synchronous and depth-first: cache entries are installed
        // before recursive materialization, and generated dependencies are accumulated first.
        // Open templates are removed before any concrete item is rewritten; only generated closed
        // declarations are appended below, so semantic analysis and lowering never see templates.
        module.items.retain(|item| {
            !matches!(item, Item::Function(function) if !function.type_parameters.is_empty())
                && !matches!(item, Item::Class(value) | Item::Struct(value) | Item::Interface(value) if !value.type_parameters.is_empty())
                && !matches!(item, Item::Enum(value) if !value.type_parameters.is_empty())
        });
        for item in &mut module.items {
            GenericTypeConcretizer::new(&mut self).visit_item_mut(item);
            match item {
                Item::Function(function) => self.function(function),
                Item::Class(declaration)
                | Item::Struct(declaration)
                | Item::Interface(declaration) => {
                    self.analyze_type_declaration(declaration);
                }
                Item::Enum(_) | Item::Variable(_) => {}
            }
        }
        module.items.append(&mut self.generated_types);
        module
            .items
            .extend(self.generated.drain(..).map(Item::Function));
        self.diagnostics
    }

    /// A generic type template named `Task` or `List` is stripped from
    /// `module.items` below (open templates never reach semantic analysis),
    /// so `semantic::validate_no_reserved_type_names` cannot see it. Both are
    /// reserved regardless of arity, so this catches the generic-template
    /// case at the one point it is still visible; the non-generic case is
    /// caught later, in `semantic`.
    fn reject_reserved_task_template(&mut self, module: &Module) {
        for item in &module.items {
            let (kind, name, span) = match item {
                Item::Class(value) if !value.type_parameters.is_empty() => {
                    ("class", &value.name, value.span)
                }
                Item::Struct(value) if !value.type_parameters.is_empty() => {
                    ("struct", &value.name, value.span)
                }
                Item::Interface(value) if !value.type_parameters.is_empty() => {
                    ("interface", &value.name, value.span)
                }
                Item::Enum(value) if !value.type_parameters.is_empty() => {
                    ("enum", &value.name, value.span)
                }
                _ => continue,
            };
            if name == "Task" {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`{name}` is reserved for the intrinsic task system and cannot be declared as a generic {kind} template"
                        ),
                        span,
                    )
                    .with_help(
                        "rename this type; `Task<T>` is a built-in type, not something a program can declare",
                    ),
                );
            }
            if name == "List" {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`{name}` is reserved for the built-in `List<T>` type and cannot be declared as a generic {kind} template"
                        ),
                        span,
                    )
                    .with_help(
                        "rename this type; `List<T>` is a built-in type, not something a program can declare",
                    ),
                );
            }
            if name == "Dictionary" {
                self.diagnostics.push(
                    Diagnostic::error(
                        format!("`{name}` is reserved for the built-in `Dictionary<K, V>` type and cannot be declared as a generic {kind} template"),
                        span,
                    )
                    .with_help("rename this type; `Dictionary<K, V>` is a built-in type, not something a program can declare"),
                );
            }
        }
    }
}
