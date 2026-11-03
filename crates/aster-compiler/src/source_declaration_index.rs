//! Deterministic, queryable index of source declarations.
//!
//! [`SourceDeclarationIndex`] maps the stable [`DeclarationKey`] of every
//! declaration in one module to its snapshot-local [`DeclarationLocator`]. The
//! key answers "which logical declaration is this?" and survives unrelated
//! edits; the locator answers "where is it in *this* module?" and does not.
//! The two are kept apart on purpose, and the locator never feeds identity.
//!
//! The index is built from the same traversal as `source_declaration_keys`
//! (`declaration_key::collect_source_declarations`), so there is one definition
//! of which declarations exist. It is structural only: it does not resolve
//! names, check types, pick overloads or replace the semantic `Context`, and
//! `DeclarationKey` equality means "same logical declaration", not "unchanged"
//! (a body, return type or modifier edit retains the key).
//!
//! Like keys, an index must be built from the linked, **pre-monomorphization**
//! module. Nothing in the compiler owns an index yet; a future in-memory
//! compiler session will.

#![allow(dead_code, reason = "the index is defined ahead of its first consumer")]

use std::{collections::HashMap, fmt};

use aster_syntax::{
    EnumCase, Field, FunctionDeclaration, Item, Member, Module, Parameter, Property,
};

use crate::declaration_key::{
    DeclarationKey, DeclarationLocator, KeyedDeclaration, collect_source_declarations,
};

/// The syntax node a [`DeclarationLocator`] designates in a specific module.
#[derive(Clone, Copy, Debug)]
pub(crate) enum ResolvedDeclaration<'m> {
    /// A top-level item, or the declaring item of a type or enum.
    Item(&'m Item),
    Field(&'m Field),
    /// A method or constructor.
    Method(&'m FunctionDeclaration),
    /// A property; also the location of its getter and setter.
    Property(&'m Property),
    EnumCase(&'m EnumCase),
    EnumCaseField(&'m Parameter),
}

impl DeclarationLocator {
    /// The node at this location in `module`, which must be the module the
    /// locator was collected from. `None` if the location does not exist there.
    pub(crate) fn resolve(self, module: &Module) -> Option<ResolvedDeclaration<'_>> {
        match self {
            Self::Item(item) => module.items.get(item).map(ResolvedDeclaration::Item),
            Self::Member { item, member } => {
                let (Item::Class(declaration)
                | Item::Struct(declaration)
                | Item::Interface(declaration)) = module.items.get(item)?
                else {
                    return None;
                };
                Some(match declaration.members.get(member)? {
                    Member::Field(field) => ResolvedDeclaration::Field(field),
                    Member::Method(method) => ResolvedDeclaration::Method(method),
                    Member::Property(property) => ResolvedDeclaration::Property(property),
                })
            }
            Self::EnumCase { item, case } => {
                let Item::Enum(declaration) = module.items.get(item)? else {
                    return None;
                };
                declaration
                    .cases
                    .get(case)
                    .map(ResolvedDeclaration::EnumCase)
            }
            Self::EnumCaseField { item, case, field } => {
                let Item::Enum(declaration) = module.items.get(item)? else {
                    return None;
                };
                declaration
                    .cases
                    .get(case)?
                    .fields
                    .get(field)
                    .map(ResolvedDeclaration::EnumCaseField)
            }
        }
    }
}

/// Why an index could not be built. Internal only: user-facing duplicate and
/// overload diagnostics remain the semantic compiler's job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SourceIndexError {
    /// Two declarations produced the same key. Neither was kept over the other.
    DuplicateKey {
        key: DeclarationKey,
        first: DeclarationLocator,
        second: DeclarationLocator,
    },
}

impl fmt::Display for SourceIndexError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateKey { key, first, second } => write!(
                formatter,
                "duplicate declaration key {key} at {first:?} and {second:?}"
            ),
        }
    }
}

/// Which logical declarations were added, removed or kept between two
/// snapshots. "Retained" means the identity exists in both; it says nothing
/// about whether the declaration's body, return type, modifiers, layout or
/// dependencies changed.
///
/// Ordering is deterministic and independent of hashing: `added` follows the
/// later snapshot's source traversal order; `removed` and `retained` follow the
/// earlier snapshot's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeclarationDelta {
    pub(crate) added: Vec<DeclarationKey>,
    pub(crate) removed: Vec<DeclarationKey>,
    pub(crate) retained: Vec<DeclarationKey>,
}

