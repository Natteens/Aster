//! Integer constants passed to parameters of another integer type.
//!
//! ASTER converts an integer constant implicitly when its value fits the target
//! type (`byte b = 10;`). Calls must follow the same rule: before the fix, a call
//! such as `Identity(123)` to a `ulong` parameter was accepted by the argument
//! check but never recorded as resolved, and HIR lowering panicked with
//! "validated call is missing lowering metadata".

use aster_compiler::{Compilation, compile, hir, mir};

fn compile_valid(source: &str) -> Compilation {
    compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}\n{source}"))
}

fn errors(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

/// Every call expression nested in the expression, innermost first.
fn calls(expression: &hir::Expression, out: &mut Vec<hir::Expression>) {
    match &expression.kind {
        hir::ExpressionKind::Call { arguments, .. } => {
            for argument in arguments {
                calls(argument, out);
            }
            out.push(expression.clone());
        }
        hir::ExpressionKind::Convert { operand } => calls(operand, out),
        hir::ExpressionKind::Binary { left, right, .. } => {
            calls(left, out);
            calls(right, out);
        }
        _ => {}
    }
}

fn first_call_in(compilation: &Compilation, function: &str) -> hir::Expression {
    let function = compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(candidate) if candidate.name == function => Some(candidate),
            _ => None,
        })
        .expect("HIR function");
    let mut found = Vec::new();
    for statement in &function.body.as_ref().expect("body").statements {
        if let hir::Statement::Return(Some(expression)) = statement {
            calls(expression, &mut found);
        }
    }
    // Nested calls are collected innermost first; the outermost call is last.
    found.pop().expect("a call in the return expression")
}

#[test]
fn reported_example_compiles_and_converts_the_argument() {
    let compilation = compile_valid(
        "public ulong Identity(ulong value) { return value; }
         public ulong Example() { return Identity(123); }",
    );
    let call = first_call_in(&compilation, "Example");
    assert_eq!(call.type_, hir::Type::ULong);
    let hir::ExpressionKind::Call { arguments, .. } = &call.kind else {
        panic!("a call");
    };
    // The literal is an `int`; HIR converts it to the parameter type.
    assert_eq!(arguments[0].type_, hir::Type::ULong);
    let hir::ExpressionKind::Convert { operand } = &arguments[0].kind else {
        panic!("the argument must be converted: {:?}", arguments[0]);
    };
    assert_eq!(operand.type_, hir::Type::Int);
}

#[test]
fn constants_convert_for_every_literal_form_and_parameter_type() {
    for parameter in [
        "byte", "sbyte", "short", "ushort", "uint", "ulong", "long", "int",
    ] {
        for argument in ["7", "0x7", "0b111", "(7)", "-0"] {
            if argument == "-0" && matches!(parameter, "byte" | "ushort" | "uint" | "ulong") {
                // Negating zero is an `int` zero, which fits every integer type.
            }
            let source = format!(
                "public {parameter} Identity({parameter} value) {{ return value; }}
                 public {parameter} Example() {{ return Identity({argument}); }}"
            );
            compile_valid(&source);
        }
    }
    // Wider unsigned literals reach the same parameters when the value fits.
    compile_valid(
        "public ulong Identity(ulong value) { return value; }
         public ulong Example() { return Identity(0xFFFFFFFFFFFFFFFF); }",
    );
    compile_valid(
        "public uint Identity(uint value) { return value; }
         public uint Example() { return Identity(0xFFFFFFFF) + Identity(4294967295U); }",
    );
}

#[test]
fn the_same_rule_applies_to_methods_constructors_named_and_nested_calls() {
    let compilation = compile_valid(
        "public class Counter
         {
             private ulong total;
             public Counter(ulong start) { total = start; }
             public ulong Add(ulong amount) { total = total + amount; return total; }
             public static uint Twice(uint value) { return value * 2U; }
         }
         public uint Pick(uint first, uint second) { return first + second; }
         public ulong Scale(ulong value, byte factor) { return value * factor; }
         public ulong Example()
         {
             Counter counter = new Counter(10);
             uint twice = Counter.Twice(21);
             uint named = Pick(second: 5, first: 7);
             ulong nested = counter.Add(Scale(3, 4));
             return nested + counter.Add(0x10);
         }",
    );
    let _ = compilation;
}

