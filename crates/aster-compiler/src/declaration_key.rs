//! Stable declaration identity.
//!
//! A [`DeclarationKey`] answers "is this the same logical declaration after an
//! unrelated edit or a recompilation?". It is deliberately separate from the
//! dense `hir::SymbolId` / `mir::SymbolId`: those are `u32` counters handed out
//! by one whole-module HIR lowering pass, so they are only meaningful inside a
//! single compiled snapshot and shift whenever an earlier declaration is added,
//! removed or reordered. Keys are structural and never depend on them.
//!
//! Identity components: the declaration's linked name (the existing
//! package/namespace-qualified naming authority from project linking, or the
//! bare name for a single source), the owning declaration, the declaration
//! kind, the member name, generic arity, and, for callables, the ordered
//! parameter types (the compiler's own overload identity; the return type
//! alone cannot distinguish overloads, so it is an attribute, not identity).
//!
//! Deliberately excluded: source path, byte offsets and spans, traversal or
//! unit-load order, dense symbol ids, parameter names, return type, modifiers
//! such as visibility, `static` and `async`, and generic parameter spellings
//! (references to a type parameter are keyed by position).
//!
//! # Phase contract
//!
//! Source declaration keys are computed from the *linked, pre-monomorphization*
//! AST: the module `project::link` produces (type names already canonical,
//! generic templates still present) before `compile_module` runs
//! `generics::monomorphize`. Three identities exist, one per phase:
//!
//! - [`DeclarationKey`]: stable identity of a source declaration, including a
//!   generic template.
//! - [`SpecializationKey`]: stable identity of a concrete specialization,
//!   derived from the template's key and the ordered concrete type arguments,
//!   never from generated names.
//! - `SymbolId`: dense, ephemeral identity inside one compiled snapshot.
//!
//! Do not pass a monomorphized module (such as `Compilation::module`) to
//! [`source_declaration_keys`]: the monomorphizer replaces templates with
//! generated declarations, and generic-method specializations are named from
//! the template's source offset, so keys of generated items move with
//! unrelated edits. A uniqueness check over such output is possible but proves
//! nothing about cross-edit stability.
//!
//! Nothing in the compiler consumes keys yet: incremental compilation will be
//! their consumer, and this module only defines and tests the identity model.

#![allow(dead_code, reason = "keys are defined ahead of their first consumer")]

use std::fmt;

use aster_syntax::{
    EnumDeclaration, FunctionDeclaration, Item, Member, Module, Parameter, TypeDeclaration,
    TypeParameter, VariableDeclaration,
};

use crate::type_names::TypeName;

/// Which declaration a [`DeclarationKey`] names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum DeclarationKind {
    Function,
    Variable,
    Class,
    Struct,
    Interface,
    Enum,
    EnumCase,
    EnumCaseField,
    Field,
    Method,
    Constructor,
    Property,
    Getter,
    Setter,
}

/// Which generic parameter list a type parameter reference belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum GenericScope {
    /// The containing type's parameters.
    Owner,
    /// The declaration's own parameters.
    Own,
}

/// Canonical type identity used inside callable signatures.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum KeyType {
    /// Primitive or nominal type, possibly generic and/or an array.
    Named {
        base: String,
        arguments: Vec<KeyType>,
        array_depth: usize,
    },
    /// A reference to a generic parameter, by position so renaming the
    /// parameter does not change identity.
    GenericParameter {
        scope: GenericScope,
        index: usize,
        array_depth: usize,
    },
}

impl KeyType {
    fn from_name(name: &TypeName, owner: &[&str], own: &[&str]) -> Self {
        let position = |names: &[&str]| names.iter().position(|candidate| *candidate == name.base);
        if name.arguments.is_empty() {
            if let Some(index) = position(own) {
                return Self::GenericParameter {
                    scope: GenericScope::Own,
                    index,
                    array_depth: name.array_depth,
                };
            }
            if let Some(index) = position(owner) {
                return Self::GenericParameter {
                    scope: GenericScope::Owner,
                    index,
                    array_depth: name.array_depth,
                };
            }
        }
        Self::Named {
            base: name.base.clone(),
            arguments: name
                .arguments
                .iter()
                .map(|argument| Self::from_name(argument, owner, own))
                .collect(),
            array_depth: name.array_depth,
        }
    }

