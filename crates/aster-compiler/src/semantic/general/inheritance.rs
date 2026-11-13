//! Single class inheritance (object model B1, pull request 1): heritage
//! validation, acyclicity, member inheritance and member access rules.
//!
//! Semantic analysis is the only layer that classifies the names after `:` and
//! decides what a derived class inherits. Inherited accessible members are
//! merged into the derived class's `TypeInfo` (fields, methods, properties), so
//! every lookup that already consults `TypeInfo` finds them without walking the
//! base chain; each merged member keeps its declaring class in its
//! `CallableKey` or `field_owner`, which is what HIR lowering and the access
//! rules use.

use super::{
    Analyzer, Argument, Binding, Callable, Context, Diagnostic, FunctionDeclaration, HashMap,
    HashSet, Item, Member, MemberDispatch, Model, Module, ResolvedBaseConstructor, Span, Type,
    TypeDeclaration, TypeInfo, TypeKind, Visibility, resolve_type_readonly,
};

pub(super) fn resolve(module: &Module, context: &mut Context, diagnostics: &mut Vec<Diagnostic>) {
    let classes = module
        .items
        .iter()
        .filter_map(|item| match item {
            Item::Class(class) => Some(class),
            _ => None,
        })
        .collect::<Vec<_>>();
    validate_heritage(&classes, context, diagnostics);
    break_cycles(&classes, context, diagnostics);
    let by_name = classes
        .iter()
        .map(|class| (class.name.as_str(), *class))
        .collect::<HashMap<_, _>>();
    let mut done = HashSet::new();
    for class in &classes {
        merge(&class.name, &by_name, context, &mut done, diagnostics);
    }
    validate_overrides(&classes, context, diagnostics);
}

/// Classify every name after `:` and report the invalid ones.
fn validate_heritage(
    classes: &[&TypeDeclaration],
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for class in classes {
        let mut seen_base = false;
        for (index, entry) in class.interfaces.iter().enumerate() {
            let Some(info) = context.types.get(&entry.name) else {
                // Unknown names are reported by the interface validation.
                continue;
            };
            match info.kind {
                Some(TypeKind::Class) => {
                    if info.is_static {
                        diagnostics.push(
                            Diagnostic::error(
                                format!("static class `{}` cannot be a base class", entry.name),
                                entry.span,
                            )
                            .with_help(
                                "a static class is never instantiated, so nothing derives from it",
                            ),
                        );
                    } else if info.is_sealed {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "class `{}` is sealed and cannot be derived from",
                                    entry.name
                                ),
                                entry.span,
                            )
                            .with_help(
                                "remove `sealed` from the base class or derive from another class",
                            ),
                        );
                    }
                    if seen_base {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "class `{}` cannot have a second base class `{}`",
                                    class.name, entry.name
                                ),
                                entry.span,
                            )
                            .with_help("a class has at most one base class; further names must be interfaces"),
                        );
                    } else if index > 0 {
                        diagnostics.push(
                            Diagnostic::error(
                                format!(
                                    "the base class `{}` must be listed before the interfaces",
                                    entry.name
                                ),
                                entry.span,
                            )
                            .with_help(
                                "write the base class first: `class D : Base, IFirst, ISecond`",
                            ),
                        );
                    }
                    seen_base = true;
                }
                Some(TypeKind::Struct | TypeKind::Enum) => diagnostics.push(
                    Diagnostic::error(
                        format!(
                            "`{}` is a {} and cannot be a base class",
                            entry.name,
                            if info.kind == Some(TypeKind::Struct) {
                                "struct"
                            } else {
                                "enum"
                            }
                        ),
                        entry.span,
                    )
                    .with_help(
                        "only a class can be a base; interfaces are implemented, not derived from",
                    ),
                ),
                Some(TypeKind::Interface) | None => {}
            }
        }
    }
}