/// Stable key -> snapshot-local location for one module.
#[derive(Clone, Debug)]
pub(crate) struct SourceDeclarationIndex {
    /// Source traversal order; the single ordering authority.
    entries: Vec<KeyedDeclaration>,
    /// Key -> position in `entries`.
    positions: HashMap<DeclarationKey, usize>,
}

impl SourceDeclarationIndex {
    /// Index every declaration of `module` (linked, pre-monomorphization), or
    /// fail with the first duplicate key without keeping a partial index.
    #[allow(clippy::result_large_err, reason = "internal cold error path")]
    pub(crate) fn build(module: &Module) -> Result<Self, SourceIndexError> {
        let entries = collect_source_declarations(module);
        let mut positions: HashMap<DeclarationKey, usize> = HashMap::with_capacity(entries.len());
        for (position, entry) in entries.iter().enumerate() {
            if let Some(&first) = positions.get(&entry.key) {
                return Err(SourceIndexError::DuplicateKey {
                    key: entry.key.clone(),
                    first: entries[first].locator,
                    second: entry.locator,
                });
            }
            positions.insert(entry.key.clone(), position);
        }
        Ok(Self { entries, positions })
    }

    pub(crate) fn get(&self, key: &DeclarationKey) -> Option<&KeyedDeclaration> {
        self.positions
            .get(key)
            .map(|&position| &self.entries[position])
    }

    /// All entries in source traversal order.
    pub(crate) fn entries(&self) -> &[KeyedDeclaration] {
        &self.entries
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Compare this (earlier) snapshot with a `later` one by logical identity.
    pub(crate) fn compare(&self, later: &Self) -> DeclarationDelta {
        let mut removed = Vec::new();
        let mut retained = Vec::new();
        for entry in &self.entries {
            if later.positions.contains_key(&entry.key) {
                retained.push(entry.key.clone());
            } else {
                removed.push(entry.key.clone());
            }
        }
        let added = later
            .entries
            .iter()
            .filter(|entry| !self.positions.contains_key(&entry.key))
            .map(|entry| entry.key.clone())
            .collect();
        DeclarationDelta {
            added,
            removed,
            retained,
        }
    }
}

#[cfg(test)]
mod tests {
    use aster_syntax::{Item, Member, Module, lex, parse};

    use super::{ResolvedDeclaration, SourceDeclarationIndex, SourceIndexError};
    use crate::declaration_key::{DeclarationKey, DeclarationKind, DeclarationLocator};

    fn module(source: &str) -> Module {
        parse(lex(source).expect("lex")).expect("parse")
    }

    fn index(source: &str) -> (Module, SourceDeclarationIndex) {
        let module = module(source);
        let index = SourceDeclarationIndex::build(&module).expect("no duplicate keys");
        (module, index)
    }

    /// The single entry of `kind` named `name` (optionally under owner `owner`).
    fn key(
        index: &SourceDeclarationIndex,
        kind: DeclarationKind,
        owner: Option<&str>,
        name: &str,
    ) -> DeclarationKey {
        let found: Vec<_> = index
            .entries()
            .iter()
            .filter(|entry| {
                entry.key.kind() == kind
                    && entry.key.name() == name
                    && entry.key.owner().map(DeclarationKey::name) == owner
            })
            .collect();
        assert_eq!(found.len(), 1, "expected one {kind:?} `{name}`");
        found[0].key.clone()
    }

    fn function(index: &SourceDeclarationIndex, name: &str) -> DeclarationKey {
        key(index, DeclarationKind::Function, None, name)
    }

    fn locator(index: &SourceDeclarationIndex, key: &DeclarationKey) -> DeclarationLocator {
        index.get(key).expect("indexed").locator
    }

    // --- A: basic indexing ---------------------------------------------------

    #[test]
    fn every_top_level_declaration_resolves_to_itself() {
        let (module, index) = index(
            "public int Add(int a, int b) { return a + b; } \
             public int Sub(int a, int b) { return a - b; } \
             public class Counter { } public struct Point { } public interface IScore { int Value(); }",
        );
        for (kind, name) in [
            (DeclarationKind::Function, "Add"),
            (DeclarationKind::Function, "Sub"),
            (DeclarationKind::Class, "Counter"),
            (DeclarationKind::Struct, "Point"),
            (DeclarationKind::Interface, "IScore"),
        ] {
            let entry = index.get(&key(&index, kind, None, name)).expect("indexed");
            let Some(ResolvedDeclaration::Item(item)) = entry.locator.resolve(&module) else {
                panic!("{name} must resolve to a top-level item");
            };
            let resolved = match item {
                Item::Function(function) => function.name.as_str(),
                Item::Class(declaration)
                | Item::Struct(declaration)
                | Item::Interface(declaration) => declaration.name.as_str(),
                _ => panic!("unexpected item"),
            };
            assert_eq!(resolved, name);
        }
        assert_eq!(index.len(), index.entries().len());
    }

