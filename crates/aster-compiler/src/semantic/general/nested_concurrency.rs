//! Conservative, transitive rejection of nested concurrency (Lote 5, kept
//! deliberately simple ahead of Lote 6's real race/scheduling analysis).
//!
//! A function submitted to a worker (`Task.Run`'s target, or a `Parallel.For`
//! /`Parallel.ForEach` body) must never itself, or through any function it
//! calls, use `Task.Run`, `Task<T>.Wait`, `Parallel.For`/`Parallel.ForEach`,
//! or be an `async` function. Symmetrically, an `async` function's own
//! ordinary (non-awaited) calls must never reach `Parallel.For`/`ForEach`.
//! Both rules are checked over the same directly-resolved call graph, walked
//! with a visited set so ordinary (non-concurrency) recursion terminates and
//! is never itself an error.

use std::collections::{HashMap, HashSet};

use aster_diagnostics::{Diagnostic, Span};
use aster_syntax::{Block, FunctionDeclaration, Item, Member, Module, TypeDeclaration};

use super::{CallableKey, Model};

/// Static facts about one declared function or method, keyed by the same
/// [`CallableKey`] the resolved call graph in [`Model`] already uses.
struct FunctionFacts<'a> {
    name: String,
    is_async: bool,
    body: Option<&'a Block>,
}

pub(super) fn validate(module: &Module, model: &Model, diagnostics: &mut Vec<Diagnostic>) {
    let mut facts: HashMap<CallableKey, FunctionFacts<'_>> = HashMap::new();
    let mut context_to_key: HashMap<String, CallableKey> = HashMap::new();
    collect_facts(module, &mut facts, &mut context_to_key);

    // Direct call edges only (`Dispatch::Direct`/`Instance`, both resolved to
    // a concrete declaration); interface dispatch has no statically known
    // single target and is not walked here.
    let mut callees: HashMap<CallableKey, Vec<CallableKey>> = HashMap::new();
    for (node_key, resolved) in &model.calls {
        if let Some(caller) = context_to_key.get(&node_key.context) {
            callees
                .entry(caller.clone())
                .or_default()
                .push(resolved.callable.clone());
        }
    }

    for (node_key, resolved) in &model.task_runs {
        check_target(
            &resolved.function,
            "Task.Run",
            node_key.span,
            &facts,
            &callees,
            diagnostics,
        );
    }
    for (node_key, resolved) in &model.parallel_for {
        check_target(
            &resolved.body,
            "Parallel.For",
            node_key.span,
            &facts,
            &callees,
            diagnostics,
        );
    }
    for (node_key, resolved) in &model.parallel_for_each {
        check_target(
            &resolved.body,
            "Parallel.ForEach",
            node_key.span,
            &facts,
            &callees,
            diagnostics,
        );
    }

    for (key, fact) in &facts {
        if !fact.is_async {
            continue;
        }
        let Some(body) = fact.body else { continue };
        if let Some((offender, reason)) =
            find_reachable(key, &callees, &facts, ConcurrencyFilter::ParallelOnly)
        {
            diagnostics.push(
                Diagnostic::error(
                    format!(
                        "async function `{}` transitively calls `{}`, which uses {reason}",
                        fact.name, offender
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
    callees: &HashMap<CallableKey, Vec<CallableKey>>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let target_name = facts
        .get(target)
        .map_or_else(|| "<unknown>".to_owned(), |fact| fact.name.clone());
    if let Some(reason) = direct_reason(target, facts) {
        diagnostics.push(
            Diagnostic::error(
                format!("`{operation}` target `{target_name}` itself uses {reason}"),
                submission_span,
            )
            .with_help("nested concurrency is not supported in this version"),
        );
        return;
    }
    if let Some((offender, reason)) = find_reachable(target, callees, facts, ConcurrencyFilter::Any)
    {
        diagnostics.push(
            Diagnostic::error(
                format!(
                    "`{operation}` target `{target_name}` transitively calls `{offender}`, which uses {reason}"
                ),
                submission_span,
            )
            .with_help("nested concurrency is not supported in this version"),
        );
    }
}

/// The reason `key` itself is a direct concurrency use, if any.
fn direct_reason(
    key: &CallableKey,
    facts: &HashMap<CallableKey, FunctionFacts<'_>>,
) -> Option<&'static str> {
    let fact = facts.get(key)?;
    if fact.is_async {
        return Some("being an `async` function");
    }
    let body = fact.body?;
    direct_use_in_body(body)
}

/// Scan `body`'s statements structurally for a direct `Task.Run`,
/// `Task<T>.Wait`, `Parallel.For`, or `Parallel.ForEach` call, matching the
/// same structural shape checks the resolver itself uses.
fn direct_use_in_body(body: &Block) -> Option<&'static str> {
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
        if matches!(&callee.kind, aster_syntax::ExpressionKind::Member { name, .. } if name == "Wait")
        {
            return Some("`Task<T>.Wait`");
        }
        if super::calls::is_parallel_for_callee(callee) {
            return Some("`Parallel.For`");
        }
        if super::calls::is_parallel_for_each_callee(callee) {
            return Some("`Parallel.ForEach`");
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
    callees: &HashMap<CallableKey, Vec<CallableKey>>,
    facts: &HashMap<CallableKey, FunctionFacts<'_>>,
    filter: ConcurrencyFilter,
) -> Option<(String, &'static str)> {
    let mut visited: HashSet<CallableKey> = HashSet::new();
    visited.insert(start.clone());
    let mut queue: std::collections::VecDeque<CallableKey> =
        callees.get(start).cloned().unwrap_or_default().into();
    while let Some(current) = queue.pop_front() {
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
                    fact.body.and_then(direct_use_in_body)
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
                    } else {
                        None
                    }
                })
            }),
        };
        if let Some(reason) = reason {
            return Some((fact.name.clone(), reason));
        }
        if let Some(next) = callees.get(&current) {
            queue.extend(next.iter().cloned());
        }
    }
    None
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
    context_to_key.insert(context, key.clone());
    facts.insert(
        key,
        FunctionFacts {
            name: function.name.clone(),
            is_async: function.is_async,
            body: function.body.as_ref(),
        },
    );
}
