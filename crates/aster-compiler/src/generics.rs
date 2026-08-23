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
    arity: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MethodTemplateKey {
    owner: String,
    declaration_start: usize,
    parameters: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct MethodSpecializationKey {
    template: MethodTemplateKey,
    arguments: Vec<String>,
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
    method_signatures: HashMap<(String, String), Vec<Vec<String>>>,
    method_templates: HashMap<(String, String), Vec<FunctionDeclaration>>,
    method_cache: HashMap<MethodSpecializationKey, String>,
    method_active: Vec<MethodSpecializationKey>,
    generated_methods: HashMap<String, Vec<FunctionDeclaration>>,
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
    constants: HashMap<String, crate::constexpr::ConstValue>,
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
        let parameter_names = parameters
            .iter()
            .map(|parameter| parameter.name.clone())
            .collect::<Vec<_>>();
        let substitutions = substitutions(&parameter_names, concrete);
        for (parameter, argument) in parameters.iter().zip(concrete) {
            for constraint in &parameter.constraints {
                let substituted = substitute_name(&constraint.name, &substitutions);
                let required = self.concretize_type_name(&substituted, span);
                if self.satisfies(argument, &required) {
                    continue;
                }
                satisfied = false;
                self.diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "type argument `{argument}` does not satisfy constraint `{}: {}`",
                            parameter.name, required
                        ),
                        span,
                    )
                    .with_help(format!(
                        "pass a class that implements `{required}`, or the interface itself"
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
            if let Item::Class(declaration)
            | Item::Struct(declaration)
            | Item::Interface(declaration) = item
            {
                self.extract_generic_methods(declaration);
            }
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
        self.attach_generated_methods(module);
        module.items.append(&mut self.generated_types);
        module
            .items
            .extend(self.generated.drain(..).map(Item::Function));
        self.diagnostics
    }

    fn extract_generic_methods(&mut self, declaration: &mut TypeDeclaration) {
        let owner = declaration.name.clone();
        for member in &declaration.members {
            let Member::Method(method) = member else {
                continue;
            };
            if method.type_parameters.is_empty() {
                continue;
            }
            let templates = self
                .method_templates
                .entry((owner.clone(), method.name.clone()))
                .or_default();
            if !templates.iter().any(|existing| {
                existing.span.start == method.span.start
                    && existing
                        .parameters
                        .iter()
                        .map(|parameter| &parameter.type_ref.name)
                        .eq(method
                            .parameters
                            .iter()
                            .map(|parameter| &parameter.type_ref.name))
            }) {
                templates.push(method.clone());
            }
        }
        declaration.members.retain(|member| {
            !matches!(member, Member::Method(method) if !method.type_parameters.is_empty())
        });
    }

    fn attach_generated_methods(&mut self, module: &mut Module) {
        for item in module.items.iter_mut().chain(&mut self.generated_types) {
            let (Item::Class(declaration) | Item::Struct(declaration)) = item else {
                continue;
            };
            if let Some(mut methods) = self.generated_methods.remove(&declaration.name) {
                declaration
                    .members
                    .extend(methods.drain(..).map(Member::Method));
            }
        }
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