/// Report inheritance cycles and cut them (the classes of a cycle lose their
/// base), so the merge below always terminates.
fn break_cycles(
    classes: &[&TypeDeclaration],
    context: &mut Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let spans = classes
        .iter()
        .map(|class| (class.name.as_str(), class.span))
        .collect::<HashMap<_, _>>();
    let mut done = HashSet::<String>::new();
    for class in classes {
        let mut chain: Vec<String> = Vec::new();
        let mut current = class.name.clone();
        loop {
            if let Some(position) = chain.iter().position(|name| *name == current) {
                let cycle = &chain[position..];
                let mut path = cycle.join(" -> ");
                path.push_str(" -> ");
                path.push_str(&current);
                diagnostics.push(
                    Diagnostic::error(
                        format!("cyclic inheritance: {path}"),
                        spans.get(current.as_str()).copied().unwrap_or(class.span),
                    )
                    .with_help("a class cannot be its own ancestor; remove one base declaration"),
                );
                for member in cycle {
                    if let Some(info) = context.types.get_mut(member) {
                        info.base = None;
                    }
                }
                break;
            }
            if done.contains(&current) {
                break;
            }
            chain.push(current.clone());
            let Some(base) = context
                .types
                .get(&current)
                .and_then(|info| info.base.clone())
            else {
                break;
            };
            current = base;
        }
        done.extend(chain);
    }
}