    // --- B: members ----------------------------------------------------------

    #[test]
    fn members_constructors_and_accessors_have_distinct_correct_locations() {
        let (module, index) = index(
            "public class C { public int x; public C(int x) {} public int M() { return 1; } \
             public int P { get { return 1; } set { } } }",
        );
        let field = key(&index, DeclarationKind::Field, Some("C"), "x");
        let constructor = key(&index, DeclarationKind::Constructor, Some("C"), "");
        let method = key(&index, DeclarationKind::Method, Some("C"), "M");
        let property = key(&index, DeclarationKind::Property, Some("C"), "P");
        let getter = key(&index, DeclarationKind::Getter, Some("C"), "P");
        let setter = key(&index, DeclarationKind::Setter, Some("C"), "P");

        let resolve = |key: &DeclarationKey| {
            index
                .get(key)
                .expect("indexed")
                .locator
                .resolve(&module)
                .expect("resolves")
        };
        assert!(matches!(resolve(&field), ResolvedDeclaration::Field(f) if f.name == "x"));
        assert!(matches!(resolve(&constructor), ResolvedDeclaration::Method(m) if m.constructor));
        assert!(
            matches!(resolve(&method), ResolvedDeclaration::Method(m) if m.name == "M" && !m.constructor)
        );
        for accessor in [&property, &getter, &setter] {
            assert!(matches!(resolve(accessor), ResolvedDeclaration::Property(p) if p.name == "P"));
        }
        // Distinct declarations get distinct keys; an accessor shares its property's member.
        let all = [&field, &constructor, &method, &property, &getter, &setter];
        for (position, left) in all.iter().enumerate() {
            for right in &all[position + 1..] {
                assert_ne!(left, right);
            }
        }
        assert_eq!(locator(&index, &property), locator(&index, &getter));
        assert_eq!(locator(&index, &property), locator(&index, &setter));
        assert_ne!(locator(&index, &field), locator(&index, &method));
    }

    // --- C: enums ------------------------------------------------------------

    #[test]
    fn enum_cases_and_payload_fields_are_indexed() {
        let (module, index) =
            index("public enum Shape { None, Circle(int radius), Rect(int width, int height), }");
        let shape = key(&index, DeclarationKind::Enum, None, "Shape");
        assert!(matches!(
            locator(&index, &shape).resolve(&module),
            Some(ResolvedDeclaration::Item(Item::Enum(e))) if e.name == "Shape"
        ));
        let rect = index
            .entries()
            .iter()
            .find(|entry| {
                entry.key.kind() == DeclarationKind::EnumCase && entry.key.name() == "Rect"
            })
            .expect("Rect case");
        assert!(matches!(
            rect.locator.resolve(&module),
            Some(ResolvedDeclaration::EnumCase(case)) if case.name == "Rect" && case.fields.len() == 2
        ));
        let height = index
            .entries()
            .iter()
            .find(|entry| {
                entry.key.kind() == DeclarationKind::EnumCaseField && entry.key.name() == "height"
            })
            .expect("height payload field");
        assert!(matches!(
            height.locator.resolve(&module),
            Some(ResolvedDeclaration::EnumCaseField(field)) if field.name == "height"
        ));
        assert_eq!(height.key.owner().map(DeclarationKey::name), Some("Rect"));
    }

    // --- D: overloads ----------------------------------------------------------

    #[test]
    fn overloads_coexist_with_distinct_keys() {
        let (_, index) = index(
            "public int Foo(int value) { return value; } public int Foo(string value) { return 1; }",
        );
        let foos: Vec<_> = index
            .entries()
            .iter()
            .filter(|entry| entry.key.name() == "Foo")
            .collect();
        assert_eq!(foos.len(), 2);
        assert_ne!(foos[0].key, foos[1].key);
        assert_ne!(foos[0].locator, foos[1].locator);
        assert!(index.get(&foos[0].key).is_some() && index.get(&foos[1].key).is_some());
    }

    // --- E-H: edits seen through the delta --------------------------------------

