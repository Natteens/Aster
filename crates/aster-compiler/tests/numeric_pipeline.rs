use aster_compiler::{compile, hir, mir};

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    compile(source).unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}"))
}

fn messages(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

#[test]
fn hir_preserves_new_numeric_local_types_and_literals() {
    let compilation = compile_valid(
        r"
        public void Values()
        {
            sbyte a = 1;
            byte b = 2;
            short c = 3;
            ushort d = 4;
            uint e = 5U;
            ulong f = 6UL;
        }
        ",
    );
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("expected HIR function");
    };
    let variables: Vec<_> = function
        .body
        .as_ref()
        .expect("function body")
        .statements
        .iter()
        .map(|statement| {
            let hir::Statement::Variable(variable) = statement else {
                panic!("expected HIR variable");
            };
            variable
        })
        .collect();
    assert_eq!(
        variables
            .iter()
            .map(|variable| variable.type_.clone())
            .collect::<Vec<_>>(),
        [
            hir::Type::SByte,
            hir::Type::Byte,
            hir::Type::Short,
            hir::Type::UShort,
            hir::Type::UInt,
            hir::Type::ULong,
        ]
    );
    for variable in &variables {
        assert_eq!(
            variable.initializer.as_ref().expect("initializer").type_,
            variable.type_
        );
    }
    assert!(matches!(
        variables[4].initializer.as_ref().unwrap().kind,
        hir::ExpressionKind::Literal(hir::Literal::Integer(ref value)) if value == "5"
    ));
    assert!(matches!(
        variables[5].initializer.as_ref().unwrap().kind,
        hir::ExpressionKind::Literal(hir::Literal::Integer(ref value)) if value == "6"
    ));
}

#[test]
fn mir_preserves_new_numeric_local_types_and_constants() {
    let compilation = compile_valid(
        "public void Values() { sbyte a = 1; byte b = 2; short c = 3; ushort d = 4; uint e = 5U; ulong f = 6UL; }",
    );
    let function = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "Values")
        .expect("MIR function");
    let source_locals: Vec<_> = function
        .locals
        .iter()
        .filter(|local| !local.temporary)
        .map(|local| local.type_.clone())
        .collect();
    assert_eq!(
        source_locals,
        [
            mir::Type::SByte,
            mir::Type::Byte,
            mir::Type::Short,
            mir::Type::UShort,
            mir::Type::UInt,
            mir::Type::ULong,
        ]
    );
}

#[test]
fn decimal_is_rejected_before_hir_and_mir_are_built() {
    for source in [
        "public decimal Money() { return 1.25m; }",
        "public int Run() { decimal value = (decimal)1; return 0; }",
        "public int Run(decimal value) { return 0; }",
        "public class Money { public decimal value; } public int Run() { return 0; }",
        "public int Run() { decimal[] values = new decimal[1]; return 0; }",
        "public int Run() { List<decimal> values = new List<decimal>(); return 0; }",
        "public int Run() { Dictionary<int,decimal> values = new Dictionary<int,decimal>(); return 0; }",
        "public class Box<T> { public Box() {} } public int Run() { new Box<decimal>(); return 0; }",
        "public class Box<T> { public Box() {} } public int Run() { Box<List<decimal>> value = new Box<List<decimal>>(); return 0; }",
        "public enum Slot<T> { Empty, } public int Run() { Slot<decimal>.Empty; return 0; }",
        "public class Tools { public Tools() {} public T Keep<T>(T value) { return value; } } public int Run() { new Tools().Keep<decimal>(1); return 0; }",
        "public interface IBox<T> { T Get(); } public T Keep<T>(T value) where T : IBox<decimal> { return value; } public int Run() { return 0; }",
    ] {
        let diagnostics = compile(source).expect_err("decimal surface is deliberately deferred");
        assert!(diagnostics.iter().any(|diagnostic| {
            diagnostic
                .message
                .contains("`decimal` is reserved but not supported")
        }));
    }
}

#[test]
fn semantic_rejects_compound_assignment_narrowing() {
    let diagnostics =
        messages("public void Bad() { byte small = 1; int wide = 2; small += wide; }");
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("compound assignment would narrow `int` to `byte`"))
    );

    let diagnostics =
        messages("public void Bad() { uint small = 1U; long wide = 2L; small *= wide; }");
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("compound assignment would narrow `long` to `uint`"))
    );
}

#[test]
fn semantic_rejects_implicit_conversions_that_lose_range_or_sign() {
    for (source, expected) in [
        (
            "public void Bad() { int value = 1; short result = value; }",
            "expected `short`, found `int`",
        ),
        (
            "public void Bad() { int value = 1; uint result = value; }",
            "expected `uint`, found `int`",
        ),
        (
            "public void Bad() { ulong value = 1UL; long result = value; }",
            "expected `long`, found `ulong`",
        ),
        (
            "public void Bad() { double value = 1D; float result = value; }",
            "expected `float`, found `double`",
        ),
    ] {
        assert!(
            messages(source)
                .iter()
                .any(|message| message.contains(expected)),
            "missing `{expected}` for `{source}`"
        );
    }
}