    fn from_spelling(spelling: &str, owner: &[&str], own: &[&str]) -> Self {
        TypeName::parse(spelling).map_or_else(
            || Self::Named {
                base: spelling.to_owned(),
                arguments: Vec::new(),
                array_depth: 0,
            },
            |name| Self::from_name(&name, owner, own),
        )
    }
}

impl fmt::Display for KeyType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let array_depth = match self {
            Self::Named {
                base,
                arguments,
                array_depth,
            } => {
                formatter.write_str(base)?;
                if !arguments.is_empty() {
                    formatter.write_str("<")?;
                    for (index, argument) in arguments.iter().enumerate() {
                        if index != 0 {
                            formatter.write_str(",")?;
                        }
                        write!(formatter, "{argument}")?;
                    }
                    formatter.write_str(">")?;
                }
                *array_depth
            }
            Self::GenericParameter {
                scope,
                index,
                array_depth,
            } => {
                let marker = match scope {
                    GenericScope::Owner => "O",
                    GenericScope::Own => "G",
                };
                write!(formatter, "#{marker}{index}")?;
                *array_depth
            }
        };
        for _ in 0..array_depth {
            formatter.write_str("[]")?;
        }
        Ok(())
    }
}

/// Stable logical identity of one source declaration.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct DeclarationKey {
    owner: Option<Box<DeclarationKey>>,
    kind: DeclarationKind,
    name: String,
    generic_arity: usize,
    parameters: Option<Vec<KeyType>>,
}

impl DeclarationKey {
    fn new(
        owner: Option<&DeclarationKey>,
        kind: DeclarationKind,
        name: &str,
        generic_arity: usize,
        parameters: Option<Vec<KeyType>>,
    ) -> Self {
        Self {
            owner: owner.map(|owner| Box::new(owner.clone())),
            kind,
            name: name.to_owned(),
            generic_arity,
            parameters,
        }
    }

    pub(crate) fn kind(&self) -> DeclarationKind {
        self.kind
    }

    /// The declaration's own name: the linked name for a top-level
    /// declaration, the member name otherwise (empty for constructors).
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// The containing declaration, when this is a member.
    pub(crate) fn owner(&self) -> Option<&DeclarationKey> {
        self.owner.as_deref()
    }

    pub(crate) fn generic_arity(&self) -> usize {
        self.generic_arity
    }

    /// Ordered parameter types for callables.
    pub(crate) fn parameters(&self) -> Option<&[KeyType]> {
        self.parameters.as_deref()
    }
}

/// Deterministic debugging representation; the structure above, not this
/// string, is the identity.
impl fmt::Display for DeclarationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(owner) = &self.owner {
            write!(formatter, "{owner}/")?;
        }
        write!(formatter, "{:?}:{}", self.kind, self.name)?;
        if self.generic_arity != 0 {
            write!(formatter, "<{}>", self.generic_arity)?;
        }
        if let Some(parameters) = &self.parameters {
            formatter.write_str("(")?;
            for (index, parameter) in parameters.iter().enumerate() {
                if index != 0 {
                    formatter.write_str(",")?;
                }
                write!(formatter, "{parameter}")?;
            }
            formatter.write_str(")")?;
        }
        Ok(())
    }
}

/// A generic template instantiated with ordered concrete type arguments.
/// Prepared for future specialization identity; no specialization cache uses
/// it yet.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SpecializationKey {
    template: DeclarationKey,
    arguments: Vec<KeyType>,
}

impl SpecializationKey {
    /// `None` when an argument is not a well-formed type spelling.
    pub(crate) fn new(template: DeclarationKey, arguments: &[&str]) -> Option<Self> {
        let arguments = arguments
            .iter()
            .map(|argument| {
                TypeName::parse(argument).map(|name| KeyType::from_name(&name, &[], &[]))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            template,
            arguments,
        })
    }

    pub(crate) fn template(&self) -> &DeclarationKey {
        &self.template
    }

    pub(crate) fn arguments(&self) -> &[KeyType] {
        &self.arguments
    }
}

/// Source identities of every declaration in `module` (including members,
/// accessors, enum cases and their payload fields), in source order. The
/// result is a set of identities: callers must not rely on the order.
///
/// `module` must be the linked, **pre-monomorphization** module (see the
/// module docs). Generic templates yield one key each (with their arity);
/// their specializations are identified by [`SpecializationKey`], not by
/// keys of generated items.
pub(crate) fn source_declaration_keys(module: &Module) -> Vec<DeclarationKey> {
    let mut keys = Vec::new();
    for item in &module.items {
        source_item_keys(item, &mut keys);
    }
    keys
}

