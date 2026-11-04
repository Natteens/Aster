//! Numeric conformance across semantic validation, HIR, and MIR.
//!
//! The expected tables below are written from `docs/specification/02-types.md`
//! ("Implicit conversions" and "Binary promotion"), not derived from
//! `aster_types::promote`, so a disagreement between the central rule and a
//! lowering layer is observable. Semantic analysis is private to the compiler;
//! its result type is observed through the public interface by checking which
//! return types the declaration accepts.

use std::fmt::Write as _;

use aster_compiler::{compile, hir, mir};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Signed(u32),
    Unsigned(u32),
    Float(u32),
}

struct Numeric {
    name: &'static str,
    hir: hir::Type,
    mir: mir::Type,
    kind: Kind,
}

fn numerics() -> Vec<Numeric> {
    vec![
        Numeric {
            name: "sbyte",
            hir: hir::Type::SByte,
            mir: mir::Type::SByte,
            kind: Kind::Signed(8),
        },
        Numeric {
            name: "byte",
            hir: hir::Type::Byte,
            mir: mir::Type::Byte,
            kind: Kind::Unsigned(8),
        },
        Numeric {
            name: "short",
            hir: hir::Type::Short,
            mir: mir::Type::Short,
            kind: Kind::Signed(16),
        },
        Numeric {
            name: "ushort",
            hir: hir::Type::UShort,
            mir: mir::Type::UShort,
            kind: Kind::Unsigned(16),
        },
        Numeric {
            name: "int",
            hir: hir::Type::Int,
            mir: mir::Type::Int,
            kind: Kind::Signed(32),
        },
        Numeric {
            name: "uint",
            hir: hir::Type::UInt,
            mir: mir::Type::UInt,
            kind: Kind::Unsigned(32),
        },
        Numeric {
            name: "long",
            hir: hir::Type::Long,
            mir: mir::Type::Long,
            kind: Kind::Signed(64),
        },
        Numeric {
            name: "ulong",
            hir: hir::Type::ULong,
            mir: mir::Type::ULong,
            kind: Kind::Unsigned(64),
        },
        Numeric {
            name: "float",
            hir: hir::Type::Float,
            mir: mir::Type::Float,
            kind: Kind::Float(32),
        },
        Numeric {
            name: "double",
            hir: hir::Type::Double,
            mir: mir::Type::Double,
            kind: Kind::Float(64),
        },
    ]
}

fn index_of(name: &str) -> usize {
    numerics()
        .iter()
        .position(|numeric| numeric.name == name)
        .unwrap_or_else(|| panic!("unknown numeric type {name}"))
}

/// "Implicit conversions" in the specification: the target must represent
/// every source value exactly.
fn implicit(from: &Numeric, to: &Numeric) -> bool {
    use Kind::{Float, Signed, Unsigned};
    if from.name == to.name {
        return true;
    }
    match (from.kind, to.kind) {
        (Signed(source), Signed(target))
        | (Unsigned(source), Unsigned(target))
        | (Float(source), Float(target)) => target >= source,
        (Unsigned(source), Signed(target)) => target > source,
        (Signed(_), Unsigned(_)) | (Float(_), Signed(_) | Unsigned(_)) => false,
        (Signed(source) | Unsigned(source), Float(32)) => source <= 16,
        (Signed(source) | Unsigned(source), Float(_)) => source <= 32,
    }
}

/// "Binary promotion" in the specification.
fn promoted(left: &Numeric, right: &Numeric) -> Option<usize> {
    let all = numerics();
    let candidates: &[&str] = match (left.kind, right.kind) {
        (Kind::Float(_), _) | (_, Kind::Float(_)) => &["float", "double"],
        (Kind::Signed(l) | Kind::Unsigned(l), Kind::Signed(r) | Kind::Unsigned(r))
            if l < 32 && r < 32 =>
        {
            return Some(index_of("int"));
        }
        _ => &["int", "uint", "long", "ulong"],
    };
    candidates
        .iter()
        .map(|name| index_of(name))
        .find(|&candidate| implicit(left, &all[candidate]) && implicit(right, &all[candidate]))
}