    #[test]
    fn unrelated_insertion_retains_the_key_and_only_moves_the_locator() {
        let (_, before) = index("public int Foo() { return 1; }");
        let (after_module, after) =
            index("public int Extra() { return 0; } public int Foo() { return 1; }");
        let foo = function(&before, "Foo");
        assert_eq!(foo, function(&after, "Foo"));
        assert_eq!(locator(&before, &foo), DeclarationLocator::Item(0));
        assert_eq!(locator(&after, &foo), DeclarationLocator::Item(1));
        assert!(matches!(
            locator(&after, &foo).resolve(&after_module),
            Some(ResolvedDeclaration::Item(Item::Function(f))) if f.name == "Foo"
        ));

        let delta = before.compare(&after);
        assert_eq!(delta.added, vec![function(&after, "Extra")]);
        assert_eq!(delta.removed, []);
        assert_eq!(delta.retained, vec![foo]);
    }

    #[test]
    fn body_only_edit_changes_nothing_in_the_delta() {
        let (_, before) = index("public int Add(int a, int b) { return a + b; }");
        let (_, after) = index("public int Add(int a, int b) { return a - b; }");
        let delta = before.compare(&after);
        assert!(delta.added.is_empty() && delta.removed.is_empty());
        assert_eq!(delta.retained, vec![function(&before, "Add")]);
    }

    #[test]
    fn rename_is_remove_plus_add() {
        let (_, before) = index("public int Foo() { return 1; }");
        let (_, after) = index("public int Bar() { return 1; }");
        let delta = before.compare(&after);
        assert_eq!(delta.removed, vec![function(&before, "Foo")]);
        assert_eq!(delta.added, vec![function(&after, "Bar")]);
        assert_eq!(delta.retained, []);
    }

    #[test]
    fn parameter_type_change_is_remove_plus_add() {
        let (_, before) = index("public int Foo(int a) { return a; }");
        let (_, after) = index("public int Foo(long a) { return 1; }");
        let delta = before.compare(&after);
        assert_eq!(delta.removed.len(), 1);
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.retained, []);
        assert_ne!(delta.removed[0], delta.added[0]);
    }

    // --- I: duplicates ----------------------------------------------------------

    #[test]
    fn duplicate_keys_are_a_precise_error_not_an_overwrite() {
        let module = module(
            "public int Foo(int a) { return a; } public int Other() { return 0; } public int Foo(int b) { return b; }",
        );
        let error = SourceDeclarationIndex::build(&module).expect_err("duplicate overload");
        let SourceIndexError::DuplicateKey { key, first, second } = error;
        assert_eq!(key.name(), "Foo");
        assert_eq!(first, DeclarationLocator::Item(0));
        assert_eq!(second, DeclarationLocator::Item(2));
        assert!(
            SourceIndexError::DuplicateKey { key, first, second }
                .to_string()
                .contains("Foo")
        );
    }

    #[test]
    fn duplicate_members_are_reported_too() {
        let module = module("public class C { public int x; public int x; }");
        assert!(matches!(
            SourceDeclarationIndex::build(&module),
            Err(SourceIndexError::DuplicateKey {
                first: DeclarationLocator::Member { member: 0, .. },
                second: DeclarationLocator::Member { member: 1, .. },
                ..
            })
        ));
    }

    // --- J: generic templates -----------------------------------------------------

    #[test]
    fn generic_templates_keep_their_identity_across_unrelated_earlier_edits() {
        const GENERIC: &str = "public class Box<T> { public T Value; public Box() {} public U Map<U>(U other) { return other; } } \
                               public T Identity<T>(T value) { return value; }";
        let (_, before) = index(GENERIC);
        let (_, after) = index(&format!(
            "public int Earlier() {{ return 0; }} public class Extra {{ }} {GENERIC}"
        ));
        let class = key(&before, DeclarationKind::Class, None, "Box");
        let map = key(&before, DeclarationKind::Method, Some("Box"), "Map");
        let identity = function(&before, "Identity");
        assert_eq!(
            (
                class.generic_arity(),
                map.generic_arity(),
                identity.generic_arity()
            ),
            (1, 1, 1)
        );
        for key in [&class, &map, &identity] {
            assert!(after.get(key).is_some(), "lost {key}");
            assert!(before.get(key).is_some());
        }
        let delta = before.compare(&after);
        assert_eq!(delta.removed, []);
        assert_eq!(delta.added.len(), 2);
        assert_eq!(delta.retained.len(), before.len());
    }

    // --- K: deterministic comparison ----------------------------------------------