pub(crate) fn source_item_keys(item: &Item, keys: &mut Vec<DeclarationKey>) {
    match item {
        Item::Function(function) => {
            keys.push(callable_key(None, function, &[], DeclarationKind::Function));
        }
        Item::Variable(variable) => keys.push(variable_key(variable)),
        Item::Class(declaration) => type_keys(declaration, DeclarationKind::Class, keys),
        Item::Struct(declaration) => type_keys(declaration, DeclarationKind::Struct, keys),
        Item::Interface(declaration) => type_keys(declaration, DeclarationKind::Interface, keys),
        Item::Enum(declaration) => enum_keys(declaration, keys),
    }
}

fn parameter_names(parameters: &[TypeParameter]) -> Vec<&str> {
    parameters
        .iter()
        .map(|parameter| parameter.name.as_str())
        .collect()
}

fn signature(parameters: &[Parameter], owner: &[&str], own: &[&str]) -> Vec<KeyType> {
    parameters
        .iter()
        .map(|parameter| KeyType::from_spelling(&parameter.type_ref.name, owner, own))
        .collect()
}

fn callable_key(
    owner: Option<&DeclarationKey>,
    function: &FunctionDeclaration,
    owner_parameters: &[&str],
    kind: DeclarationKind,
) -> DeclarationKey {
    let own = parameter_names(&function.type_parameters);
    let kind = if function.constructor {
        DeclarationKind::Constructor
    } else {
        kind
    };
    // A constructor is identified by its owner, not by how the type is spelled.
    let name = if function.constructor {
        ""
    } else {
        &function.name
    };
    DeclarationKey::new(
        owner,
        kind,
        name,
        function.type_parameters.len(),
        Some(signature(&function.parameters, owner_parameters, &own)),
    )
}

fn variable_key(variable: &VariableDeclaration) -> DeclarationKey {
    DeclarationKey::new(None, DeclarationKind::Variable, &variable.name, 0, None)
}

fn type_keys(declaration: &TypeDeclaration, kind: DeclarationKind, keys: &mut Vec<DeclarationKey>) {
    let owner = DeclarationKey::new(
        None,
        kind,
        &declaration.name,
        declaration.type_parameters.len(),
        None,
    );
    let owner_parameters = parameter_names(&declaration.type_parameters);
    keys.push(owner.clone());
    for member in &declaration.members {
        match member {
            Member::Field(field) => keys.push(DeclarationKey::new(
                Some(&owner),
                DeclarationKind::Field,
                &field.name,
                0,
                None,
            )),
            Member::Method(method) => keys.push(callable_key(
                Some(&owner),
                method,
                &owner_parameters,
                DeclarationKind::Method,
            )),
            Member::Property(property) => {
                keys.push(DeclarationKey::new(
                    Some(&owner),
                    DeclarationKind::Property,
                    &property.name,
                    0,
                    None,
                ));
                if property.getter.is_some() {
                    keys.push(DeclarationKey::new(
                        Some(&owner),
                        DeclarationKind::Getter,
                        &property.name,
                        0,
                        None,
                    ));
                }
                if property.setter.is_some() {
                    keys.push(DeclarationKey::new(
                        Some(&owner),
                        DeclarationKind::Setter,
                        &property.name,
                        0,
                        None,
                    ));
                }
            }
        }
    }
}