/// The next narrower type of the same family, used to prove that semantic
/// analysis does not type an operation narrower than the specification.
fn narrower(name: &str) -> Option<&'static str> {
    match name {
        "int" => Some("short"),
        "uint" => Some("ushort"),
        "long" => Some("int"),
        "ulong" => Some("uint"),
        "double" => Some("float"),
        _ => None,
    }
}

fn compile_valid(source: &str) -> aster_compiler::Compilation {
    compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected valid source: {diagnostics:#?}\n{source}"))
}

fn diagnostics(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source should be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
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

fn binary_operands(expression: &hir::Expression) -> (&hir::Expression, &hir::Expression) {
    let hir::ExpressionKind::Binary { left, right, .. } = &expression.kind else {
        panic!("expected a binary expression, found {expression:?}");
    };
    (left, right)
}

/// An operand already of the promoted type is untouched; any other operand is
/// wrapped in exactly one conversion from its declared type.
fn assert_operand(
    operand: &hir::Expression,
    original: &hir::Type,
    target: &hir::Type,
    context: &str,
) {
    assert_eq!(&operand.type_, target, "{context}: operand type");
    if original == target {
        assert!(
            !matches!(operand.kind, hir::ExpressionKind::Convert { .. }),
            "{context}: operand of the promoted type must not be converted: {operand:?}"
        );
    } else {
        let hir::ExpressionKind::Convert { operand: inner } = &operand.kind else {
            panic!("{context}: expected a conversion from {original:?}: {operand:?}");
        };
        assert_eq!(&inner.type_, original, "{context}: converted source type");
    }
}

const ARITHMETIC: [(&str, &str); 4] = [("Add", "+"), ("Sub", "-"), ("Mul", "*"), ("Div", "/")];
const COMPARISON: [(&str, &str); 6] = [
    ("Lt", "<"),
    ("Le", "<="),
    ("Gt", ">"),
    ("Ge", ">="),
    ("Eq", "=="),
    ("Ne", "!="),
];

fn is_integer(numeric: &Numeric) -> bool {
    !matches!(numeric.kind, Kind::Float(_))
}

#[test]
fn promotion_matrix_agrees_across_semantic_hir_and_mir() {
    let all = numerics();
    for left in &all {
        for right in &all {
            let pair = format!("{} and {}", left.name, right.name);
            let Some(index) = promoted(left, right) else {
                continue;
            };
            let target = &all[index];
            let mut source = String::new();
            for (name, operator) in ARITHMETIC {
                writeln!(
                    source,
                    "public {} {name}({} a, {} b) {{ return a {operator} b; }}",
                    target.name, left.name, right.name
                )
                .expect("writing to a String cannot fail");
            }
            if is_integer(left) && is_integer(right) {
                writeln!(
                    source,
                    "public {} Rem({} a, {} b) {{ return a % b; }}",
                    target.name, left.name, right.name
                )
                .expect("writing to a String cannot fail");
            }
            for (name, operator) in COMPARISON {
                writeln!(
                    source,
                    "public bool {name}({} a, {} b) {{ return a {operator} b; }}",
                    left.name, right.name
                )
                .expect("writing to a String cannot fail");
            }
            // Accepting the promoted return type and every wider type is the
            // semantic result; the HIR and MIR must match it exactly.
            let compilation = compile_valid(&source);
            let mut names: Vec<&str> = ARITHMETIC.iter().map(|(name, _)| *name).collect();
            if is_integer(left) && is_integer(right) {
                names.push("Rem");
            }
            for name in names {
                let context = format!("{pair} {name}");
                let expression = returned_expression(&compilation, name);
                assert_eq!(expression.type_, target.hir, "{context}: HIR result type");
                let (l, r) = binary_operands(&expression);
                assert_operand(l, &left.hir, &target.hir, &context);
                assert_operand(r, &right.hir, &target.hir, &context);
            }
            for (name, _) in COMPARISON {
                let context = format!("{pair} {name}");
                let expression = returned_expression(&compilation, name);
                assert_eq!(
                    expression.type_,
                    hir::Type::Bool,
                    "{context}: HIR result type"
                );
                let (l, r) = binary_operands(&expression);
                assert_operand(l, &left.hir, &target.hir, &context);
                assert_operand(r, &right.hir, &target.hir, &context);
            }
            // MIR: every executable binary operation works on the promoted
            // type, and every comparison operates on promoted operands.
            for function in &compilation.mir.functions {
                for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
                    let mir::Instruction::Assign { value, .. } = instruction else {
                        continue;
                    };
                    let mir::RvalueKind::Binary {
                        left: l, right: r, ..
                    } = &value.kind
                    else {
                        continue;
                    };
                    let context = format!("{pair} MIR {}", function.name);
                    assert_eq!(l.type_, target.mir, "{context}: left operand");
                    assert_eq!(r.type_, target.mir, "{context}: right operand");
                    let expected_result =
                        if COMPARISON.iter().any(|(name, _)| *name == function.name) {
                            mir::Type::Bool
                        } else {
                            target.mir.clone()
                        };
                    assert_eq!(value.type_, expected_result, "{context}: result");
                }
            }
            // Semantic analysis must not type the operation narrower.
            if let Some(narrow) = narrower(target.name) {
                let source = format!(
                    "public {narrow} Narrow({} a, {} b) {{ return a + b; }}",
                    left.name, right.name
                );
                let messages = diagnostics(&source);
                let expected = format!("expected `{narrow}`, found `{}`", target.name);
                assert!(
                    messages.contains(&expected),
                    "{pair}: semantic result must be `{}`: {messages:?}",
                    target.name
                );
            }
        }
    }
}

#[test]
fn pairs_without_a_safe_common_type_are_rejected_for_every_operator() {
    let all = numerics();
    let mut rejected = 0;
    for left in &all {
        for right in &all {
            if promoted(left, right).is_some() {
                continue;
            }
            rejected += 1;
            let mut source = String::new();
            for (name, operator) in ARITHMETIC.iter().chain(COMPARISON.iter()) {
                writeln!(
                    source,
                    "public bool {name}({} a, {} b) {{ var r = a {operator} b; return true; }}",
                    left.name, right.name
                )
                .expect("writing to a String cannot fail");
            }
            let messages = diagnostics(&source);
            assert_eq!(
                messages.len(),
                ARITHMETIC.len() + COMPARISON.len(),
                "{} and {}: one diagnostic per operator: {messages:?}",
                left.name,
                right.name
            );
            let expected = format!(
                "`{}` and `{}` have no implicit common type",
                left.name, right.name
            );
            assert!(
                messages.iter().all(|message| *message == expected),
                "{} and {}: {messages:?}",
                left.name,
                right.name
            );
        }
    }
    // long/ulong with floating point, and ulong with every signed integer.
    assert!(rejected > 0);
}

#[test]
fn specification_promotion_examples_hold() {
    let all = numerics();
    let result = |left: &str, right: &str| {
        promoted(&all[index_of(left)], &all[index_of(right)]).map(|i| all[i].name)
    };
    assert_eq!(result("byte", "byte"), Some("int"));
    assert_eq!(result("sbyte", "ushort"), Some("int"));
    assert_eq!(result("short", "short"), Some("int"));
    assert_eq!(result("int", "uint"), Some("long"));
    assert_eq!(result("uint", "ulong"), Some("ulong"));
    assert_eq!(result("int", "float"), Some("double"));
    assert_eq!(result("short", "float"), Some("float"));
    assert_eq!(result("float", "double"), Some("double"));
    assert_eq!(result("long", "float"), None);
    assert_eq!(result("ulong", "int"), None);
    assert_eq!(result("ulong", "sbyte"), None);
}

fn is_signed_or_float(numeric: &Numeric) -> bool {
    matches!(numeric.kind, Kind::Signed(_) | Kind::Float(_))
}

fn narrower_signed(name: &str) -> Option<&'static str> {
    match name {
        "short" => Some("sbyte"),
        other => narrower(other),
    }
}

