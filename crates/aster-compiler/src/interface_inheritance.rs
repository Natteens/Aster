//! Interface inheritance (LANG-012): `interface IB : IA, IC { ... }`.
//!
//! The rewrite runs after monomorphization and before semantic analysis, so a
//! generic base such as `IA<int>` is already a plain interface name. It keeps
//! every later layer unaware of the feature:
//!
//! - an interface receives, as its own members, every member of its base
//!   interfaces (transitively). Members with the same name and exact parameter
//!   and return types are the same member (C# semantics for a diamond); the
//!   same name and parameters with another return type is an error;
//! - the `:` list of an interface is replaced by its transitive closure of
//!   base interfaces, which semantic analysis uses for the implicit
//!   `IB -> IA` conversion;
//! - a class that lists an interface also lists each of its transitive base
//!   interfaces, so the class has a method table for every one of them and its
//!   descriptor lists them all (the run-time interface search finds `IA` for a
//!   class that names only `IB` in source). Interfaces the base class chain
//!   already implements are not repeated.

use std::collections::{HashMap, HashSet};

use aster_diagnostics::Diagnostic;
use aster_syntax::{Item, Member, Module, TypeDeclaration, TypeRef};

/// Rewrite `module` in place. Returns the diagnostics of invalid interface
/// heritage (unknown or non-interface base, repeated base, cycle, conflicting
/// inherited members).
pub(crate) fn expand(module: &mut Module) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let interfaces = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Interface(declaration) => Some((declaration.name.clone(), declaration.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let kinds = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Class(declaration) | Item::Struct(declaration) | Item::Interface(declaration) => {
                Some((declaration.name.clone(), matches!(item, Item::Interface(_))))
            }
            Item::Enum(declaration) => Some((declaration.name.clone(), false)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    let mut closures: HashMap<String, Vec<TypeRef>> = HashMap::new();
    let mut cyclic = HashSet::new();
    let mut ordered_names = Vec::new();
    for item in &module.items {
        if let Item::Interface(declaration) = item {
            ordered_names.push(declaration.name.clone());
        }
    }
    for item in &module.items {
        let Item::Interface(declaration) = item else {
            continue;
        };
        validate_bases(declaration, &kinds, &mut diagnostics);
    }
    for name in &ordered_names {
        let mut stack = Vec::new();
        close(
            name,
            &interfaces,
            &mut closures,
            &mut cyclic,
            &mut stack,
            &mut diagnostics,
        );
    }

    // Interface members: own members, then each inherited member once.
    let mut flattened: HashMap<String, Vec<Member>> = HashMap::new();
    for name in &ordered_names {
        if cyclic.contains(name) {
            continue;
        }
        let declaration = &interfaces[name];
        let mut members = declaration.members.clone();
        for base in closures.get(name).into_iter().flatten() {
            let Some(base_declaration) = interfaces.get(&base.name) else {
                continue;
            };
            for member in &base_declaration.members {
                add_inherited(
                    name,
                    base,
                    member,
                    &mut members,
                    &mut diagnostics,
                    declaration,
                );
            }
        }
        flattened.insert(name.clone(), members);
    }

    for item in &mut module.items {
        match item {
            Item::Interface(declaration) => {
                if let Some(members) = flattened.remove(&declaration.name) {
                    declaration.members = members;
                }
                if let Some(closure) = closures.get(&declaration.name) {
                    declaration.interfaces.clone_from(closure);
                }
            }
            Item::Class(_)
            | Item::Struct(_)
            | Item::Enum(_)
            | Item::Function(_)
            | Item::Variable(_) => {}
        }
    }

    expand_classes(module, &closures, &kinds);
    diagnostics
}

fn validate_bases(
    declaration: &TypeDeclaration,
    kinds: &HashMap<String, bool>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut seen = HashSet::new();
    for base in &declaration.interfaces {
        if !seen.insert(base.name.as_str()) {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "interface `{}` is listed more than once as a base of `{}`",
                        base.name, declaration.name
                    ),
                    base.span,
                )
                .with_help("remove the duplicate base interface"),
            );
            continue;
        }
        match kinds.get(&base.name) {
            None => diagnostics.push(
                Diagnostic::error(format!("unknown interface `{}`", base.name), base.span)
                    .with_help("declare the interface or add a using for its namespace"),
            ),
            Some(false) => diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`{}` is not an interface and cannot be a base of interface `{}`",
                        base.name, declaration.name
                    ),
                    base.span,
                )
                .with_help("an interface can only extend other interfaces"),
            ),
            Some(true) => {}
        }
    }
}