fn enum_keys(declaration: &EnumDeclaration, keys: &mut Vec<DeclarationKey>) {
    let owner = DeclarationKey::new(
        None,
        DeclarationKind::Enum,
        &declaration.name,
        declaration.type_parameters.len(),
        None,
    );
    keys.push(owner.clone());
    for case in &declaration.cases {
        let case_key =
            DeclarationKey::new(Some(&owner), DeclarationKind::EnumCase, &case.name, 0, None);
        keys.push(case_key.clone());
        for field in &case.fields {
            keys.push(DeclarationKey::new(
                Some(&case_key),
                DeclarationKind::EnumCaseField,
                &field.name,
                0,
                None,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use aster_syntax::{Module, lex, parse};

    use super::{DeclarationKey, DeclarationKind, source_declaration_keys};

    fn module(source: &str) -> Module {
        parse(lex(source).expect("lex")).expect("parse")
    }

    fn keys(source: &str) -> Vec<DeclarationKey> {
        source_declaration_keys(&module(source))
    }

    /// The single key of `kind` named `name` (optionally under `owner`).
    fn find(
        keys: &[DeclarationKey],
        kind: DeclarationKind,
        owner: Option<&str>,
        name: &str,
    ) -> DeclarationKey {
        let matches: Vec<_> = keys
            .iter()
            .filter(|key| {
                key.kind() == kind
                    && key.name() == name
                    && key.owner().map(DeclarationKey::name) == owner
            })
            .collect();
        assert_eq!(matches.len(), 1, "expected exactly one {kind:?} `{name}`");
        matches[0].clone()
    }

    fn function(keys: &[DeclarationKey], name: &str) -> DeclarationKey {
        find(keys, DeclarationKind::Function, None, name)
    }

    fn method(keys: &[DeclarationKey], owner: &str, name: &str) -> DeclarationKey {
        find(keys, DeclarationKind::Method, Some(owner), name)
    }

    // --- stability -------------------------------------------------------

    #[test]
    fn body_edit_keeps_the_key() {
        let before = keys("public int Foo() { return 1; }");
        let after = keys("public int Foo() { int value = 1; return value; }");
        assert_eq!(function(&before, "Foo"), function(&after, "Foo"));
    }

    #[test]
    fn unrelated_declaration_insertion_keeps_the_key() {
        let before = keys("public int Foo() { return 1; }");
        let after = keys(
            "public int Other() { return 2; } public class Extra { } public int Foo() { return 1; }",
        );
        assert_eq!(function(&before, "Foo"), function(&after, "Foo"));
    }

    #[test]
    fn unrelated_method_insertion_keeps_the_key() {
        let before = keys("public class A { public int M1() { return 1; } }");
        let after =
            keys("public class A { public int M0() { return 0; } public int M1() { return 1; } }");
        assert_eq!(method(&before, "A", "M1"), method(&after, "A", "M1"));
    }

    #[test]
    fn source_position_movement_keeps_the_key() {
        let before = keys("public int Foo() { return 1; }");
        let after =
            keys("// a comment\n\n\n    // more\n   public   int   Foo( )\n{\n return 1 ;\n}");
        assert_eq!(function(&before, "Foo"), function(&after, "Foo"));
    }

    #[test]
    fn using_reorder_keeps_unrelated_keys() {
        let before = keys("using a; using b; public int Foo() { return 1; }");
        let after = keys("using b; using a; public int Foo() { return 1; }");
        assert_eq!(function(&before, "Foo"), function(&after, "Foo"));
    }

    #[test]
    fn return_type_and_parameter_names_and_modifiers_do_not_define_identity() {
        let before = keys("public int Foo(int a) { return a; }");
        let after = keys("internal long Foo(int renamed) { return 1; }");
        assert_eq!(function(&before, "Foo"), function(&after, "Foo"));
    }

    // --- identity changes --------------------------------------------------

    #[test]
    fn rename_changes_the_key() {
        let foo = keys("public int Foo() { return 1; }");
        let bar = keys("public int Bar() { return 1; }");
        assert_ne!(function(&foo, "Foo"), function(&bar, "Bar"));
        assert!(!bar.contains(&function(&foo, "Foo")));
    }

    #[test]
    fn parameter_type_changes_the_key() {
        let int = keys("public int Foo(int a) { return a; }");
        let long = keys("public int Foo(long a) { return 1; }");
        assert_ne!(function(&int, "Foo"), function(&long, "Foo"));
    }

    #[test]
    fn overloads_get_distinct_keys_at_once() {
        let overloads = keys(
            "public int Foo(int a) { return a; } public int Foo(string a) { return 1; } public int Foo(int a, int b) { return a; }",
        );
        let foos: Vec<_> = overloads.iter().filter(|key| key.name() == "Foo").collect();
        assert_eq!(foos.len(), 3);
        for (index, left) in foos.iter().enumerate() {
            for right in &foos[index + 1..] {
                assert_ne!(left, right);
            }
        }
    }

    #[test]
    fn parameter_order_defines_overload_identity() {
        let one = keys("public int Foo(int a, string b) { return a; }");
        let two = keys("public int Foo(string a, int b) { return 1; }");
        assert_ne!(function(&one, "Foo"), function(&two, "Foo"));
    }

    #[test]
    fn owner_changes_the_key() {
        let source = keys(
            "public class A { public int M() { return 1; } } public class B { public int M() { return 1; } }",
        );
        assert_ne!(method(&source, "A", "M"), method(&source, "B", "M"));
    }

    #[test]
    fn kind_prevents_collisions_between_same_named_declarations() {
        let source = keys(
            "public class A { public int X; public int X2() { return 1; } public int Y { get { return 1; } } }",
        );
        let field = find(&source, DeclarationKind::Field, Some("A"), "X");
        let property = find(&source, DeclarationKind::Property, Some("A"), "Y");
        assert_ne!(field, property);
        let renamed = keys("public int A() { return 1; } public class B { public int A; }");
        assert_ne!(
            function(&renamed, "A"),
            find(&renamed, DeclarationKind::Field, Some("B"), "A")
        );
    }

    #[test]
    fn generic_arity_changes_the_key() {
        let one = keys("public T Foo<T>(T a) { return a; }");
        let two = keys("public T Foo<T, U>(T a) { return a; }");
        assert_ne!(function(&one, "Foo"), function(&two, "Foo"));
    }

    #[test]
    fn renaming_a_generic_parameter_keeps_the_key() {
        let t = keys("public T Foo<T>(T a, T[] items) { return a; }");
        let u = keys("public U Foo<U>(U a, U[] items) { return a; }");
        assert_eq!(function(&t, "Foo"), function(&u, "Foo"));
        let different = keys("public int Foo<U>(int a, U[] items) { return a; }");
        assert_ne!(function(&t, "Foo"), function(&different, "Foo"));
    }

    #[test]
    fn array_and_generic_argument_shapes_are_distinguished() {
        let scalar = keys("public int Foo(int a) { return a; }");
        let array = keys("public int Foo(int[] a) { return 1; }");
        let list = keys("public int Foo(List<int> a) { return 1; }");
        let list_long = keys("public int Foo(List<long> a) { return 1; }");
        let all = [
            function(&scalar, "Foo"),
            function(&array, "Foo"),
            function(&list, "Foo"),
            function(&list_long, "Foo"),
        ];
        for (index, left) in all.iter().enumerate() {
            for right in &all[index + 1..] {
                assert_ne!(left, right);
            }
        }
    }

    // --- declaration kinds ---------------------------------------------------

    #[test]
    fn every_declaration_category_receives_a_key() {
        let all = keys(
            "public int top = 1; \
             public int Fn(int a) { return a; } \
             public T Gen<T>(T a) { return a; } \
             public class C<T> { public int f; public C() {} public int M() { return 1; } public T G<U>(U a) { T v; return v; } public int P { get { return 1; } set { } } } \
             public struct S { public int x; } \
             public interface I { int Value(); } \
             public enum E { None, Some(int value), }",
        );
        let kinds: Vec<_> = all.iter().map(DeclarationKey::kind).collect();
        for expected in [
            DeclarationKind::Variable,
            DeclarationKind::Function,
            DeclarationKind::Class,
            DeclarationKind::Struct,
            DeclarationKind::Interface,
            DeclarationKind::Enum,
            DeclarationKind::Field,
            DeclarationKind::Constructor,
            DeclarationKind::Method,
            DeclarationKind::Property,
            DeclarationKind::Getter,
            DeclarationKind::Setter,
            DeclarationKind::EnumCase,
            DeclarationKind::EnumCaseField,
        ] {
            assert!(kinds.contains(&expected), "missing {expected:?}");
        }
        // Templates carry their generic arity; ordinary declarations have none.
        assert_eq!(function(&all, "Gen").generic_arity(), 1);
        assert_eq!(function(&all, "Fn").generic_arity(), 0);
        assert_eq!(
            find(&all, DeclarationKind::Class, None, "C").generic_arity(),
            1
        );
        assert_eq!(method(&all, "C", "G").generic_arity(), 1);
    }

    #[test]
    fn a_constructor_key_does_not_depend_on_the_type_name_spelling() {
        let source = keys("public class C { public C(int a) {} public C(string a) {} }");
        let constructors: Vec<_> = source
            .iter()
            .filter(|key| key.kind() == DeclarationKind::Constructor)
            .collect();
        assert_eq!(constructors.len(), 2);
        assert_ne!(constructors[0], constructors[1]);
    }

    // --- specialization preparation ---------------------------------------

    #[test]
    fn specialization_keys_are_template_plus_ordered_concrete_arguments() {
        use super::SpecializationKey;
        let template = keys("public T Identity<T>(T value) { return value; }");
        let template = function(&template, "Identity");
        let int = SpecializationKey::new(template.clone(), &["int"]).expect("int");
        let string = SpecializationKey::new(template.clone(), &["string"]).expect("string");
        assert_ne!(int, string);
        assert_eq!(
            int,
            SpecializationKey::new(template.clone(), &["int"]).expect("int")
        );
        let pair_a = SpecializationKey::new(template.clone(), &["int", "string"]).expect("pair");
        let pair_b = SpecializationKey::new(template, &["string", "int"]).expect("pair");
        assert_ne!(pair_a, pair_b);
    }

    /// Uniqueness only: this runs over post-monomorphization output, so it makes no
    /// claim about cross-edit stability (see the phase contract in the module docs).
    #[test]
    fn post_monomorphization_keys_are_unique_across_every_compilable_example_project() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        collect_sources(&repository.join("examples"), &mut files);
        let mut linked = 0;
        for file in files
            .iter()
            .filter(|file| file.file_name().is_some_and(|name| name == "main.aster"))
        {
            let Ok(compilation) = crate::compile_project(file) else {
                continue;
            };
            let all = source_declaration_keys(&compilation.compilation.module);
            let unique: std::collections::HashSet<_> = all.iter().collect();
            assert_eq!(
                unique.len(),
                all.len(),
                "duplicate post-monomorphization keys in {}",
                file.display()
            );
            linked += 1;
        }
        assert!(linked >= 5, "only {linked} example projects compiled");
    }

    // --- phase contract: source identity vs. monomorphized output -------------

    const TOOLS: &str = "public class Tools { public Tools() {} public T Identity<T>(T value) { return value; } } \
        public int Run() { Tools tools = new Tools(); int a = tools.Identity<int>(1); string s = tools.Identity<string>(\"x\"); return a + s.Length; }";
    const EARLIER: &str = "public int Earlier() { return 0; } ";

    /// Names of the generic-method specializations the monomorphizer generated.
    fn generated_method_names(source: &str) -> Vec<String> {
        let compilation = crate::compile(source).expect("valid generic method program");
        let mut names: Vec<String> = compilation
            .module
            .items
            .iter()
            .filter_map(|item| match item {
                aster_syntax::Item::Class(class) => Some(&class.members),
                _ => None,
            })
            .flatten()
            .filter_map(|member| match member {
                aster_syntax::Member::Method(method) if method.name.contains("#method#") => {
                    Some(method.name.clone())
                }
                _ => None,
            })
            .collect();
        names.sort();
        names
    }

    #[test]
    fn monomorphized_method_names_and_keys_move_with_unrelated_edits() {
        // Why `source_declaration_keys` must never see post-monomorphization output:
        // generated method names embed the template's source offset.
        let before = generated_method_names(TOOLS);
        let after = generated_method_names(&format!("{EARLIER}{TOOLS}"));
        assert_eq!(before.len(), 2, "{before:?}");
        assert_eq!(after.len(), 2, "{after:?}");
        assert_ne!(
            before, after,
            "generated names should shift with span.start"
        );

        let keys_of = |source: &str| {
            let module = crate::compile(source).expect("compiles").module;
            source_declaration_keys(&module)
        };
        let before_keys = keys_of(TOOLS);
        let after_keys = keys_of(&format!("{EARLIER}{TOOLS}"));
        let generated = |keys: &[DeclarationKey]| -> Vec<DeclarationKey> {
            keys.iter()
                .filter(|key| key.name().contains("#method#"))
                .cloned()
                .collect()
        };
        assert_ne!(
            generated(&before_keys),
            generated(&after_keys),
            "post-monomorphization keys are not stable identities"
        );
    }

    #[test]
    fn generic_method_template_and_specialization_keys_are_stable_before_monomorphization() {
        use super::SpecializationKey;
        let template_of = |source: &str| {
            let all = keys(source);
            find(&all, DeclarationKind::Method, Some("Tools"), "Identity")
        };
        let before = template_of(TOOLS);
        let after = template_of(&format!("{EARLIER}{TOOLS}"));
        assert_eq!(before, after);
        assert_eq!(before.generic_arity(), 1);

        let specialize = |template: &DeclarationKey, argument: &str| {
            SpecializationKey::new(template.clone(), &[argument]).expect("well-formed argument")
        };
        assert_eq!(specialize(&before, "int"), specialize(&after, "int"));
        assert_eq!(specialize(&before, "string"), specialize(&after, "string"));
        assert_ne!(specialize(&before, "int"), specialize(&before, "string"));
    }

    // --- packages and multi-file projects -------------------------------------

    /// Compile a two-package workspace and return the keys of the linked module.
    fn workspace_keys(label: &str, main: &str, library: &str) -> Vec<DeclarationKey> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aster-declaration-key-{label}-{}-{id}",
            std::process::id()
        ));
        let write = |relative: &str, contents: &str| {
            let path = root.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("directory");
            std::fs::write(path, contents).expect("write");
        };
        write(
            "app/Aster.toml",
            "[package]\nname = \"app\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n",
        );
        write("lib/Aster.toml", "[package]\nname = \"lib\"\n");
        write("app/main.aster", main);
        write("lib/lib/Lib.aster", library);
        let compilation = crate::compile_project(&root.join("app/main.aster"));
        let keys =
            source_declaration_keys(&compilation.expect("workspace compiles").compilation.module);
        // The stability assertions below are only valid because these fixtures have no
        // generics, so the monomorphized module equals the pre-monomorphization one.
        assert!(
            keys.iter().all(|key| !key.name().contains('#')),
            "fixture must not generate declarations"
        );
        let _ = std::fs::remove_dir_all(&root);
        keys
    }

    const APP: &str = "using lib; public int Value() { return 1; } public int Run() { return Value() + Shared(); }";

    #[test]
    fn same_name_in_different_packages_does_not_collide() {
        let keys = workspace_keys(
            "collide",
            APP,
            "public int Shared() { return 2; } public int Value() { return 3; }",
        );
        let values: Vec<_> = keys
            .iter()
            .filter(|key| key.name().ends_with("Value") && key.kind() == DeclarationKind::Function)
            .collect();
        assert_eq!(values.len(), 2, "{keys:?}");
        assert_ne!(values[0], values[1]);
        let unique: std::collections::HashSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn unrelated_dependency_edit_keeps_existing_keys_and_builds_are_deterministic() {
        let library = "public int Shared() { return 2; } public int Value() { return 3; }";
        let before = workspace_keys("before", APP, library);
        let again = workspace_keys("again", APP, library);
        assert_eq!(before, again, "identical sources must give identical keys");

        let edited = workspace_keys(
            "after",
            APP,
            "public int Unrelated() { return 9; } public int Shared() { return 2; } public int Value() { return 4; }",
        );
        for key in &before {
            assert!(edited.contains(key), "lost {key}");
        }
        assert_eq!(edited.len(), before.len() + 1);
    }

    // --- determinism ---------------------------------------------------------

    #[test]
    fn keys_are_independent_of_declaration_order() {
        let forward = keys(
            "public int A() { return 1; } public int B(int x) { return x; } public class C { public int M() { return 1; } }",
        );
        let reverse = keys(
            "public class C { public int M() { return 1; } } public int B(int x) { return x; } public int A() { return 1; }",
        );
        let mut left: Vec<String> = forward.iter().map(ToString::to_string).collect();
        let mut right: Vec<String> = reverse.iter().map(ToString::to_string).collect();
        left.sort();
        right.sort();
        assert_eq!(left, right);
        for key in &forward {
            assert!(reverse.contains(key));
        }
    }

    // --- uniqueness audit over every checked-in ASTER source -----------------

    fn collect_sources(directory: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                collect_sources(&path, found);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "aster")
            {
                found.push(path);
            }
        }
    }

    #[test]
    fn keys_are_unique_across_every_checked_in_source_file() {
        let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for directory in ["stdlib", "examples", "benchmarks"] {
            collect_sources(&repository.join(directory), &mut files);
        }
        assert!(
            files.len() > 40,
            "expected the repository sources, found {}",
            files.len()
        );
        let mut audited = 0;
        let mut declarations = 0;
        for file in files {
            let source = std::fs::read_to_string(&file).expect("read source");
            let Some(parsed) = lex(&source).ok().and_then(|tokens| parse(tokens).ok()) else {
                continue;
            };
            let all = source_declaration_keys(&parsed);
            let unique: std::collections::HashSet<_> = all.iter().collect();
            assert_eq!(
                unique.len(),
                all.len(),
                "duplicate declaration keys in {}",
                file.display()
            );
            audited += 1;
            declarations += all.len();
        }
        assert!(
            audited > 40 && declarations > 500,
            "audited {audited} files / {declarations} declarations"
        );
    }
}