/// Merge the accessible members of the base into the derived class, bases
/// first.
#[allow(clippy::too_many_lines)]
fn merge(
    name: &str,
    classes: &HashMap<&str, &TypeDeclaration>,
    context: &mut Context,
    done: &mut HashSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !done.insert(name.to_owned()) {
        return;
    }
    let Some(base) = context.types.get(name).and_then(|info| info.base.clone()) else {
        return;
    };
    merge(&base, classes, context, done, diagnostics);
    let Some(base_info) = context.types.get(&base).cloned() else {
        return;
    };
    let Some(declaration) = classes.get(name) else {
        return;
    };
    let Some(info) = context.types.get_mut(name) else {
        return;
    };

    // A derived class may not list an interface its base already implements.
    for entry in &declaration.interfaces {
        if info.implemented_interfaces.contains(&entry.name)
            && base_info.implemented_interfaces.contains(&entry.name)
        {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "interface `{}` is already implemented by base class `{base}`",
                        entry.name
                    ),
                    entry.span,
                )
                .with_help(
                    "remove it from the list; the derived class inherits the implementation",
                ),
            );
        }
    }
    for interface in &base_info.implemented_interfaces {
        if !info.implemented_interfaces.contains(interface) {
            info.implemented_interfaces.push(interface.clone());
        }
    }
    info.ancestors = std::iter::once(base.clone())
        .chain(base_info.ancestors.iter().cloned())
        .collect();
    info.hidden_private.clone_from(&base_info.hidden_private);

    let conflict = |what: &str,
                    member: &str,
                    inherited: &str,
                    diagnostics: &mut Vec<Diagnostic>| {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "member `{member}` of `{name}` conflicts with the inherited {what} `{inherited}.{member}`"
                ),
                member_span(declaration, member),
            )
            .with_help("rename the member; hiding an inherited member is not supported"),
        );
    };

    for (field, type_) in &base_info.fields {
        let visibility = base_info
            .field_visibility
            .get(field)
            .copied()
            .unwrap_or(Visibility::Public);
        let declared_in = base_info
            .field_owner
            .get(field)
            .cloned()
            .unwrap_or_else(|| base.clone());
        if visibility == Visibility::Private {
            info.hidden_private.insert(field.clone(), declared_in);
            continue;
        }
        if info.fields.contains_key(field)
            || info.methods.contains_key(field)
            || info.properties.contains_key(field)
        {
            conflict("field", field, &declared_in, diagnostics);
            continue;
        }
        info.fields.insert(field.clone(), type_.clone());
        info.field_visibility.insert(field.clone(), visibility);
        info.field_owner.insert(field.clone(), declared_in);
    }
    let mut inherited_methods: Vec<(String, Callable)> = Vec::new();
    let mut method_names = base_info.methods.keys().cloned().collect::<Vec<_>>();
    method_names.sort();
    for method_name in method_names {
        for callable in &base_info.methods[&method_name] {
            if callable.visibility == Visibility::Private {
                info.hidden_private.insert(
                    method_name.clone(),
                    callable.key.owner.clone().unwrap_or_else(|| base.clone()),
                );
                continue;
            }
            inherited_methods.push((method_name.clone(), callable.clone()));
        }
    }
    for (method_name, callable) in inherited_methods {
        let declared_in = callable.key.owner.clone().unwrap_or_else(|| base.clone());
        if info.fields.contains_key(&method_name) || info.properties.contains_key(&method_name) {
            conflict("method", &method_name, &declared_in, diagnostics);
            continue;
        }
        let overloads = info.methods.entry(method_name.clone()).or_default();
        let own = overloads.iter().find(|own| {
            own.key.owner.as_deref() == Some(name)
                && own.signature.parameters == callable.signature.parameters
        });
        let Some(own) = own else {
            overloads.push(callable);
            continue;
        };
        if own.dispatch == MemberDispatch::Override {
            // The override replaces the inherited declaration: it is the only
            // candidate left in this class (B1 section 5).
            if check_override(
                &format!("method `{name}.{method_name}`"),
                &format!("`{declared_in}.{method_name}`"),
                own,
                &callable,
                member_span(declaration, &method_name),
                diagnostics,
            ) {
                context
                    .overrides
                    .insert(own.key.clone(), callable.key.clone());
            }
            context.matched_overrides.insert(own.key.clone());
            continue;
        }
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "method `{name}.{method_name}` has the signature of the inherited method `{declared_in}.{method_name}`"
                ),
                member_span(declaration, &method_name),
            )
            .with_help(if callable.dispatch == MemberDispatch::None {
                "hiding an inherited method is not supported; rename it or change the parameters (only a `virtual`, `abstract` or `override` member can be overridden)"
            } else {
                "add `override` to replace the inherited member, or rename it; hiding is not supported"
            }),
        );
    }

    let mut property_names = base_info.properties.keys().cloned().collect::<Vec<_>>();
    property_names.sort();
    for property_name in property_names {
        let property = &base_info.properties[&property_name];
        let accessible = property
            .getter
            .iter()
            .chain(property.setter.iter())
            .any(|accessor| accessor.visibility != Visibility::Private);
        let declared_in = property
            .getter
            .iter()
            .chain(property.setter.iter())
            .find_map(|accessor| accessor.key.owner.clone())
            .unwrap_or_else(|| base.clone());
        if !accessible {
            info.hidden_private
                .insert(property_name.clone(), declared_in);
            continue;
        }
        if info.fields.contains_key(&property_name) || info.methods.contains_key(&property_name) {
            conflict("property", &property_name, &declared_in, diagnostics);
            continue;
        }
        let Some(own) = info.properties.get(&property_name).cloned() else {
            info.properties
                .insert(property_name.clone(), property.clone());
            continue;
        };
        let own_dispatch = own
            .getter
            .iter()
            .chain(own.setter.iter())
            .map(|accessor| accessor.dispatch)
            .next()
            .unwrap_or(MemberDispatch::None);
        if own_dispatch != MemberDispatch::Override {
            conflict("property", &property_name, &declared_in, diagnostics);
            continue;
        }
        let span = member_span(declaration, &property_name);
        let target = format!("`{declared_in}.{property_name}`");
        let what = format!("property `{name}.{property_name}`");
        for accessor in own.getter.iter().chain(own.setter.iter()) {
            context.matched_overrides.insert(accessor.key.clone());
        }
        if own.type_ != property.type_ {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "{what} overrides {target} with a different type (`{}` instead of `{}`)",
                        own.type_.display(),
                        property.type_.display()
                    ),
                    span,
                )
                .with_help("an override must match the overridden property exactly"),
            );
            continue;
        }
        for (own_accessor, base_accessor, kind) in [
            (&own.getter, &property.getter, "getter"),
            (&own.setter, &property.setter, "setter"),
        ] {
            let Some(own_accessor) = own_accessor else {
                continue;
            };
            let Some(base_accessor) = base_accessor else {
                diagnostics.push(
                    Diagnostic::error(
                        format!("{what} adds a {kind} that {target} does not have"),
                        span,
                    )
                    .with_help(
                        "an override must keep the accessor set of the property it overrides",
                    ),
                );
                continue;
            };
            if check_override(
                &format!("{kind} of {what}"),
                &format!("the {kind} of {target}"),
                own_accessor,
                base_accessor,
                span,
                diagnostics,
            ) {
                context
                    .overrides
                    .insert(own_accessor.key.clone(), base_accessor.key.clone());
            }
        }
        // Overriding one accessor keeps the other inherited.
        let merged = info
            .properties
            .get_mut(&property_name)
            .expect("own property is still present");
        if merged.getter.is_none() {
            merged.getter.clone_from(&property.getter);
        }
        if merged.setter.is_none() {
            merged.setter.clone_from(&property.setter);
        }
    }
}