/// Transitive base interfaces of `name`, nearest first and without repeats,
/// memoized in `closures`. A cycle reports one error and marks every interface
/// of the cycle (its closure is left empty so the rewrite still terminates).
fn close(
    name: &str,
    interfaces: &HashMap<String, TypeDeclaration>,
    closures: &mut HashMap<String, Vec<TypeRef>>,
    cyclic: &mut HashSet<String>,
    stack: &mut Vec<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if closures.contains_key(name) {
        return;
    }
    let Some(declaration) = interfaces.get(name) else {
        return;
    };
    if let Some(index) = stack.iter().position(|entry| entry == name) {
        let mut path = stack[index..].to_vec();
        path.push(name.to_owned());
        diagnostics.push(
            Diagnostic::error(
                format!("interface inheritance cycle: {}", path.join(" -> ")),
                declaration.span,
            )
            .with_help("an interface cannot inherit from itself, directly or indirectly"),
        );
        cyclic.extend(stack[index..].iter().cloned());
        return;
    }
    stack.push(name.to_owned());
    let mut closure: Vec<TypeRef> = Vec::new();
    for base in &declaration.interfaces {
        if !interfaces.contains_key(&base.name) {
            continue;
        }
        close(&base.name, interfaces, closures, cyclic, stack, diagnostics);
        if cyclic.contains(&base.name) {
            continue;
        }
        if !closure.iter().any(|known| known.name == base.name) {
            closure.push(base.clone());
        }
        for inherited in closures.get(&base.name).into_iter().flatten() {
            if !closure.iter().any(|known| known.name == inherited.name) {
                closure.push(inherited.clone());
            }
        }
    }
    stack.pop();
    if cyclic.contains(name) {
        closures.insert(name.to_owned(), Vec::new());
    } else {
        closures.insert(name.to_owned(), closure);
    }
}

fn parameter_types(method: &aster_syntax::FunctionDeclaration) -> Vec<&str> {
    method
        .parameters
        .iter()
        .map(|parameter| parameter.type_ref.name.as_str())
        .collect()
}

/// Add `member` of `base` to `members` unless an identical member is already
/// present; a member that differs only by return type is a conflict.
fn add_inherited(
    owner: &str,
    base: &TypeRef,
    member: &Member,
    members: &mut Vec<Member>,
    diagnostics: &mut Vec<Diagnostic>,
    declaration: &TypeDeclaration,
) {
    match member {
        Member::Method(method) => {
            for existing in members.iter() {
                let Member::Method(existing) = existing else {
                    continue;
                };
                if existing.name == method.name
                    && parameter_types(existing) == parameter_types(method)
                {
                    if existing.return_type.name != method.return_type.name {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "interface `{owner}` inherits conflicting declarations of `{}` (returns `{}` and `{}`)",
                                    method.name, existing.return_type.name, method.return_type.name
                                ),
                                declaration.span,
                            )
                            .with_help(
                                "inherited members with the same name and parameters must have the same return type",
                            ),
                        );
                    }
                    return;
                }
            }
            members.push(member.clone());
        }
        Member::Property(property) => {
            for existing in members.iter() {
                let Member::Property(existing) = existing else {
                    continue;
                };
                if existing.name == property.name {
                    if existing.type_ref.name != property.type_ref.name {
                        diagnostics.push(Diagnostic::error(
                            format!(
                                "interface `{owner}` inherits conflicting declarations of `{}` from `{}`",
                                property.name, base.name
                            ),
                            declaration.span,
                        ));
                    }
                    return;
                }
            }
            members.push(member.clone());
        }
        // Interfaces cannot declare fields (reported by semantic analysis).
        Member::Field(_) => {}
    }
}

fn inherited(
    name: &str,
    bases: &HashMap<String, TypeDeclaration>,
    closures: &HashMap<String, Vec<TypeRef>>,
    kinds: &HashMap<String, bool>,
    visiting: &mut Vec<String>,
) -> HashSet<String> {
    let mut set = HashSet::new();
    let Some(class) = bases.get(name) else {
        return set;
    };
    if visiting.iter().any(|entry| entry == name) {
        return set;
    }
    visiting.push(name.to_owned());
    for entry in &class.interfaces {
        if bases.contains_key(&entry.name) {
            set.extend(inherited(&entry.name, bases, closures, kinds, visiting));
        } else if kinds.get(&entry.name) == Some(&true) {
            set.insert(entry.name.clone());
            for base in closures.get(&entry.name).into_iter().flatten() {
                set.insert(base.name.clone());
            }
        }
    }
    visiting.pop();
    set
}
fn expand_classes(
    module: &mut Module,
    closures: &HashMap<String, Vec<TypeRef>>,
    kinds: &HashMap<String, bool>,
) {
    // Interfaces implemented by each class (listed, expanded, and from its
    // base chain), computed along the base chain.
    let bases = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Class(class) => Some((class.name.clone(), class.clone())),
            _ => None,
        })
        .collect::<HashMap<_, _>>();
    let mut inherited_sets: HashMap<String, HashSet<String>> = HashMap::new();
    // Only the interfaces of the base chain are skipped; those the class lists
    // itself are kept as written.
    for (name, class) in &bases {
        let mut from_base = HashSet::new();
        for entry in &class.interfaces {
            if bases.contains_key(&entry.name) {
                let mut visiting = vec![name.clone()];
                from_base.extend(inherited(
                    &entry.name,
                    &bases,
                    closures,
                    kinds,
                    &mut visiting,
                ));
            }
        }
        inherited_sets.insert(name.clone(), from_base);
    }
    for item in &mut module.items {
        let Item::Class(class) = item else {
            continue;
        };
        let from_base = &inherited_sets[&class.name];
        let mut present = class
            .interfaces
            .iter()
            .map(|entry| entry.name.clone())
            .collect::<HashSet<_>>();
        let mut additions = Vec::new();
        for entry in &class.interfaces {
            for base in closures.get(&entry.name).into_iter().flatten() {
                if present.contains(&base.name) || from_base.contains(&base.name) {
                    continue;
                }
                present.insert(base.name.clone());
                additions.push(TypeRef::new(base.name.clone(), entry.span));
            }
        }
        class.interfaces.extend(additions);
    }
}
