//! Conservative, transitive rejection of nested concurrency (Lote 5, kept
//! deliberately simple ahead of Lote 6's real race/scheduling analysis).
//!
//! A function submitted to a worker (`Task.Run`'s target, a `Parallel.For`
//! /`Parallel.ForEach` body, or a `Parallel.Reduce` `Accumulate`/`Combine`)
//! must never itself, or through any function it calls, use `Task.Run`,
//! `Task<T>.Wait`, `Parallel.For`/`ForEach`/`Reduce`, or be an `async`
//! function. Symmetrically, an `async` function's own ordinary (non-awaited)
//! calls must never reach `Parallel.For`/`ForEach`/`Reduce`. Both rules are
//! checked over the same resolved call graph. Interface calls conservatively
//! expand to every exact registered implementation. A visited set bounds
//! ordinary (non-concurrency) recursion.

use std::collections::{HashMap, HashSet, VecDeque};

use aster_diagnostics::{Diagnostic, Span};
use aster_syntax::{Block, FunctionDeclaration, Item, Member, Module, TypeDeclaration};

use super::{CallableKey, Context, Dispatch, Model, callable_key};

/// Static facts about one declared function or method, keyed by the same
/// [`CallableKey`] the resolved call graph in [`Model`] already uses.
struct FunctionFacts<'a> {
    name: String,
    context: String,
    is_async: bool,
    body: Option<&'a Block>,
}

#[derive(Clone)]
struct InterfaceDispatch {
    interface_call: CallableKey,
    implementation: CallableKey,
}

#[derive(Clone)]
struct CallEdge {
    target: CallableKey,
    interface_dispatch: Option<InterfaceDispatch>,
}

struct ReachableUse {
    offender: String,
    reason: &'static str,
    interface_dispatch: Option<InterfaceDispatch>,
}

pub(super) fn validate(
    module: &Module,
    context: &Context,
    model: &Model,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut facts: HashMap<CallableKey, FunctionFacts<'_>> = HashMap::new();
    let mut context_to_key: HashMap<String, CallableKey> = HashMap::new();
    collect_facts(module, &mut facts, &mut context_to_key);

    let callees = build_call_graph(module, context, model, &context_to_key);

    let mut task_runs = model.task_runs.iter().collect::<Vec<_>>();
    task_runs.sort_by_key(|(key, _)| (key.context.as_str(), key.span.start, key.span.end));
    for (node_key, resolved) in task_runs {
        check_target(
            &resolved.function,
            "Task.Run",
            node_key.span,
            &facts,
            &callees,
            model,
            diagnostics,
        );
    }
    let mut parallel_for = model.parallel_for.iter().collect::<Vec<_>>();
    parallel_for.sort_by_key(|(key, _)| (key.context.as_str(), key.span.start, key.span.end));
    for (node_key, resolved) in parallel_for {
        check_target(
            &resolved.body,
            "Parallel.For",
            node_key.span,
            &facts,
            &callees,
            model,
            diagnostics,
        );
    }
    let mut parallel_for_each = model.parallel_for_each.iter().collect::<Vec<_>>();
    parallel_for_each.sort_by_key(|(key, _)| (key.context.as_str(), key.span.start, key.span.end));
    for (node_key, resolved) in parallel_for_each {
        check_target(
            &resolved.body,
            "Parallel.ForEach",
            node_key.span,
            &facts,
            &callees,
            model,
            diagnostics,
        );
    }
    let mut parallel_reduce = model.parallel_reduce.iter().collect::<Vec<_>>();
    parallel_reduce.sort_by_key(|(key, _)| (key.context.as_str(), key.span.start, key.span.end));
    for (node_key, resolved) in parallel_reduce {
        check_target(
            &resolved.accumulate,
            "Parallel.Reduce (Accumulate)",
            node_key.span,
            &facts,
            &callees,
            model,
            diagnostics,
        );
        check_target(
            &resolved.combine,
            "Parallel.Reduce (Combine)",
            node_key.span,
            &facts,
            &callees,
            model,
            diagnostics,
        );
    }

    let mut fact_keys = facts.keys().collect::<Vec<_>>();
    fact_keys.sort_by(|left, right| callable_order(left, right));
    for key in fact_keys {
        let fact = &facts[key];
        if !fact.is_async {
            continue;
        }
        let Some(body) = fact.body else { continue };
        if let Some(reachable) = find_reachable(
            key,
            &callees,
            &facts,
            model,
            ConcurrencyFilter::ParallelOnly,
        ) {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "async function `{}` transitively calls `{}`, which uses {}",
                        fact.name, reachable.offender, reachable.reason
                    ),
                    body.span,
                )
                .with_help(
                    "`Parallel` is not supported inside an `async` function in this version",
                ),
            );
        }
    }
}