    #[test]
    fn comparison_is_deterministic_and_ordered_by_snapshot_traversal() {
        const BEFORE: &str = "public int A() { return 1; } public int B() { return 2; } public int C() { return 3; } public int D() { return 4; }";
        const AFTER: &str = "public int Z() { return 0; } public int B() { return 2; } public int Y() { return 9; } public int D() { return 4; } public int X() { return 8; }";
        let first = {
            let (_, before) = index(BEFORE);
            let (_, after) = index(AFTER);
            before.compare(&after)
        };
        for _ in 0..50 {
            let (_, before) = index(BEFORE);
            let (_, after) = index(AFTER);
            assert_eq!(before.compare(&after), first);
        }
        let names = |keys: &[DeclarationKey]| {
            keys.iter()
                .map(|key| key.name().to_owned())
                .collect::<Vec<_>>()
        };
        // Added: later snapshot's order. Removed and retained: earlier snapshot's order.
        assert_eq!(names(&first.added), ["Z", "Y", "X"]);
        assert_eq!(names(&first.removed), ["A", "C"]);
        assert_eq!(names(&first.retained), ["B", "D"]);
    }

    // --- the index does not mutate or alias its input --------------------------------

    #[test]
    fn building_an_index_does_not_modify_the_module() {
        let module = module("public class C { public int x; } public int F() { return 1; }");
        let snapshot = module.clone();
        let _ = SourceDeclarationIndex::build(&module).expect("index");
        assert_eq!(module, snapshot);
        let Item::Class(class) = &module.items[0] else {
            panic!("class")
        };
        assert!(matches!(class.members[0], Member::Field(_)));
    }

    // --- project linking integration ---------------------------------------------------

    use std::sync::atomic::{AtomicU64, Ordering};

    /// Compile a two-package workspace and index its linked, pre-monomorphization module.
    fn linked_index(
        label: &str,
        app: &str,
        lib_a: &str,
        lib_b: &str,
    ) -> (Module, SourceDeclarationIndex) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aster-source-index-{label}-{}-{id}",
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
        write("app/main.aster", app);
        write("lib/lib/A.aster", lib_a);
        write("lib/lib/B.aster", lib_b);
        let main = root.join("app/main.aster");
        crate::compile_project(&main).expect("workspace compiles");
        let linked = crate::project::linked_module_for_test(&main).expect("linked module captured");
        let _ = std::fs::remove_dir_all(&root);
        let index = SourceDeclarationIndex::build(&linked).expect("unique linked keys");
        (linked, index)
    }

    const APP: &str = "using lib; public int Value() { return 1; } public int Run() { return Value() + Shared(); }";
    const LIB_A: &str =
        "public int Value() { return 2; } public T Pick<T>(T item) { return item; }";
    const LIB_B: &str =
        "public int Shared() { return 3; } public class Holder<T> { public T Item; }";

    #[test]
    fn linked_project_declarations_are_package_qualified_and_templates_are_still_visible() {
        let (module, index) = linked_index("qualified", APP, LIB_A, LIB_B);
        let values: Vec<_> = index
            .entries()
            .iter()
            .filter(|entry| {
                entry.key.kind() == DeclarationKind::Function && entry.key.name().ends_with("Value")
            })
            .collect();
        assert_eq!(
            values.len(),
            2,
            "root and dependency `Value` must not collide"
        );
        assert_ne!(values[0].key, values[1].key);
        // Generic templates are present because the module is pre-monomorphization.
        assert!(
            index
                .entries()
                .iter()
                .any(|entry| entry.key.name().ends_with("Pick") && entry.key.generic_arity() == 1)
        );
        assert!(
            index
                .entries()
                .iter()
                .any(|entry| entry.key.kind() == DeclarationKind::Class
                    && entry.key.name().ends_with("Holder")
                    && entry.key.generic_arity() == 1)
        );
        assert!(
            index
                .entries()
                .iter()
                .all(|entry| !entry.key.name().contains('#')),
            "no generated declarations"
        );
        for entry in index.entries() {
            assert!(entry.locator.resolve(&module).is_some());
        }
    }

    #[test]
    fn linked_keys_survive_an_unrelated_edit_in_another_unit_while_locators_move() {
        let (_, before) = linked_index("before", APP, LIB_A, LIB_B);
        let (_, after) = linked_index(
            "after",
            APP,
            LIB_A,
            &format!("public int Unrelated() {{ return 9; }} {LIB_B}"),
        );
        let delta = before.compare(&after);
        assert_eq!(delta.removed, []);
        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.retained.len(), before.len());
        // A declaration that sits after the inserted one moved, yet kept its identity.
        let shared = before
            .entries()
            .iter()
            .find(|entry| entry.key.name().ends_with("Shared"))
            .expect("Shared");
        let moved = after.get(&shared.key).expect("Shared retained");
        assert_ne!(shared.locator, moved.locator);
    }
}
