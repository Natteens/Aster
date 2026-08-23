//! Minting fresh, collision-free `SymbolId`s for compiler-generated functions
//! (today only async `MoveNext` machines). Symbols are dense counters assigned
//! during HIR lowering; the highest one in the module bounds every real
//! declaration, so allocating strictly above it can never collide with a
//! user-declared symbol and, because generated symbols carry names no source
//! can spell, they are never resolvable, selectable as an entry point, or
//! chosen by a manifest.

use super::hir;

/// Hands out `SymbolId`s strictly greater than every symbol already present in
/// the module.
pub(super) struct SymbolAllocator {
    next: u32,
}

impl SymbolAllocator {
    pub(super) fn new(module: &hir::Module) -> Self {
        let mut max = 0;
        for item in &module.items {
            collect_item(item, &mut max);
        }
        Self {
            next: max.saturating_add(1),
        }
    }

    pub(super) fn fresh(&mut self) -> hir::SymbolId {
        let symbol = hir::SymbolId(self.next);
        self.next = self.next.checked_add(1).expect("symbol id space exhausted");
        symbol
    }
}

fn note(max: &mut u32, symbol: hir::SymbolId) {
    *max = (*max).max(symbol.0);
}

fn collect_item(item: &hir::Item, max: &mut u32) {
    match item {
        hir::Item::Class(declaration)
        | hir::Item::Struct(declaration)
        | hir::Item::Interface(declaration) => {
            note(max, declaration.symbol);
            for field in &declaration.fields {
                note(max, field.symbol);
                if let Some(initializer) = &field.initializer {
                    collect_expression(initializer, max);
                }
            }
            for method in &declaration.methods {
                collect_function(method, max);
            }
        }
        hir::Item::Enum(declaration) => {
            note(max, declaration.symbol);
            for case in &declaration.cases {
                note(max, case.symbol);
                for field in &case.fields {
                    note(max, field.symbol);
                }
            }
        }
        hir::Item::Function(function) => collect_function(function, max),
        hir::Item::Variable(variable) => collect_variable(variable, max),
    }
}

fn collect_function(function: &hir::Function, max: &mut u32) {
    note(max, function.symbol);
    for parameter in &function.parameters {
        note(max, parameter.symbol);
    }
    if let Some(body) = &function.body {
        collect_block(body, max);
    }
}

fn collect_block(block: &hir::Block, max: &mut u32) {
    for statement in &block.statements {
        collect_statement(statement, max);
    }
}

fn collect_statement(statement: &hir::Statement, max: &mut u32) {
    match statement {
        hir::Statement::Variable(variable) => collect_variable(variable, max),
        hir::Statement::Return(value) => {
            if let Some(value) = value {
                collect_expression(value, max);
            }
        }
        hir::Statement::Expression(expression) => collect_expression(expression, max),
        hir::Statement::If {
            condition,
            then_block,
            else_block,
        } => {
            collect_expression(condition, max);
            collect_block(then_block, max);
            if let Some(else_block) = else_block {
                collect_block(else_block, max);
            }
        }
        hir::Statement::While { condition, body } => {
            collect_expression(condition, max);
            collect_block(body, max);
        }
        hir::Statement::For {
            initializer,
            condition,
            update,
            body,
        } => {
            if let Some(initializer) = initializer {
                collect_statement(initializer, max);
            }
            if let Some(condition) = condition {
                collect_expression(condition, max);
            }
            if let Some(update) = update {
                collect_expression(update, max);
            }
            collect_block(body, max);
        }
        hir::Statement::ForEach {
            element,
            collection,
            body,
        } => {
            note(max, element.symbol);
            collect_expression(collection, max);
            collect_block(body, max);
        }
        hir::Statement::Switch {
            value,
            cases,
            default,
        } => {
            collect_expression(value, max);
            for case in cases {
                note(max, case.case);
                for binding in &case.bindings {
                    note(max, binding.symbol);
                }
                collect_block(&case.body, max);
            }
            if let Some(default) = default {
                collect_block(default, max);
            }
        }
        hir::Statement::Break | hir::Statement::Continue => {}
        hir::Statement::Block(block) => collect_block(block, max),
    }
}

fn collect_variable(variable: &hir::Variable, max: &mut u32) {
    note(max, variable.symbol);
    if let Some(initializer) = &variable.initializer {
        collect_expression(initializer, max);
    }
}

