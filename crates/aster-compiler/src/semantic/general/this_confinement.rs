//! The `this` rule for objects under construction (object model B1, section
//! 6.3, rule B: this-confinement).
//!
//! While a constructor runs, `this` is *under construction*, and for the whole
//! constructor chain (a base constructor runs on an object whose dynamic type
//! is the derived class). There, `this` may only be used to read and write
//! fields, as the receiver of a *this-confined* method, and in `==`/`!=`. Every
//! other use (passing, storing, returning, converting, capturing) is an error.
//!
//! A method is this-confined when its body is visible to the compiler, `this`
//! is only used in it under the same rules, and every method it calls on
//! `this` is this-confined. For each this-confined method the analysis also
//! computes `Reads(M)`: the fields of `this` it may read, transitively. A
//! constructor may call `M` on `this` only when every field of `Reads(M)` that
//! has no zero default is definitely assigned at the call site; writes made by
//! helpers never count as assignments. Unproven means rejected.
//!
//! Method bodies are analyzed in any order, so the analyzer records one
//! [`ThisUsage`] per instance body and the constructor call sites; the
//! fixpoint and the constructor checks run here once every body is known.

use std::collections::BTreeSet;

use super::{
    Analyzer, CallableKey, Context, Diagnostic, HashMap, HashSet, Model, Span, ThisSummary,
};

/// How one instance body (method, accessor or constructor) uses `this`.
#[derive(Clone, Debug, Default)]
pub(super) struct ThisUsage {
    /// The first use that lets `this` escape (methods only: in a constructor
    /// it is an immediate error).
    pub escape: Option<Span>,
    /// The first virtual or abstract member called on `this` (dynamic
    /// dispatch), in methods only; in a constructor it is an immediate error.
    pub virtual_call: Option<Span>,
    /// Fields of `this` the body may read, as `Declaring.field`.
    pub reads: BTreeSet<String>,
    /// Methods and accessors called on `this`, with the fields that were
    /// still unassigned at the call (constructors only).
    pub calls: Vec<ThisCall>,
}

#[derive(Clone, Debug)]
pub(super) struct ThisCall {
    pub callable: CallableKey,
    pub span: Span,
    /// Fields without a zero default that were not definitely assigned at the
    /// call, as `Declaring.field`.
    pub unassigned: Vec<String>,
    /// Dispatched dynamically: a virtual, abstract or overriding member
    /// reached without `base`.
    pub virtual_dispatch: bool,
}