#[test]
fn accepts_the_minimum_long_literal() {
    let compilation = compile_valid("public long Minimum() { return -9223372036854775808; }");
    let function = &compilation.mir.functions[0];
    assert_eq!(function.return_type, mir::Type::Long);
    assert!(matches!(
        function.blocks[0].terminator,
        mir::Terminator::Return(Some(mir::Operand {
            type_: mir::Type::Long,
            kind: mir::OperandKind::Constant(mir::Constant::Integer(ref value)),
        })) if value == "-9223372036854775808"
    ));
}

#[test]
fn constexpr_ulong_overflow_is_a_diagnostic_instead_of_a_panic() {
    let diagnostics = messages(
        "public void Bad() { const ulong TooLarge = 18446744073709551615UL * 18446744073709551615UL; }",
    );
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("constant expression overflows `ulong`"))
    );
}

fn returned_expression(compilation: &aster_compiler::Compilation, name: &str) -> hir::Expression {
    let function = compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("HIR function `{name}`"));
    function
        .body
        .as_ref()
        .expect("function body")
        .statements
        .iter()
        .find_map(|statement| match statement {
            hir::Statement::Return(Some(expression)) => Some(expression.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{name}` returns a value"))
}

fn assert_promoted_operand(operand: &hir::Expression, narrow: &hir::Type) {
    assert_eq!(operand.type_, hir::Type::Int);
    let hir::ExpressionKind::Convert { operand: inner } = &operand.kind else {
        panic!("small-integer operand must be converted to int, found {operand:?}");
    };
    assert_eq!(&inner.type_, narrow);
}

fn binary_operands(expression: &hir::Expression) -> (&hir::Expression, &hir::Expression) {
    let hir::ExpressionKind::Binary { left, right, .. } = &expression.kind else {
        panic!("expected a binary expression, found {expression:?}");
    };
    (left, right)
}

#[test]
fn hir_converts_small_integer_binary_operands_to_int() {
    let cases = [
        ("byte", hir::Type::Byte),
        ("sbyte", hir::Type::SByte),
        ("short", hir::Type::Short),
        ("ushort", hir::Type::UShort),
    ];
    for (name, narrow) in cases {
        let source = format!(
            "public int Arithmetic() {{ {name} a = 1; {name} b = 2; return a + b; }}
             public bool Comparison() {{ {name} a = 1; {name} b = 2; return a < b; }}
             public int Multiply() {{ {name} a = 1; {name} b = 2; return a * b; }}"
        );
        let compilation = compile_valid(&source);
        for function in ["Arithmetic", "Multiply"] {
            let expression = returned_expression(&compilation, function);
            assert_eq!(expression.type_, hir::Type::Int, "{name} {function}");
            let (left, right) = binary_operands(&expression);
            assert_promoted_operand(left, &narrow);
            assert_promoted_operand(right, &narrow);
        }
        let comparison = returned_expression(&compilation, "Comparison");
        assert_eq!(comparison.type_, hir::Type::Bool, "{name} comparison");
        let (left, right) = binary_operands(&comparison);
        assert_promoted_operand(left, &narrow);
        assert_promoted_operand(right, &narrow);
    }
}

#[test]
fn hir_promotes_mixed_small_integers_and_leaves_wide_and_nonnumeric_operands() {
    let compilation = compile_valid(
        r#"
        public int Mixed() { byte a = 1; short b = 2; return a + b; }
        public int Wide() { int a = 1; int b = 2; return a + b; }
        public long WideLong() { long a = 1L; long b = 2L; return a + b; }
        public string Text() { string a = "a"; string b = "b"; return a + b; }
        public bool Logic() { bool a = true; bool b = false; return a && b; }
        "#,
    );
    let mixed = returned_expression(&compilation, "Mixed");
    assert_eq!(mixed.type_, hir::Type::Int);
    let (left, right) = binary_operands(&mixed);
    assert_promoted_operand(left, &hir::Type::Byte);
    assert_promoted_operand(right, &hir::Type::Short);

    for (name, type_) in [
        ("Wide", hir::Type::Int),
        ("WideLong", hir::Type::Long),
        ("Text", hir::Type::String),
        ("Logic", hir::Type::Bool),
    ] {
        let expression = returned_expression(&compilation, name);
        assert_eq!(expression.type_, type_, "{name}");
        let (left, right) = binary_operands(&expression);
        for operand in [left, right] {
            assert!(
                !matches!(operand.kind, hir::ExpressionKind::Convert { .. }),
                "{name} operands must not gain a conversion: {operand:?}"
            );
        }
    }
}

#[test]
fn mir_small_integer_binary_operation_executes_at_int_width() {
    let compilation = compile_valid(
        "public int Add() { byte a = 200; byte b = 200; return a + b; }
         public bool Compare() { short a = 1; short b = 2; return a < b; }",
    );
    for (name, result) in [("Add", mir::Type::Int), ("Compare", mir::Type::Bool)] {
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("MIR function");
        let mut found = false;
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            let mir::Instruction::Assign { value, .. } = instruction else {
                continue;
            };
            let mir::RvalueKind::Binary { left, right, .. } = &value.kind else {
                continue;
            };
            found = true;
            assert_eq!(value.type_, result, "{name} result type");
            assert_eq!(left.type_, mir::Type::Int, "{name} left operand");
            assert_eq!(right.type_, mir::Type::Int, "{name} right operand");
        }
        assert!(found, "`{name}` keeps a MIR binary operation");
    }
}