fn build_call_graph(
    module: &Module,
    context: &Context,
    model: &Model,
    context_to_key: &HashMap<String, CallableKey>,
) -> HashMap<CallableKey, Vec<CallEdge>> {
    let interface_targets = collect_interface_targets(module, context);
    let mut callees: HashMap<CallableKey, Vec<CallEdge>> = HashMap::new();
    let mut resolved_calls = model.calls.iter().collect::<Vec<_>>();
    resolved_calls.sort_by(|(left, _), (right, _)| {
        left.context
            .cmp(&right.context)
            .then(left.span.start.cmp(&right.span.start))
            .then(left.span.end.cmp(&right.span.end))
    });
    for (node_key, resolved) in resolved_calls {
        let Some(caller) = context_to_key.get(&node_key.context) else {
            continue;
        };
        let edges = callees.entry(caller.clone()).or_default();
        match resolved.dispatch {
            Dispatch::Interface => {
                if let Some(targets) = interface_targets.get(&resolved.callable) {
                    edges.extend(targets.iter().cloned().map(|implementation| CallEdge {
                        target: implementation.clone(),
                        interface_dispatch: Some(InterfaceDispatch {
                            interface_call: resolved.callable.clone(),
                            implementation,
                        }),
                    }));
                }
            }
            Dispatch::Direct | Dispatch::Instance => edges.push(CallEdge {
                target: resolved.callable.clone(),
                interface_dispatch: None,
            }),
        }
    }
    callees
}

/// Which concurrency uses to look for while walking a call graph.
#[derive(Clone, Copy)]
enum ConcurrencyFilter {
    /// Any of `Task.Run`, `Task<T>.Wait`, `Parallel.For`/`ForEach`, or being
    /// `async`: used when validating a concurrency target.
    Any,
    /// Only `Parallel.For`/`ForEach`: used when validating an async body's
    /// ordinary calls, where `Task.Run`/`await` are already the expected shape.
    ParallelOnly,
}

fn check_target(
    target: &CallableKey,
    operation: &str,
    submission_span: Span,
    facts: &HashMap<CallableKey, FunctionFacts<'_>>,
    callees: &HashMap<CallableKey, Vec<CallEdge>>,
    model: &Model,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target_name = facts
        .get(target)
        .map_or_else(|| "<unknown>".to_owned(), |fact| fact.name.clone());
    if let Some(reason) = direct_reason(target, facts, model) {
        diagnostics.push(
            Diagnostic::error(
                format!("`{operation}` target `{target_name}` itself uses {reason}"),
                submission_span,
            )
            .with_help("nested concurrency is not supported in this version"),
        );
        return;
    }
    if let Some(reachable) = find_reachable(target, callees, facts, model, ConcurrencyFilter::Any) {
        let message = if let Some(dispatch) = reachable.interface_dispatch {
            format!(
                "`{operation}` target `{target_name}` reaches nested concurrency through interface call `{interface_call}`; concrete implementation `{implementation}` reaches `{offender}`, which uses {reason}",
                interface_call = callable_name(&dispatch.interface_call),
                implementation = callable_name(&dispatch.implementation),
                offender = reachable.offender,
                reason = reachable.reason,
            )
        } else {
            format!(
                "`{operation}` target `{target_name}` transitively calls `{}`, which uses {}",
                reachable.offender, reachable.reason
            )
        };
        diagnostics.push(
            Diagnostic::error(message, submission_span)
                .with_help("nested concurrency is not supported in this version"),
        );
    }
}