#[test]
fn unary_negation_keeps_the_operand_type_and_rejects_unsigned_operands() {
    for numeric in numerics() {
        if is_signed_or_float(&numeric) {
            let source = format!("public {0} Negate({0} a) {{ return -a; }}", numeric.name);
            let compilation = compile_valid(&source);
            let expression = returned_expression(&compilation, "Negate");
            assert_eq!(expression.type_, numeric.hir, "{} negation", numeric.name);
            let hir::ExpressionKind::Unary { operand, .. } = &expression.kind else {
                panic!("expected a unary expression: {expression:?}");
            };
            assert_eq!(operand.type_, numeric.hir);
            assert!(!matches!(operand.kind, hir::ExpressionKind::Convert { .. }));
            // Semantic analysis keeps the operand type: a narrower declared
            // result is rejected.
            if let Some(narrow) = narrower_signed(numeric.name) {
                let messages = diagnostics(&format!(
                    "public {narrow} Negate({0} a) {{ return -a; }}",
                    numeric.name
                ));
                assert_eq!(
                    messages,
                    [format!("expected `{narrow}`, found `{}`", numeric.name)],
                    "{}",
                    numeric.name
                );
            }
        } else {
            let messages = diagnostics(&format!(
                "public {0} Negate({0} a) {{ return -a; }}",
                numeric.name
            ));
            assert_eq!(
                messages,
                [format!(
                    "cannot negate a value of the unsigned type `{}`",
                    numeric.name
                )]
            );
        }
    }
    let compilation = compile_valid("public bool Flip(bool a) { return !a; }");
    assert_eq!(
        returned_expression(&compilation, "Flip").type_,
        hir::Type::Bool
    );
    assert_eq!(
        diagnostics("public int Flip(int a) { return !a; }").len(),
        1
    );
}