impl Analyzer<'_> {
    /// Whether `name` currently resolves to a field of the object (not a
    /// local, parameter or global that shadows it).
    pub(super) fn is_field_binding(&self, name: &str) -> bool {
        if !self.instance_context || !self.field_names.contains(name) {
            return false;
        }
        self.scopes
            .iter()
            .rposition(|scope| scope.contains_key(name))
            == Some(0)
    }

    /// The class that declares field `name` of the current class.
    fn declaring_class_of_field(&self, name: &str) -> String {
        let owner = self.owner.clone().unwrap_or_default();
        self.context
            .types
            .get(&owner)
            .and_then(|info| info.field_owner.get(name).cloned())
            .unwrap_or(owner)
    }

    /// Record a read of field `name` of `this` (explicit or implicit).
    pub(super) fn note_this_field_read(&mut self, name: &str) {
        if self.this_usage.is_none() || !self.is_field_binding(name) {
            return;
        }
        let qualified = format!("{}.{name}", self.declaring_class_of_field(name));
        if let Some(usage) = &mut self.this_usage {
            usage.reads.insert(qualified);
        }
    }

    /// Record a read of an auto-property of `this` that is lowered directly
    /// to its backing field (LANG-005). Within the declaring class this is a
    /// field read, subject to the same definite-assignment rule; a property
    /// inherited from a base class keeps being summarized as its getter call.
    pub(super) fn note_auto_property_read(
        &mut self,
        backing: &str,
        declared_here: bool,
        getter: &CallableKey,
        property: &str,
        span: Span,
    ) {
        if !declared_here {
            self.note_this_call(getter, span);
            return;
        }
        if self.constructor
            && self
                .binding(backing)
                .is_some_and(|binding| !binding.initialized)
        {
            self.diagnostics.push(Diagnostic::error(
                format!("property `{property}` is used before initialization"),
                span,
            ));
        }
        self.note_this_field_read(backing);
    }

    /// Record a call of `callable` on `this` (a method, or a property
    /// accessor), with the constructor's current assignment state.
    pub(super) fn note_this_call(&mut self, callable: &CallableKey, span: Span) {
        if self.this_usage.is_none() {
            return;
        }
        // A virtual, abstract or overriding member is bound at run time to the
        // most derived override, whose body this analysis cannot see. A call
        // through `base` is a static call to the base implementation.
        let virtual_dispatch =
            !self.base_access && self.context.virtual_callables.contains(callable);
        if virtual_dispatch
            && !self.constructor
            && let Some(usage) = &mut self.this_usage
        {
            usage.virtual_call.get_or_insert(span);
            return;
        }
        let unassigned = if self.constructor {
            let mut names = self
                .field_names
                .iter()
                .filter(|field| {
                    self.binding(field)
                        .is_some_and(|binding| !binding.initialized)
                })
                .map(|field| format!("{}.{field}", self.declaring_class_of_field(field)))
                .collect::<Vec<_>>();
            names.sort();
            names
        } else {
            Vec::new()
        };
        if let Some(usage) = &mut self.this_usage {
            usage.calls.push(ThisCall {
                callable: callable.clone(),
                span,
                unassigned,
                virtual_dispatch,
            });
        }
    }

    /// `this` used as a receiver of a field access or a method call: the only
    /// uses a constructor allows besides `==`/`!=`. It is not an escape.
    pub(super) fn this_receiver(&mut self, span: Span) -> super::Type {
        if self.instance_context
            && let Some(owner) = self.owner.clone()
        {
            super::Type::Class(owner)
        } else {
            self.this_outside_instance(span)
        }
    }

    /// Analyze an expression that may be `this` in a receiver position.
    pub(super) fn expression_receiver(&mut self, expression: &super::Expression) -> super::Type {
        if matches!(expression.kind, super::ExpressionKind::This) {
            self.this_receiver(expression.span)
        } else if matches!(expression.kind, super::ExpressionKind::Base) {
            self.base_receiver(expression.span)
        } else {
            self.expression(expression)
        }
    }

    /// `this` used in a position that lets it escape (an argument, a stored
    /// or returned value, a conversion): an error in a constructor, and the
    /// end of this-confinement everywhere else.
    pub(super) fn this_escapes(&mut self, span: Span) {
        if self.constructor {
            self.diagnostics.push(
                Diagnostic::error(
                    "`this` cannot be passed, stored, returned or converted while the object is under construction",
                    span,
                )
                .with_help(
                    "inside a constructor `this` may only read or write fields, receive calls to this-confined methods, and be compared with `==`/`!=`",
                ),
            );
        } else if let Some(usage) = &mut self.this_usage {
            usage.escape.get_or_insert(span);
        }
    }

    pub(super) fn this_outside_instance(&mut self, span: Span) -> super::Type {
        let message = if self.base_arguments {
            if self.chain_label == "this" {
                "`this` cannot be used in the arguments of `this(...)`"
            } else {
                "`this` cannot be used in the arguments of `base(...)`"
            }
        } else {
            "`this` is valid only inside an instance method"
        };
        self.diagnostics.push(
            Diagnostic::error(message, span)
                .with_help("access the object through an explicit variable"),
        );
        super::Type::Unknown
    }
}