/// The reason `key` itself is a direct concurrency use, if any.
fn direct_reason(
    key: &CallableKey,
    facts: &HashMap<CallableKey, FunctionFacts<'_>>,
    model: &Model,
) -> Option<&'static str> {
    let fact = facts.get(key)?;
    if fact.is_async {
        return Some("being an `async` function");
    }
    let body = fact.body?;
    direct_use_in_body(body, &fact.context, model)
}

/// Scan `body`'s statements structurally for a direct `Task.Run`,
/// `Task<T>.Wait`, `Parallel.For`, or `Parallel.ForEach` call, matching the
/// same structural shape checks the resolver itself uses.
fn direct_use_in_body(body: &Block, context: &str, model: &Model) -> Option<&'static str> {
    let mut calls = Vec::new();
    for statement in &body.statements {
        super::declarations::collect_statement_calls(statement, &mut calls);
    }
    for call in calls {
        let aster_syntax::ExpressionKind::Call { callee, .. } = &call.kind else {
            continue;
        };
        if super::calls::is_task_run_callee(callee) {
            return Some("`Task.Run`");
        }
        let key = crate::semantic::ModelNodeKey {
            context: context.to_owned(),
            span: call.span,
        };
        if model.foreign_calls.contains(&key) {
            return Some("a foreign call");
        }
        if matches!(&callee.kind, aster_syntax::ExpressionKind::Member { name, .. } if name == "Wait")
            && !model.calls.contains_key(&key)
        {
            return Some("`Task<T>.Wait`");
        }
        if super::calls::is_parallel_for_callee(callee) {
            return Some("`Parallel.For`");
        }
        if super::calls::is_parallel_for_each_callee(callee) {
            return Some("`Parallel.ForEach`");
        }
        if super::calls::is_parallel_reduce_callee(callee) {
            return Some("`Parallel.Reduce`");
        }
    }
    None
}

/// Breadth-first search over `callees` starting at `start`'s own callees
/// (not `start` itself), returning the first reachable function's display
/// name and reason once a direct concurrency use matching `filter` is found.
/// A visited set bounds the walk, so plain recursion (including through
/// `start` itself) never loops.
fn find_reachable(
    start: &CallableKey,
    callees: &HashMap<CallableKey, Vec<CallEdge>>,
    facts: &HashMap<CallableKey, FunctionFacts<'_>>,
    model: &Model,
    filter: ConcurrencyFilter,
) -> Option<ReachableUse> {
    let mut visited: HashSet<CallableKey> = HashSet::new();
    visited.insert(start.clone());
    let mut queue: VecDeque<(CallableKey, Option<InterfaceDispatch>)> = callees
        .get(start)
        .into_iter()
        .flatten()
        .map(|edge| (edge.target.clone(), edge.interface_dispatch.clone()))
        .collect();
    while let Some((current, interface_dispatch)) = queue.pop_front() {
        if !visited.insert(current.clone()) {
            continue;
        }
        let Some(fact) = facts.get(&current) else {
            continue;
        };
        let reason = match filter {
            ConcurrencyFilter::Any => {
                if fact.is_async {
                    Some("being an `async` function")
                } else {
                    fact.body
                        .and_then(|body| direct_use_in_body(body, &fact.context, model))
                }
            }
            ConcurrencyFilter::ParallelOnly => fact.body.and_then(|body| {
                let mut calls = Vec::new();
                for statement in &body.statements {
                    super::declarations::collect_statement_calls(statement, &mut calls);
                }
                calls.into_iter().find_map(|call| {
                    let aster_syntax::ExpressionKind::Call { callee, .. } = &call.kind else {
                        return None;
                    };
                    if super::calls::is_parallel_for_callee(callee) {
                        Some("`Parallel.For`")
                    } else if super::calls::is_parallel_for_each_callee(callee) {
                        Some("`Parallel.ForEach`")
                    } else if super::calls::is_parallel_reduce_callee(callee) {
                        Some("`Parallel.Reduce`")
                    } else {
                        None
                    }
                })
            }),
        };
        if let Some(reason) = reason {
            return Some(ReachableUse {
                offender: fact.name.clone(),
                reason,
                interface_dispatch,
            });
        }
        if let Some(next) = callees.get(&current) {
            queue.extend(next.iter().map(|edge| {
                (
                    edge.target.clone(),
                    edge.interface_dispatch
                        .clone()
                        .or_else(|| interface_dispatch.clone()),
                )
            }));
        }
    }
    None
}