#[test]
fn explicit_numeric_casts_lower_to_one_conversion_of_the_declared_target() {
    let all = numerics();
    for source in &all {
        let mut text = String::new();
        for target in &all {
            writeln!(
                text,
                "public {1} To_{1}({0} a) {{ return ({1})a; }}",
                source.name, target.name
            )
            .expect("writing to a String cannot fail");
        }
        let compilation = compile_valid(&text);
        for target in &all {
            let context = format!("({}){}", target.name, source.name);
            let expression = returned_expression(&compilation, &format!("To_{}", target.name));
            assert_eq!(expression.type_, target.hir, "{context}");
            if source.name != target.name {
                let hir::ExpressionKind::Convert { operand } = &expression.kind else {
                    panic!("{context}: expected a conversion: {expression:?}");
                };
                assert_eq!(operand.type_, source.hir, "{context}: source type");
            }
        }
    }
}

#[test]
fn implicit_conversions_follow_the_specification_for_every_pair() {
    let all = numerics();
    for target in &all {
        for source in &all {
            let text = format!(
                "public {0} Assign({1} b) {{ {0} a = b; return a; }}",
                target.name, source.name
            );
            let context = format!("{} to {}", source.name, target.name);
            if implicit(source, target) {
                let compilation = compile_valid(&text);
                let function = compilation
                    .hir
                    .items
                    .iter()
                    .find_map(|item| match item {
                        hir::Item::Function(function) if function.name == "Assign" => {
                            Some(function)
                        }
                        _ => None,
                    })
                    .expect("function");
                let hir::Statement::Variable(variable) =
                    &function.body.as_ref().expect("body").statements[0]
                else {
                    panic!("{context}: expected a variable declaration");
                };
                assert_eq!(variable.type_, target.hir, "{context}");
                let initializer = variable.initializer.as_ref().expect("initializer");
                assert_eq!(initializer.type_, target.hir, "{context}: initializer type");
            } else {
                let messages = diagnostics(&text);
                assert_eq!(
                    messages,
                    [format!(
                        "expected `{}`, found `{}`",
                        target.name, source.name
                    )],
                    "{context}"
                );
            }
        }
    }
}

