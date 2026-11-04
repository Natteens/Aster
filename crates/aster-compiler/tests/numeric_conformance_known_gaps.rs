//! Regression coverage for conditional expressions whose branches have equal
//! small-integer types, originally recorded here as a known gap.
//!
//! Semantic analysis types `c ? a : b` with two `byte` branches as `int`
//! (`promoted_numeric` in `semantic/general/expressions.rs`), which is
//! observable because `var x = c ? a : b; byte y = x;` is rejected with
//! "expected `byte`, found `int`". HIR lowering used to type the same expression
//! `byte`, because `conditional_type` in `hir_lowering/types.rs` returned the
//! shared branch type before consulting the promotion table.

use aster_compiler::{compile, hir, mir};

const SMALL_INTEGERS: [&str; 4] = ["sbyte", "byte", "short", "ushort"];

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    compile(source).unwrap_or_else(|diagnostics| panic!("valid source: {diagnostics:#?}\n{source}"))
}

fn function<'a>(compilation: &'a aster_compiler::Compilation, name: &str) -> &'a hir::Function {
    compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("HIR function `{name}`"))
}

fn returned_expression(compilation: &aster_compiler::Compilation, name: &str) -> hir::Expression {
    function(compilation, name)
        .body
        .as_ref()
        .expect("body")
        .statements
        .iter()
        .find_map(|statement| match statement {
            hir::Statement::Return(Some(expression)) => Some(expression.clone()),
            _ => None,
        })
        .expect("return statement")
}

/// The conditional itself must be typed `int` and both branches must be
/// explicit conversions from the original narrow type.
fn assert_promoted_conditional(expression: &hir::Expression, narrow: &hir::Type, context: &str) {
    assert!(
        matches!(expression.kind, hir::ExpressionKind::Conditional { .. }),
        "{context}: the conditional must not be wrapped in a conversion: {expression:?}"
    );
    assert_eq!(expression.type_, hir::Type::Int, "{context}");
    let hir::ExpressionKind::Conditional {
        when_true,
        when_false,
        ..
    } = &expression.kind
    else {
        unreachable!();
    };
    for branch in [when_true, when_false] {
        assert_eq!(branch.type_, hir::Type::Int, "{context}: branch type");
        let hir::ExpressionKind::Convert { operand } = &branch.kind else {
            panic!("{context}: branch must be an explicit conversion: {branch:?}");
        };
        assert_eq!(&operand.type_, narrow, "{context}: converted source");
    }
}

fn narrow_type(name: &str) -> hir::Type {
    match name {
        "sbyte" => hir::Type::SByte,
        "byte" => hir::Type::Byte,
        "short" => hir::Type::Short,
        "ushort" => hir::Type::UShort,
        other => panic!("unexpected type {other}"),
    }
}

#[test]
fn equal_small_integer_conditional_branches_are_typed_int_in_hir() {
    for name in SMALL_INTEGERS {
        let compilation = compile_valid(&format!(
            "public int Pick(bool c, {name} a, {name} b) {{ return c ? a : b; }}"
        ));
        assert_promoted_conditional(
            &returned_expression(&compilation, "Pick"),
            &narrow_type(name),
            name,
        );
    }
}

#[test]
fn conditional_initializers_and_nested_conditionals_use_the_promoted_type() {
    for name in SMALL_INTEGERS {
        let compilation = compile_valid(&format!(
            "public int Init(bool c, {name} a, {name} b) {{ var x = c ? a : b; return x; }}
             public int Nested(bool c, bool d, {name} a, {name} b) {{ return c ? (d ? a : b) : a; }}"
        ));
        // `var` takes the promoted type.
        let init = function(&compilation, "Init");
        let hir::Statement::Variable(variable) = &init.body.as_ref().expect("body").statements[0]
        else {
            panic!("{name}: expected a variable declaration");
        };
        assert_eq!(variable.type_, hir::Type::Int, "{name}: var type");
        assert_promoted_conditional(
            variable.initializer.as_ref().expect("initializer"),
            &narrow_type(name),
            name,
        );
        // The outer conditional promotes its (already promoted) inner branch
        // and its narrow branch; the inner conditional is typed `int` too.
        let outer = returned_expression(&compilation, "Nested");
        assert_eq!(outer.type_, hir::Type::Int, "{name}: outer");
        let hir::ExpressionKind::Conditional { when_true, .. } = &outer.kind else {
            panic!("{name}: expected a conditional: {outer:?}");
        };
        assert_eq!(when_true.type_, hir::Type::Int, "{name}: inner");
    }
}

