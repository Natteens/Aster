use std::{collections::HashMap, fmt::Write};

use aster_diagnostics::Diagnostic;
use aster_syntax::{
    BinaryOperator, Block, EnumDeclaration, Expression, ExpressionKind, FunctionDeclaration,
    InterpolatedPart, Item, Literal, Member, Module, Statement, SwitchCase, TypeDeclaration,
    TypeRef, VariableDeclaration, VariableKind,
    visit::{AstVisitorMut, walk_expression_mut, walk_switch_case_mut},
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
    diagnostics: Vec<Diagnostic>,
}

impl Monomorphizer {
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
        }
    }
}