/// Validate one override against the member it replaces (B1 N2, N3): the
/// overridden member must be virtual, abstract or itself an override, and the
/// parameters (already equal), return type and visibility must match exactly.
/// Returns whether the override is well formed.
fn check_override(
    what: &str,
    target: &str,
    own: &Callable,
    base: &Callable,
    span: Span,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    if base.dispatch == MemberDispatch::None {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "{what} cannot override {target}, which is not virtual, abstract or an override"
                ),
                span,
            )
            .with_help("mark the base member `virtual` or `abstract`"),
        );
        return false;
    }
    if own.signature.result != base.signature.result {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "{what} overrides {target} with a different return type (`{}` instead of `{}`)",
                    own.signature.result.display(),
                    base.signature.result.display()
                ),
                span,
            )
            .with_help(
                "an override must match the signature exactly; covariant returns are not supported",
            ),
        );
        return false;
    }
    if own.visibility != base.visibility {
        diagnostics.push(
            Diagnostic::error(
                format!("{what} overrides {target} with a different visibility"),
                span,
            )
            .with_help("an override must keep the visibility of the member it overrides"),
        );
        return false;
    }
    true
}

/// Checks that need every class merged: an `override` that matched no
/// inherited member (B1 N2/N3), and a concrete class that leaves an inherited
/// abstract member unimplemented (B1 N7).
pub(super) fn validate_overrides(
    classes: &[&TypeDeclaration],
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for class in classes {
        let Some(info) = context.types.get(&class.name) else {
            continue;
        };
        for member in &class.members {
            match member {
                Member::Method(method)
                    if !method.constructor && method.dispatch == MemberDispatch::Override =>
                {
                    let matched = info
                        .methods
                        .get(&method.name)
                        .into_iter()
                        .flatten()
                        .any(|callable| context.matched_overrides.contains(&callable.key));
                    if !matched {
                        unmatched_override(
                            class,
                            info,
                            &method.name,
                            "method",
                            method.span,
                            context,
                            diagnostics,
                        );
                    }
                }
                Member::Property(property) if property.dispatch == MemberDispatch::Override => {
                    let matched = info.properties.get(&property.name).is_some_and(|found| {
                        found
                            .getter
                            .iter()
                            .chain(found.setter.iter())
                            .any(|accessor| context.matched_overrides.contains(&accessor.key))
                    });
                    if !matched {
                        unmatched_override(
                            class,
                            info,
                            &property.name,
                            "property",
                            property.span,
                            context,
                            diagnostics,
                        );
                    }
                }
                _ => {}
            }
        }
        if info.is_abstract || info.kind != Some(TypeKind::Class) {
            continue;
        }
        let mut missing = Vec::new();
        for (member, overloads) in &info.methods {
            for callable in overloads {
                if callable.dispatch == MemberDispatch::Abstract
                    && callable.key.owner.as_deref() != Some(class.name.as_str())
                {
                    missing.push(format!(
                        "{}.{member}",
                        callable.key.owner.as_deref().unwrap_or("?")
                    ));
                }
            }
        }
        for (member, property) in &info.properties {
            for accessor in property.getter.iter().chain(property.setter.iter()) {
                if accessor.dispatch == MemberDispatch::Abstract
                    && accessor.key.owner.as_deref() != Some(class.name.as_str())
                {
                    missing.push(format!(
                        "{}.{member}",
                        accessor.key.owner.as_deref().unwrap_or("?")
                    ));
                }
            }
        }
        missing.sort();
        missing.dedup();
        for member in missing {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "class `{}` does not override the inherited abstract member `{member}`",
                        class.name
                    ),
                    class.span,
                )
                .with_help("add an `override` of it, or declare the class `abstract`"),
            );
        }
    }
}