/// Run the fixpoint over every recorded body and check each constructor call.
#[allow(clippy::too_many_lines)]
pub(super) fn validate(context: &Context, model: &Model, diagnostics: &mut Vec<Diagnostic>) {
    if model.constructor_this_calls.is_empty() {
        return;
    }
    let summaries = &model.this_summaries;
    // Greatest fixpoint: a method is confined unless its body lets `this`
    // escape or it calls something that is not confined (or unknown).
    let mut confined = summaries
        .iter()
        .filter(|(_, summary)| summary.escape.is_none() && summary.virtual_call.is_none())
        .map(|(key, _)| key.clone())
        .collect::<HashSet<_>>();
    loop {
        let removed = confined
            .iter()
            .filter(|key| {
                summaries[*key]
                    .calls
                    .iter()
                    .any(|callee| !confined.contains(callee))
            })
            .cloned()
            .collect::<Vec<_>>();
        if removed.is_empty() {
            break;
        }
        for key in removed {
            confined.remove(&key);
        }
    }
    // Least fixpoint: `Reads(M)` includes the reads of every callee.
    let mut reads = confined
        .iter()
        .map(|key| (key.clone(), summaries[key].reads.clone()))
        .collect::<HashMap<_, _>>();
    loop {
        let mut changed = false;
        for key in &confined {
            for callee in &summaries[key].calls {
                let callee_reads = reads[callee].clone();
                let own = reads.get_mut(key).expect("confined method has reads");
                for read in callee_reads {
                    changed |= own.insert(read);
                }
            }
        }
        if !changed {
            break;
        }
    }
    let _ = context;
    for call in &model.constructor_this_calls {
        let name = display_callable(&call.callable);
        if call.virtual_dispatch {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "constructor of `{}` calls the virtual member `{name}` on `this`",
                        call.class
                    ),
                    call.span,
                )
                .with_help(
                    "a derived override would run before the derived constructor has completed; call a non-virtual method, or call `base.Member` to run the base implementation",
                ),
            );
            continue;
        }
        if !confined.contains(&call.callable) {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "constructor of `{}` calls `{name}`, which is not this-confined",
                        call.class
                    ),
                    call.span,
                )
                .with_help(why_not_confined(&call.callable, summaries, &confined)),
            );
            continue;
        }
        let blocked = reads[&call.callable]
            .iter()
            .filter(|read| call.unassigned.contains(read))
            .cloned()
            .collect::<Vec<_>>();
        if !blocked.is_empty() {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "`{name}` reads {} before the constructor of `{}` has assigned {}",
                        blocked
                            .iter()
                            .map(|field| format!("`{field}`"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        call.class,
                        if blocked.len() == 1 { "it" } else { "them" }
                    ),
                    call.span,
                )
                .with_help(
                    "assign every such field in the constructor body first; assignments made by a helper method do not count",
                ),
            );
        }
    }
}

fn display_callable(key: &CallableKey) -> String {
    let owner = key.owner.as_deref().unwrap_or("?");
    match key.accessor {
        Some(super::AccessorKind::Get) => format!("{owner}.{} (getter)", key.name),
        Some(super::AccessorKind::Set) => format!("{owner}.{} (setter)", key.name),
        None => format!("{owner}.{}", key.name),
    }
}

fn why_not_confined(
    key: &CallableKey,
    summaries: &HashMap<CallableKey, ThisSummary>,
    confined: &HashSet<CallableKey>,
) -> String {
    let Some(summary) = summaries.get(key) else {
        return "its body is not available to the compiler, so it cannot be proven safe".to_owned();
    };
    if summary.virtual_call.is_some() {
        return "its body calls a virtual or abstract member on `this`, which a derived class may override".to_owned();
    }
    if summary.escape.is_some() {
        return "its body lets `this` escape (it is passed, stored, returned or converted)"
            .to_owned();
    }
    summary
        .calls
        .iter()
        .find(|callee| !confined.contains(*callee))
        .map_or_else(
            || "its use of `this` could not be proven safe".to_owned(),
            |callee| {
                format!(
                    "it calls `{}`, which is not this-confined",
                    display_callable(callee)
                )
            },
        )
}
