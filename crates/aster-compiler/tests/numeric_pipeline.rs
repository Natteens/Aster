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
            decimal g = 7M;
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
            hir::Type::Decimal,
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
    assert!(matches!(
        variables[6].initializer.as_ref().unwrap().kind,
        hir::ExpressionKind::Literal(hir::Literal::Decimal(ref value)) if value == "7"
    ));
}

#[test]
fn mir_preserves_new_numeric_local_types_and_constants() {
    let compilation = compile_valid(
        "public void Values() { sbyte a = 1; byte b = 2; short c = 3; ushort d = 4; uint e = 5U; ulong f = 6UL; decimal g = 7M; }",
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
            mir::Type::Decimal,
        ]
    );
    assert!(
        function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        type_: mir::Type::Decimal,
                        kind: mir::RvalueKind::Use(mir::Operand {
                            type_: mir::Type::Decimal,
                            kind: mir::OperandKind::Constant(mir::Constant::Decimal(value)),
                        }),
                    },
                    ..
                } if value == "7"
            ))
    );
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