fn unmatched_override(
    class: &TypeDeclaration,
    info: &TypeInfo,
    name: &str,
    what: &str,
    span: Span,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let base_info = info
        .base
        .as_deref()
        .and_then(|base| context.types.get(base));
    let inherited_virtual_with_name = base_info.is_some_and(|base_info| {
        base_info
            .methods
            .get(name)
            .into_iter()
            .flatten()
            .any(|callable| callable.dispatch != MemberDispatch::None)
    });
    let message = if inherited_virtual_with_name {
        format!(
            "{what} `{}.{name}` is marked `override` but no inherited virtual method has these parameters",
            class.name
        )
    } else {
        format!(
            "{what} `{}.{name}` is marked `override` but there is no virtual or abstract member `{name}` to override",
            class.name
        )
    };
    diagnostics.push(Diagnostic::error(message, span).with_help(
        "an override must match an inherited `virtual`, `abstract` or `override` member exactly",
    ));
}

/// Resolve and validate the constructor a constructor runs first: the target
/// of `: this(arguments)` (a constructor of the same class, B2), the explicit
/// `: base(arguments)` or, without either, the base constructor whose
/// parameters are all defaulted. The arguments are analyzed in a context with
/// no object (no `this`, fields or instance methods), because the object is not
/// usable before that constructor completes. The resolved target is stored in
/// `Model::base_constructors`; for a delegation it is a constructor of the
/// owner itself.
#[allow(clippy::too_many_lines)]
pub(super) fn analyze_base_call(
    function: &FunctionDeclaration,
    owner: &TypeDeclaration,
    context: &Context,
    diagnostics: &mut Vec<Diagnostic>,
    model: &mut Model,
) {
    let Some(info) = context.types.get(&owner.name) else {
        return;
    };
    let delegates = function
        .base_call
        .as_ref()
        .is_some_and(|call| call.delegates_to_this);
    let (target, candidates, label) = if delegates {
        (owner.name.clone(), info.constructors.clone(), "this")
    } else {
        let Some(base) = info.base.clone() else {
            if let Some(call) = &function.base_call {
                diagnostics.push(
                    Diagnostic::error(
                        format!("`base(...)` requires `{}` to have a base class", owner.name),
                        call.span,
                    )
                    .with_help("name a base class after `:` in the class declaration, or remove the initializer"),
                );
            }
            return;
        };
        let Some(base_info) = context.types.get(&base) else {
            return;
        };
        let call_span = function
            .base_call
            .as_ref()
            .map_or(function.span, |call| call.span);
        if base_info.constructors.is_empty() {
            diagnostics.push(
                Diagnostic::error(
                    format!("base class `{base}` has no constructor to call"),
                    call_span,
                )
                .with_help(format!(
                    "declare a constructor in `{base}`; an implicit one exists only when every field has an initializer or a zero default"
                )),
            );
            return;
        }
        let accessible = base_info
            .constructors
            .iter()
            .filter(|constructor| constructor.visibility != Visibility::Private)
            .cloned()
            .collect::<Vec<_>>();
        if accessible.is_empty() {
            diagnostics.push(
                Diagnostic::error(
                    format!("constructor of base class `{base}` is private"),
                    call_span,
                )
                .with_help("make the base constructor `protected` or `public`"),
            );
            return;
        }
        if function.base_call.is_none()
            && !accessible
                .iter()
                .any(|constructor| constructor.defaults.iter().all(Option::is_some))
        {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "constructor of `{}` must call `base(...)`: base class `{base}` has no constructor without required arguments",
                        owner.name
                    ),
                    function.span,
                )
                .with_help(format!("write `: base(...)` after the parameter list, passing the arguments `{base}` needs")),
            );
            return;
        }
        (base, accessible, "base")
    };
    let call_span = function
        .base_call
        .as_ref()
        .map_or(function.span, |call| call.span);
    let model_context = crate::semantic::function_context(function, Some(owner));
    let mut analyzer = Analyzer::new(
        context,
        Type::Void,
        HashMap::new(),
        HashMap::new(),
        Some(owner.name.clone()),
        false,
        false,
        &HashSet::new(),
        model_context.clone(),
        model,
    );
    analyzer.base_arguments = true;
    analyzer.chain_label = label;
    for parameter in &function.parameters {
        analyzer.declare(
            &parameter.name,
            Binding {
                type_: resolve_type_readonly(&parameter.type_ref, context),
                mutable: true,
                iteration_readonly: false,
                initialized: true,
                span: parameter.span,
                value: None,
            },
        );
    }
    let arguments: &[Argument] = function
        .base_call
        .as_ref()
        .map_or(&[], |call| call.arguments.as_slice());
    let argument_types = arguments
        .iter()
        .map(|argument| {
            if super::expressions::needs_contextual_type(argument) {
                Type::Unknown
            } else {
                analyzer.expression(argument)
            }
        })
        .collect::<Vec<_>>();
    if let Some((callable, plan)) =
        analyzer.resolve_call_overload(&target, &candidates, &argument_types, arguments, call_span)
    {
        analyzer.model.base_constructors.insert(
            model_context,
            ResolvedBaseConstructor {
                callable: callable.key,
                arguments: plan,
            },
        );
    }
    diagnostics.append(&mut analyzer.diagnostics);
}