#[test]
fn out_of_range_and_non_constant_arguments_are_controlled_errors() {
    let identity = "public ulong Identity(ulong value) { return value; }\n";
    for (call, message) in [
        ("Identity(-1)", "constant value `-1` does not fit `ulong`"),
        ("Identity(-0x1)", "constant value `-1` does not fit `ulong`"),
    ] {
        assert_eq!(
            errors(&format!("{identity}public ulong F() {{ return {call}; }}")),
            [message],
            "{call}"
        );
    }
    let byte = "public byte Id(byte value) { return value; }\n";
    for (call, message) in [
        ("Id(256)", "constant value `256` does not fit `byte`"),
        ("Id(0x100)", "constant value `256` does not fit `byte`"),
        (
            "Id(0b100000000)",
            "constant value `256` does not fit `byte`",
        ),
    ] {
        assert_eq!(
            errors(&format!("{byte}public byte F() {{ return {call}; }}")),
            [message],
            "{call}"
        );
    }
    // A variable is not a constant: it still needs an explicit conversion.
    assert_eq!(
        errors(&format!(
            "{identity}public ulong F(int x) {{ return Identity(x); }}"
        )),
        ["expected `ulong`, found `int`"]
    );
    // Another primitive family never converts.
    assert_eq!(
        errors(&format!(
            "{identity}public ulong F() {{ return Identity(1.5); }}"
        )),
        ["expected `ulong`, found `float`"]
    );
    assert_eq!(
        errors(&format!(
            "{identity}public ulong F() {{ return Identity(\"1\"); }}"
        )),
        ["expected `ulong`, found `string`"]
    );
}

#[test]
fn overload_resolution_prefers_the_existing_implicit_conversion() {
    // `int` converts implicitly to `long`, so the constant conversion to `byte`
    // never competes with it.
    let compilation = compile_valid(
        "public long Pick(long value) { return 1L; }
         public long Pick(byte value) { return 2L; }
         public long Example() { return Pick(5); }",
    );
    let call = first_call_in(&compilation, "Example");
    let hir::ExpressionKind::Call { arguments, .. } = &call.kind else {
        panic!("a call");
    };
    assert_eq!(arguments[0].type_, hir::Type::Long);
    // An exact match still wins over every conversion.
    let compilation = compile_valid(
        "public long Pick(long value) { return 1L; }
         public long Pick(int value) { return 2L; }
         public long Example() { return Pick(5); }",
    );
    let call = first_call_in(&compilation, "Example");
    let hir::ExpressionKind::Call { arguments, .. } = &call.kind else {
        panic!("a call");
    };
    assert_eq!(arguments[0].type_, hir::Type::Int);
    // When only constant conversions apply, equally good candidates are ambiguous.
    assert_eq!(
        errors(
            "public long Pick(byte value) { return 1L; }
             public long Pick(ushort value) { return 2L; }
             public long Example() { return Pick(5); }"
        ),
        ["call to `Pick` is ambiguous"]
    );
    // A single constant-convertible candidate among several is chosen.
    compile_valid(
        "public long Pick(byte value) { return 1L; }
         public long Pick(string value) { return 2L; }
         public long Example() { return Pick(5); }",
    );
    assert_eq!(
        errors(
            "public long Pick(byte value) { return 1L; }
             public long Pick(string value) { return 2L; }
             public long Example() { return Pick(500); }"
        ),
        ["no overload of `Pick` accepts these arguments"]
    );
}

#[test]
fn evaluation_order_of_converted_arguments_is_unchanged() {
    let compilation = compile_valid(
        "public ulong Mark(ulong value) { return value; }
         public ulong Combine(ulong first, ulong second) { return first + second; }
         public ulong Example() { return Combine(Mark(1), Mark(2)); }",
    );
    let combine = first_call_in(&compilation, "Example");
    let hir::ExpressionKind::Call { arguments, .. } = &combine.kind else {
        panic!("a call");
    };
    assert_eq!(arguments.len(), 2);
    for argument in arguments {
        assert_eq!(argument.type_, hir::Type::ULong);
    }
    // MIR keeps the validated operand types for the call.
    let function = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Example")
        .expect("MIR function");
    let mut call_count = 0;
    for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
        if let mir::Instruction::Call { arguments, .. } = instruction {
            call_count += 1;
            for argument in arguments {
                assert_eq!(argument.type_, mir::Type::ULong, "{instruction:?}");
            }
        }
    }
    assert_eq!(call_count, 3);
}