#[test]
fn contextual_narrow_target_still_accepts_and_rejects_as_before() {
    // An explicit narrow declaration keeps accepting a narrow conditional
    // (semantic analysis uses the declared type as context)...
    for name in SMALL_INTEGERS {
        compile_valid(&format!(
            "public {name} Pick(bool c, {name} a, {name} b) {{ return c ? a : b; }}
             public {name} Init(bool c, {name} a, {name} b) {{ {name} x = c ? a : b; return x; }}"
        ));
        // ...while an unannotated conditional is `int` and cannot be narrowed implicitly.
        let diagnostics = compile(&format!(
            "public {name} Narrow(bool c, {name} a, {name} b) {{ var x = c ? a : b; return x; }}"
        ))
        .expect_err("implicit narrowing must be rejected");
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>(),
            [format!("expected `{name}`, found `int`")],
            "{name}"
        );
    }
}

#[test]
fn mixed_and_nonnumeric_conditionals_keep_their_types() {
    let compilation = compile_valid(
        "public interface IValue { int Get(); }
         public class Impl : IValue { public Impl() {} public int Get() { return 1; } }
         public int Mixed(bool c, byte a, short b) { return c ? a : b; }
         public long Wide(bool c, int a, uint b) { return c ? a : b; }
         public int Same(bool c, int a, int b) { return c ? a : b; }
         public bool Flag(bool c, bool a, bool b) { return c ? a : b; }
         public string Text(bool c, string a, string b) { return c ? a : b; }
         public IValue Mixed2(bool c, Impl a, IValue b) { return c ? a : b; }
         public Impl Objects(bool c, Impl a, Impl b) { return c ? a : b; }",
    );
    for (name, expected) in [
        ("Mixed", hir::Type::Int),
        ("Wide", hir::Type::Long),
        ("Same", hir::Type::Int),
        ("Flag", hir::Type::Bool),
        ("Text", hir::Type::String),
    ] {
        let expression = returned_expression(&compilation, name);
        assert_eq!(expression.type_, expected, "{name}");
        let hir::ExpressionKind::Conditional {
            when_true,
            when_false,
            ..
        } = &expression.kind
        else {
            panic!("{name}: expected a conditional: {expression:?}");
        };
        if matches!(name, "Same" | "Flag" | "Text") {
            for branch in [when_true, when_false] {
                assert!(
                    !matches!(branch.kind, hir::ExpressionKind::Convert { .. }),
                    "{name}: equal non-small types must not gain a conversion"
                );
            }
        }
    }
    assert!(matches!(
        returned_expression(&compilation, "Mixed2").type_,
        hir::Type::Interface(_)
    ));
    assert!(matches!(
        returned_expression(&compilation, "Objects").type_,
        hir::Type::Class(_)
    ));
}

#[test]
fn mir_selects_the_promoted_width_for_equal_small_integer_branches() {
    for name in SMALL_INTEGERS {
        let compilation = compile_valid(&format!(
            "public int Pick(bool c, {name} a, {name} b) {{ var x = c ? a : b; return x; }}"
        ));
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == "Pick")
            .expect("MIR function");
        let local = function
            .locals
            .iter()
            .find(|local| local.name == "x")
            .expect("local x");
        assert_eq!(local.type_, mir::Type::Int, "{name}: result local");
        // Every value copied into the result is already `int`.
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let mir::Instruction::Assign { target, value } = instruction else {
                continue;
            };
            if *target == mir::Place::Local(local.id) {
                assert_eq!(value.type_, mir::Type::Int, "{name}: assignment to x");
            }
        }
    }
}