/// A constructor that delegates with `this(...)`, directly or through other
/// constructors, back to itself can never complete: report each cycle once.
pub(super) fn check_delegation_cycles(
    declaration: &TypeDeclaration,
    diagnostics: &mut Vec<Diagnostic>,
    model: &Model,
) {
    let constructors = declaration
        .members
        .iter()
        .filter_map(|member| match member {
            Member::Method(method) if method.constructor => Some(method),
            _ => None,
        })
        .collect::<Vec<_>>();
    let key_of = |method: &FunctionDeclaration| {
        crate::semantic::callable_key(
            &method.name,
            method.span.start,
            &method.parameters,
            None,
            Some(&declaration.name),
        )
    };
    // Constructor index -> index of the constructor its `this(...)` selects.
    let edges = constructors
        .iter()
        .map(|method| {
            if !method
                .base_call
                .as_ref()
                .is_some_and(|call| call.delegates_to_this)
            {
                return None;
            }
            let resolved = model
                .base_constructors
                .get(&crate::semantic::function_context(
                    method,
                    Some(declaration),
                ))?;
            constructors
                .iter()
                .position(|candidate| key_of(candidate) == resolved.callable)
        })
        .collect::<Vec<_>>();
    let mut reported = vec![false; constructors.len()];
    for start in 0..constructors.len() {
        let mut path: Vec<usize> = Vec::new();
        let mut current = Some(start);
        while let Some(index) = current {
            if let Some(at) = path.iter().position(|visited| *visited == index) {
                let cycle = &path[at..];
                if cycle.iter().all(|member| !reported[*member]) {
                    for member in cycle {
                        reported[*member] = true;
                    }
                    let first = *cycle.iter().min().expect("a cycle has a constructor");
                    diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "constructors of `{}` delegate to each other in a cycle through `this(...)`",
                                declaration.name
                            ),
                            constructors[first].span,
                        )
                        .with_help("break the cycle: one constructor must not delegate with `this(...)` and run the initializers and the body"),
                    );
                }
                break;
            }
            if reported[index] {
                break;
            }
            path.push(index);
            current = edges[index];
        }
    }
}

/// Span of the declaration of member `name`, for diagnostics.
fn member_span(declaration: &TypeDeclaration, name: &str) -> Span {
    declaration
        .members
        .iter()
        .find_map(|member| match member {
            Member::Field(field) if field.name == name => Some(field.span),
            Member::Method(method) if method.name == name && !method.constructor => {
                Some(method.span)
            }
            Member::Property(property) if property.name == name => Some(property.span),
            _ => None,
        })
        .unwrap_or(declaration.span)
}