/// Build the conservative targets of each resolved interface method from the
/// same concrete type table and exact signatures used by interface validation.
fn collect_interface_targets(
    module: &Module,
    context: &Context,
) -> HashMap<CallableKey, Vec<CallableKey>> {
    let mut targets: HashMap<CallableKey, Vec<CallableKey>> = HashMap::new();
    for item in &module.items {
        let Item::Class(class) = item else { continue };
        let Some(class_info) = context.types.get(&class.name) else {
            continue;
        };
        for interface_name in &class_info.implemented_interfaces {
            let Some(interface) = module.items.iter().find_map(|item| match item {
                Item::Interface(interface) if interface.name == *interface_name => Some(interface),
                _ => None,
            }) else {
                continue;
            };
            let Some(interface_info) = context.types.get(interface_name) else {
                continue;
            };
            for member in &interface.members {
                let Member::Method(required_declaration) = member else {
                    continue;
                };
                let required_key = callable_key(
                    &required_declaration.name,
                    required_declaration.span.start,
                    None,
                    Some(interface_name),
                );
                let Some(required) = interface_info
                    .methods
                    .get(&required_declaration.name)
                    .into_iter()
                    .flatten()
                    .find(|candidate| candidate.key == required_key)
                else {
                    continue;
                };
                let Some(implementation) = class_info
                    .methods
                    .get(&required_declaration.name)
                    .into_iter()
                    .flatten()
                    .find(|candidate| {
                        !candidate.is_static && candidate.signature == required.signature
                    })
                else {
                    continue;
                };
                let implementations = targets.entry(required.key.clone()).or_default();
                if !implementations.contains(&implementation.key) {
                    implementations.push(implementation.key.clone());
                }
            }
        }
    }
    targets
}

fn callable_name(key: &CallableKey) -> String {
    key.owner
        .as_ref()
        .map_or_else(|| key.name.clone(), |owner| format!("{owner}.{}", key.name))
}

fn callable_order(left: &CallableKey, right: &CallableKey) -> std::cmp::Ordering {
    left.owner
        .cmp(&right.owner)
        .then(left.name.cmp(&right.name))
        .then(left.declaration_start.cmp(&right.declaration_start))
}

fn collect_facts<'a>(
    module: &'a Module,
    facts: &mut HashMap<CallableKey, FunctionFacts<'a>>,
    context_to_key: &mut HashMap<String, CallableKey>,
) {
    for item in &module.items {
        match item {
            Item::Function(function) => {
                insert_fact(function, None, facts, context_to_key);
            }
            Item::Class(declaration) | Item::Struct(declaration) => {
                for member in &declaration.members {
                    if let Member::Method(method) = member {
                        insert_fact(method, Some(declaration), facts, context_to_key);
                    }
                }
            }
            Item::Interface(_) | Item::Enum(_) | Item::Variable(_) => {}
        }
    }
}

fn insert_fact<'a>(
    function: &'a FunctionDeclaration,
    owner: Option<&TypeDeclaration>,
    facts: &mut HashMap<CallableKey, FunctionFacts<'a>>,
    context_to_key: &mut HashMap<String, CallableKey>,
) {
    let owner_name = owner.map(|owner| owner.name.as_str());
    let key = super::callable_key(&function.name, function.span.start, None, owner_name);
    let context = crate::semantic::function_context(function, owner);
    context_to_key.insert(context.clone(), key.clone());
    facts.insert(
        key,
        FunctionFacts {
            name: function.name.clone(),
            context,
            is_async: function.is_async,
            body: function.body.as_ref(),
        },
    );
}