#[test]
fn compound_assignment_requires_the_promoted_type_to_equal_the_target() {
    let all = numerics();
    for target in &all {
        for value in &all {
            // `%=` is only a proposal in the specification and is not parsed.
            let operators = ["+=", "-=", "*=", "/="];
            let allowed =
                promoted(target, value).is_some_and(|index| all[index].name == target.name);
            for operator in operators {
                let text = format!(
                    "public {0} Update({0} a, {1} b) {{ a {operator} b; return a; }}",
                    target.name, value.name
                );
                let context = format!("{} {operator} {}", target.name, value.name);
                if allowed {
                    compile_valid(&text);
                } else {
                    let messages = diagnostics(&text);
                    assert_eq!(messages.len(), 1, "{context}: {messages:?}");
                    assert!(
                        messages[0].contains("would narrow")
                            || messages[0].contains("requires compatible operands"),
                        "{context}: {messages:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn conditional_expressions_promote_unequal_numeric_branches() {
    let all = numerics();
    for left in &all {
        for right in &all {
            let pair = format!("{} and {}", left.name, right.name);
            let Some(index) = promoted(left, right) else {
                let messages = diagnostics(&format!(
                    "public bool Pick(bool c, {} a, {} b) {{ var x = c ? a : b; return true; }}",
                    left.name, right.name
                ));
                assert_eq!(
                    messages,
                    [format!(
                        "`?:` branches have incompatible types `{}` and `{}`",
                        left.name, right.name
                    )],
                    "{pair}"
                );
                continue;
            };
            let target = &all[index];
            let compilation = compile_valid(&format!(
                "public {2} Pick(bool c, {0} a, {1} b) {{ return c ? a : b; }}",
                left.name, right.name, target.name
            ));
            let expression = returned_expression(&compilation, "Pick");
            assert_eq!(expression.type_, target.hir, "{pair}");
            let hir::ExpressionKind::Conditional {
                when_true,
                when_false,
                ..
            } = &expression.kind
            else {
                panic!("{pair}: expected a conditional: {expression:?}");
            };
            assert_operand(when_true, &left.hir, &target.hir, &pair);
            assert_operand(when_false, &right.hir, &target.hir, &pair);
            // Semantic analysis types the unannotated conditional as the
            // promoted type: a narrower declared result is rejected.
            if let Some(narrow) = narrower(target.name) {
                let messages = diagnostics(&format!(
                    "public {narrow} Pick(bool c, {} a, {} b) {{ var x = c ? a : b; return x; }}",
                    left.name, right.name
                ));
                assert_eq!(
                    messages,
                    [format!("expected `{narrow}`, found `{}`", target.name)],
                    "{pair}"
                );
            }
        }
    }
}

#[test]
fn constant_overflow_is_a_compile_time_diagnostic() {
    for (declaration, message) in [
        (
            "public const int X = 2147483647 + 1;",
            "constant expression overflows `int`",
        ),
        (
            "public const uint X = 0U - 1U;",
            "constant expression overflows `uint`",
        ),
        (
            "public const long X = 9223372036854775807L * 2L;",
            "constant expression overflows `long`",
        ),
        (
            "public const ulong X = 18446744073709551615UL + 1UL;",
            "constant expression overflows `ulong`",
        ),
        (
            "public const int X = 10 / 0;",
            "constant expression divides by zero",
        ),
    ] {
        assert_eq!(diagnostics(declaration), [message], "{declaration}");
    }
    // Explicit narrowing of a constant wraps instead of overflowing.
    compile_valid("public const int X = (int)3000000000L;");
}

#[test]
fn compound_remainder_assignment_is_not_part_of_the_language_yet() {
    // `docs/specification/04-expressions.md` lists `%=` only as proposed.
    assert_eq!(
        diagnostics("public int R(int a, int b) { a %= b; return a; }").len(),
        1
    );
}