#[allow(clippy::too_many_lines)]
fn collect_expression(expression: &hir::Expression, max: &mut u32) {
    match &expression.kind {
        hir::ExpressionKind::Literal(_)
        | hir::ExpressionKind::NewList { .. }
        | hir::ExpressionKind::NewDictionary { .. }
        | hir::ExpressionKind::TaskCancellationRequested => {}
        hir::ExpressionKind::NewStringBuilder { class_symbol } => note(max, *class_symbol),
        hir::ExpressionKind::Symbol(symbol) => note(max, *symbol),
        hir::ExpressionKind::StructLiteral {
            struct_symbol,
            fields,
        } => {
            note(max, *struct_symbol);
            for field in fields {
                note(max, field.field);
                collect_expression(&field.value, max);
            }
        }
        hir::ExpressionKind::EnumValue {
            enum_symbol,
            case,
            fields,
            ..
        } => {
            note(max, *enum_symbol);
            note(max, *case);
            for field in fields {
                note(max, field.field);
                collect_expression(&field.value, max);
            }
        }
        hir::ExpressionKind::NewObject {
            class_symbol,
            constructor,
            arguments,
            ..
        } => {
            note(max, *class_symbol);
            note(max, *constructor);
            for argument in arguments {
                collect_expression(argument, max);
            }
        }
        hir::ExpressionKind::ArrayLiteral(elements) => {
            for element in elements {
                collect_expression(element, max);
            }
        }
        hir::ExpressionKind::NewArray { length, .. } => collect_expression(length, max),
        hir::ExpressionKind::ListAdd { list, value } => {
            collect_expression(list, max);
            collect_expression(value, max);
        }
        hir::ExpressionKind::ListEnsureCapacity { list, minimum } => {
            collect_expression(list, max);
            collect_expression(minimum, max);
        }
        hir::ExpressionKind::DictionaryEnsureCapacity {
            dictionary,
            minimum,
        } => {
            collect_expression(dictionary, max);
            collect_expression(minimum, max);
        }
        hir::ExpressionKind::DictionaryGetOr {
            dictionary,
            key,
            fallback,
        } => {
            collect_expression(dictionary, max);
            collect_expression(key, max);
            collect_expression(fallback, max);
        }
        hir::ExpressionKind::ListAddRange { list, values, .. } => {
            collect_expression(list, max);
            collect_expression(values, max);
        }
        hir::ExpressionKind::ListRemoveRange { list, index, count }
        | hir::ExpressionKind::ListGetRange {
            list, index, count, ..
        } => {
            collect_expression(list, max);
            collect_expression(index, max);
            collect_expression(count, max);
        }
        hir::ExpressionKind::ListReverse { list } => collect_expression(list, max),
        hir::ExpressionKind::StringBuilderAppend {
            builder,
            value,
            class_symbol,
        } => {
            note(max, *class_symbol);
            collect_expression(builder, max);
            collect_expression(value, max);
        }
        hir::ExpressionKind::StringBuilderToString {
            builder,
            class_symbol,
        }
        | hir::ExpressionKind::StringBuilderLength {
            builder,
            class_symbol,
        }
        | hir::ExpressionKind::StringBuilderClear {
            builder,
            class_symbol,
        } => {
            note(max, *class_symbol);
            collect_expression(builder, max);
        }
        hir::ExpressionKind::DictionaryAdd {
            dictionary,
            key,
            value,
        }
        | hir::ExpressionKind::DictionarySet {
            dictionary,
            key,
            value,
        }
        | hir::ExpressionKind::DictionaryGetOrAdd {
            dictionary,
            key,
            value,
        } => {
            collect_expression(dictionary, max);
            collect_expression(key, max);
            collect_expression(value, max);
        }
        hir::ExpressionKind::DictionaryTryGet {
            dictionary,
            key,
            option_layout,
            ..
        } => {
            collect_expression(dictionary, max);
            collect_expression(key, max);
            note(max, option_layout.some_case);
            note(max, option_layout.some_field);
            note(max, option_layout.none_case);
        }
        hir::ExpressionKind::DictionaryContainsKey { dictionary, key }
        | hir::ExpressionKind::DictionaryRemove { dictionary, key } => {
            collect_expression(dictionary, max);
            collect_expression(key, max);
        }
        hir::ExpressionKind::DictionaryEntries {
            dictionary,
            entry_layout,
            ..
        } => {
            collect_expression(dictionary, max);
            note(max, entry_layout.key_field);
            note(max, entry_layout.value_field);
        }
        hir::ExpressionKind::DictionaryKeys {
            dictionary,
            key_type: _,
        }
        | hir::ExpressionKind::DictionaryValues {
            dictionary,
            value_type: _,
        }
        | hir::ExpressionKind::DictionaryClear { dictionary }
        | hir::ExpressionKind::ListClear { list: dictionary } => {
            collect_expression(dictionary, max);
        }
        hir::ExpressionKind::ListRemoveAt { list, index }
        | hir::ExpressionKind::ListGet { list, index, .. }
        | hir::ExpressionKind::ListIndexIncrementDecrement { list, index, .. } => {
            collect_expression(list, max);
            collect_expression(index, max);
        }
        hir::ExpressionKind::ListSet { list, index, value }
        | hir::ExpressionKind::ListInsert { list, index, value }
        | hir::ExpressionKind::ListIndexAssignment {
            list, index, value, ..
        } => {
            collect_expression(list, max);
            collect_expression(index, max);
            collect_expression(value, max);
        }
        hir::ExpressionKind::ListToArray {
            list,
            element_type: _,
        } => {
            collect_expression(list, max);
        }
        hir::ExpressionKind::Index { array, index } => {
            collect_expression(array, max);
            collect_expression(index, max);
        }
        hir::ExpressionKind::ArrayLength(operand)
        | hir::ExpressionKind::StringLength(operand)
        | hir::ExpressionKind::ListLength(operand)
        | hir::ExpressionKind::ListCapacity(operand)
        | hir::ExpressionKind::DictionaryLength(operand)
        | hir::ExpressionKind::DictionaryCapacity(operand)
        | hir::ExpressionKind::Convert { operand }
        | hir::ExpressionKind::Unary { operand, .. } => {
            collect_expression(operand, max);
        }
        hir::ExpressionKind::StringOperation {
            receiver,
            arguments,
            ..
        } => {
            collect_expression(receiver, max);
            for argument in arguments {
                collect_expression(argument, max);
            }
        }
        hir::ExpressionKind::FormatPrimitive { receiver, .. } => {
            collect_expression(receiver, max);
        }
        hir::ExpressionKind::Member { object, symbol } => {
            note(max, *symbol);
            collect_expression(object, max);
        }
        hir::ExpressionKind::Call {
            callee, arguments, ..
        } => {
            collect_expression(callee, max);
            for argument in arguments {
                collect_expression(argument, max);
            }
        }
        hir::ExpressionKind::ForeignCall {
            function,
            arguments,
            ..
        }
        | hir::ExpressionKind::TaskRun {
            function,
            arguments,
            ..
        } => {
            note(max, *function);
            for argument in arguments {
                collect_expression(argument, max);
            }
        }
        hir::ExpressionKind::PropertyAssignment {
            object,
            getter,
            setter,
            value,
            ..
        } => {
            if let Some(getter) = getter {
                note(max, *getter);
            }
            note(max, *setter);
            collect_expression(object, max);
            collect_expression(value, max);
        }
        hir::ExpressionKind::LogCall { argument, .. } => collect_expression(argument, max),
        hir::ExpressionKind::UpcastInterface {
            object,
            class,
            interface,
        } => {
            note(max, *class);
            note(max, *interface);
            collect_expression(object, max);
        }
        hir::ExpressionKind::IncrementDecrement { target, .. } => collect_expression(target, max),
        hir::ExpressionKind::Conditional {
            condition,
            when_true,
            when_false,
        } => {
            collect_expression(condition, max);
            collect_expression(when_true, max);
            collect_expression(when_false, max);
        }
        hir::ExpressionKind::Switch {
            value,
            cases,
            default,
        } => {
            collect_expression(value, max);
            for case in cases {
                note(max, case.case);
                for binding in &case.bindings {
                    note(max, binding.symbol);
                }
                collect_expression(&case.value, max);
            }
            if let Some(default) = default {
                collect_expression(default, max);
            }
        }
        hir::ExpressionKind::PropagateResult {
            operand,
            ok_case,
            ok_field,
            error_case,
            error_field,
            return_error_case,
            return_error_field,
            ..
        } => {
            note(max, *ok_case);
            note(max, *ok_field);
            note(max, *error_case);
            note(max, *error_field);
            note(max, *return_error_case);
            note(max, *return_error_field);
            collect_expression(operand, max);
        }
        hir::ExpressionKind::PropagateOption {
            operand,
            some_case,
            some_field,
            return_none_case,
            ..
        } => {
            note(max, *some_case);
            note(max, *some_field);
            note(max, *return_none_case);
            collect_expression(operand, max);
        }
        hir::ExpressionKind::Binary { left, right, .. } => {
            collect_expression(left, max);
            collect_expression(right, max);
        }
        hir::ExpressionKind::Assignment { target, value, .. } => {
            collect_expression(target, max);
            collect_expression(value, max);
        }
        hir::ExpressionKind::InterpolatedString { parts } => {
            for part in parts {
                if let hir::InterpolatedPart::Expression(expression) = part {
                    collect_expression(expression, max);
                }
            }
        }
        hir::ExpressionKind::TaskWait { task, .. } | hir::ExpressionKind::TaskCancel { task } => {
            collect_expression(task, max);
        }
        hir::ExpressionKind::TaskWaitAll { tasks, .. } => collect_expression(tasks, max),
        hir::ExpressionKind::Await { operand, .. } => collect_expression(operand, max),
        hir::ExpressionKind::ParallelFor {
            start, end, body, ..
        } => {
            note(max, *body);
            collect_expression(start, max);
            collect_expression(end, max);
        }
        hir::ExpressionKind::ParallelForEach { values, body, .. } => {
            note(max, *body);
            collect_expression(values, max);
        }
        hir::ExpressionKind::ParallelReduce {
            values,
            identity,
            accumulate,
            combine,
            ..
        } => {
            note(max, *accumulate);
            note(max, *combine);
            collect_expression(values, max);
            collect_expression(identity, max);
        }
    }
}