impl Analyzer<'_> {
    /// The base class of the class being analyzed, when `base` can be used.
    pub(super) fn base_class_of_owner(&self) -> Option<String> {
        if !self.instance_context || self.base_arguments {
            return None;
        }
        self.owner
            .as_ref()
            .and_then(|owner| self.context.types.get(owner))
            .and_then(|info| info.base.clone())
    }

    /// `base` as the receiver of a member access: the static class is the base
    /// class, and the access is a static (non-virtual) use of `this`.
    pub(super) fn base_receiver(&mut self, span: Span) -> Type {
        if let Some(base) = self.base_class_of_owner() {
            return Type::Class(base);
        }
        let message = if self.base_arguments {
            "`base` cannot be used in the arguments of `base(...)`"
        } else if !self.instance_context {
            "`base` is valid only inside an instance member"
        } else {
            "`base` requires the class to have a base class"
        };
        self.diagnostics.push(
            Diagnostic::error(message, span)
                .with_help("name a base class after `:` in the class declaration"),
        );
        Type::Unknown
    }

    /// Whether `class` is `ancestor` or derives from it.
    pub(super) fn derives_from(&self, class: &str, ancestor: &str) -> bool {
        class == ancestor
            || self
                .context
                .types
                .get(class)
                .is_some_and(|info| info.ancestors.iter().any(|name| name == ancestor))
    }

    /// Enforce the member access rules. `member` is the noun phrase used in
    /// the diagnostic (for example ``field `Box.value` ``), `declared_in` the
    /// class that declares the member and `receiver` the static class of the
    /// expression the member is reached through.
    ///
    /// * `private`: only inside the declaring class.
    /// * `protected`: inside the declaring class and its derived classes, and
    ///   only through a receiver whose static type is the accessing class or a
    ///   class derived from it.
    pub(super) fn check_member_access(
        &mut self,
        visibility: Visibility,
        declared_in: &str,
        receiver: &str,
        member: &str,
        span: Span,
    ) {
        match visibility {
            Visibility::Public | Visibility::Internal => {}
            Visibility::Private => {
                if self.owner.as_deref() != Some(declared_in) {
                    self.diagnostics.push(
                        Diagnostic::error(format!("{member} is private"), span)
                            .with_help("access the value through a public member"),
                    );
                }
            }
            Visibility::Protected => {
                let Some(current) = self.owner.clone() else {
                    self.diagnostics.push(Diagnostic::error(
                        format!("{member} is protected and not accessible here"),
                        span,
                    ));
                    return;
                };
                if !self.derives_from(&current, declared_in) {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "{member} is protected and accessible only inside `{declared_in}` and its derived classes"
                            ),
                            span,
                        )
                        .with_help("use a public member, or access it from a derived class"),
                    );
                } else if !self.derives_from(receiver, &current) {
                    self.diagnostics.push(
                        Diagnostic::error(
                            format!(
                                "{member} is protected: inside `{current}` it can only be reached through a `{current}` (or derived) receiver, not a `{receiver}`"
                            ),
                            span,
                        )
                        .with_help("access it through `this` or a value whose type is the current class"),
                    );
                }
            }
        }
    }

    /// Diagnostic for a member that exists only as a private member of a base
    /// class. Returns `true` when it reported.
    pub(super) fn report_hidden_private(
        &mut self,
        type_name: &str,
        member: &str,
        kind: &str,
        span: Span,
    ) -> bool {
        let Some(declared_in) = self
            .context
            .types
            .get(type_name)
            .and_then(|info| info.hidden_private.get(member))
            .cloned()
        else {
            return false;
        };
        self.diagnostics.push(
            Diagnostic::error(
                format!("{kind} `{declared_in}.{member}` is private to its base class"),
                span,
            )
            .with_help(
                "make it `protected` or `public` in the base class to use it from a derived class",
            ),
        );
        true
    }
}
