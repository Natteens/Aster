//! Semantic, HIR, and MIR evidence for the bitwise and shift operators.
//!
//! Expected typing is written from `docs/compiler/bitwise-shift-decision.md`
//! and `docs/specification/02-types.md`, not derived from `aster_types`.

use std::fmt::Write as _;

use aster_compiler::{Compilation, compile, hir, mir};

#[derive(Clone, Copy)]
struct Integer {
    name: &'static str,
    hir: fn() -> hir::Type,
    mir: fn() -> mir::Type,
    bits: u32,
    signed: bool,
}

/// Types are identified by name; the constructors are only used to build HIR and MIR types.
impl PartialEq for Integer {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for Integer {}

const INTEGERS: [Integer; 8] = [
    Integer {
        name: "sbyte",
        hir: || hir::Type::SByte,
        mir: || mir::Type::SByte,
        bits: 8,
        signed: true,
    },
    Integer {
        name: "byte",
        hir: || hir::Type::Byte,
        mir: || mir::Type::Byte,
        bits: 8,
        signed: false,
    },
    Integer {
        name: "short",
        hir: || hir::Type::Short,
        mir: || mir::Type::Short,
        bits: 16,
        signed: true,
    },
    Integer {
        name: "ushort",
        hir: || hir::Type::UShort,
        mir: || mir::Type::UShort,
        bits: 16,
        signed: false,
    },
    Integer {
        name: "int",
        hir: || hir::Type::Int,
        mir: || mir::Type::Int,
        bits: 32,
        signed: true,
    },
    Integer {
        name: "uint",
        hir: || hir::Type::UInt,
        mir: || mir::Type::UInt,
        bits: 32,
        signed: false,
    },
    Integer {
        name: "long",
        hir: || hir::Type::Long,
        mir: || mir::Type::Long,
        bits: 64,
        signed: true,
    },
    Integer {
        name: "ulong",
        hir: || hir::Type::ULong,
        mir: || mir::Type::ULong,
        bits: 64,
        signed: false,
    },
];

fn by_name(name: &str) -> Integer {
    *INTEGERS
        .iter()
        .find(|integer| integer.name == name)
        .expect("integer type")
}

/// An integer type holds every value of `from` (specification, "Implicit conversions").
fn holds(to: Integer, from: Integer) -> bool {
    if to == from {
        return true;
    }
    match (from.signed, to.signed) {
        (true, true) | (false, false) => to.bits >= from.bits,
        (false, true) => to.bits > from.bits,
        (true, false) => false,
    }
}

/// The common type of two integer operands ("Binary promotion").
fn common(left: Integer, right: Integer) -> Option<Integer> {
    if left.bits < 32 && right.bits < 32 {
        return Some(by_name("int"));
    }
    ["int", "uint", "long", "ulong"]
        .into_iter()
        .map(by_name)
        .find(|candidate| holds(*candidate, left) && holds(*candidate, right))
}

/// Unary promotion: small integers become `int`.
fn unary(integer: Integer) -> Integer {
    if integer.bits < 32 {
        by_name("int")
    } else {
        integer
    }
}

/// Types that implicitly convert to `int`, and so may be a shift count.
fn is_count(integer: Integer) -> bool {
    holds(by_name("int"), integer)
}

fn compile_valid(source: &str) -> Compilation {
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

fn returned(compilation: &Compilation, name: &str) -> hir::Expression {
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
        .expect("body")
        .statements
        .iter()
        .find_map(|statement| match statement {
            hir::Statement::Return(Some(expression)) => Some(expression.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("`{name}` returns a value"))
}

fn operands(expression: &hir::Expression) -> (&hir::Expression, &hir::Expression) {
    let hir::ExpressionKind::Binary { left, right, .. } = &expression.kind else {
        panic!("expected a binary expression: {expression:?}");
    };
    (left, right)
}

/// The operand has the target type; when that differs from `original` it is one conversion from it.
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
            "{context}: operand already of the target type must not be converted"
        );
    } else {
        let hir::ExpressionKind::Convert { operand: inner } = &operand.kind else {
            panic!("{context}: expected a conversion from {original:?}: {operand:?}");
        };
        assert_eq!(&inner.type_, original, "{context}: converted source");
    }
}

/// Every executable MIR binary operation in the module.
fn binary_operations<'a>(
    compilation: &'a Compilation,
    function: &str,
) -> Vec<(
    &'a mir::Rvalue,
    &'a mir::Operand,
    &'a mir::Operand,
    mir::BinaryOperator,
)> {
    let function = compilation
        .mir
        .functions
        .iter()
        .find(|candidate| candidate.name == function)
        .expect("MIR function");
    let mut operations = Vec::new();
    for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
        if let mir::Instruction::Assign { value, .. } = instruction
            && let mir::RvalueKind::Binary {
                left,
                operator,
                right,
            } = &value.kind
        {
            operations.push((value, left, right, *operator));
        }
    }
    operations
}

const LOGICAL: [(&str, &str, mir::BinaryOperator); 3] = [
    ("And", "&", mir::BinaryOperator::BitwiseAnd),
    ("Or", "|", mir::BinaryOperator::BitwiseOr),
    ("Xor", "^", mir::BinaryOperator::BitwiseXor),
];
const SHIFTS: [(&str, &str, mir::BinaryOperator); 3] = [
    ("Shl", "<<", mir::BinaryOperator::ShiftLeft),
    ("Shr", ">>", mir::BinaryOperator::ShiftRight),
    ("Ushr", ">>>", mir::BinaryOperator::UnsignedShiftRight),
];

#[test]
fn integer_bitwise_operators_follow_binary_promotion_in_every_layer() {
    for left in INTEGERS {
        for right in INTEGERS {
            let pair = format!("{} and {}", left.name, right.name);
            let Some(result) = common(left, right) else {
                let mut source = String::new();
                for (name, operator, _) in LOGICAL {
                    writeln!(
                        source,
                        "public bool {name}({} a, {} b) {{ var r = a {operator} b; return true; }}",
                        left.name, right.name
                    )
                    .expect("write");
                }
                let messages = diagnostics(&source);
                let expected = format!(
                    "`{}` and `{}` have no implicit common type",
                    left.name, right.name
                );
                assert_eq!(
                    messages,
                    [expected.clone(), expected.clone(), expected],
                    "{pair}"
                );
                continue;
            };
            let mut source = String::new();
            for (name, operator, _) in LOGICAL {
                writeln!(
                    source,
                    "public {} {name}({} a, {} b) {{ return a {operator} b; }}",
                    result.name, left.name, right.name
                )
                .expect("write");
            }
            let compilation = compile_valid(&source);
            for (name, _, operator) in LOGICAL {
                let context = format!("{pair} {name}");
                let expression = returned(&compilation, name);
                assert_eq!(expression.type_, (result.hir)(), "{context}: HIR result");
                let (l, r) = operands(&expression);
                assert_operand(l, &(left.hir)(), &(result.hir)(), &context);
                assert_operand(r, &(right.hir)(), &(result.hir)(), &context);
                let operations = binary_operations(&compilation, name);
                assert_eq!(operations.len(), 1, "{context}: one MIR operation");
                let (value, l, r, mir_operator) = operations[0];
                assert_eq!(mir_operator, operator, "{context}");
                assert_eq!(value.type_, (result.mir)(), "{context}: MIR result");
                assert_eq!(l.type_, (result.mir)(), "{context}: MIR left");
                assert_eq!(r.type_, (result.mir)(), "{context}: MIR right");
            }
            // Semantic analysis types the result exactly: a narrower declaration is rejected.
            let narrower = match result.name {
                "int" => Some("short"),
                "uint" => Some("ushort"),
                "long" => Some("int"),
                "ulong" => Some("uint"),
                _ => None,
            };
            if let Some(narrow) = narrower {
                let messages = diagnostics(&format!(
                    "public {narrow} N({} a, {} b) {{ return a & b; }}",
                    left.name, right.name
                ));
                assert_eq!(
                    messages,
                    [format!("expected `{narrow}`, found `{}`", result.name)],
                    "{pair}"
                );
            }
        }
    }
}

#[test]
fn shifts_promote_the_left_operand_and_keep_the_count_separate() {
    let counts = [
        "sbyte", "byte", "short", "ushort", "int", "uint", "long", "ulong",
    ];
    for left in INTEGERS {
        let result = unary(left);
        for count_name in counts {
            let count = by_name(count_name);
            let pair = format!("{} by {}", left.name, count.name);
            let mut source = String::new();
            for (name, operator, _) in SHIFTS {
                writeln!(
                    source,
                    "public {} {name}({} a, {} n) {{ return a {operator} n; }}",
                    result.name, left.name, count.name
                )
                .expect("write");
            }
            if !is_count(count) {
                let messages = diagnostics(&source);
                let expected = format!(
                    "shift count of type `{}` does not convert to `int`",
                    count.name
                );
                assert_eq!(
                    messages,
                    [expected.clone(), expected.clone(), expected],
                    "{pair}"
                );
                continue;
            }
            let compilation = compile_valid(&source);
            for (name, _, operator) in SHIFTS {
                let context = format!("{pair} {name}");
                let expression = returned(&compilation, name);
                // The result is the promoted left operand, never the common type with the count.
                assert_eq!(expression.type_, (result.hir)(), "{context}: HIR result");
                let (l, r) = operands(&expression);
                assert_operand(l, &(left.hir)(), &(result.hir)(), &context);
                // The count is converted to the promoted left type so MIR keeps one operand type.
                assert_operand(r, &(count.hir)(), &(result.hir)(), &context);
                let operations = binary_operations(&compilation, name);
                assert_eq!(operations.len(), 1, "{context}: one MIR operation");
                let (value, l, r, mir_operator) = operations[0];
                assert_eq!(mir_operator, operator, "{context}");
                assert_eq!(value.type_, (result.mir)(), "{context}: MIR result");
                assert_eq!(l.type_, (result.mir)(), "{context}: MIR left");
                assert_eq!(r.type_, (result.mir)(), "{context}: MIR right");
                // Only 32- and 64-bit operands ever reach the backend.
                assert!(result.bits >= 32);
            }
        }
    }
}

#[test]
fn shifts_and_bitwise_operators_reject_unsupported_operand_types() {
    for (left, right) in [
        ("float", "int"),
        ("int", "float"),
        ("double", "double"),
        ("string", "string"),
        ("char", "char"),
        ("bool", "int"),
        ("int", "bool"),
    ] {
        for operator in ["&", "|", "^"] {
            let messages = diagnostics(&format!(
                "public int F({left} a, {right} b) {{ return a {operator} b; }}"
            ));
            assert_eq!(
                messages,
                [format!(
                    "binary operator is not valid for `{left}` and `{right}`"
                )],
                "{left} {operator} {right}"
            );
        }
    }
    for (left, right) in [
        ("float", "int"),
        ("string", "int"),
        ("char", "int"),
        ("bool", "int"),
        ("double", "int"),
    ] {
        for operator in ["<<", ">>", ">>>"] {
            let messages = diagnostics(&format!(
                "public int F({left} a, {right} b) {{ return a {operator} b; }}"
            ));
            assert_eq!(
                messages,
                [format!(
                    "binary operator is not valid for `{left}` and `{right}`"
                )],
                "{left} {operator} {right}"
            );
        }
    }
    for (left, right) in [
        ("int", "float"),
        ("int", "bool"),
        ("int", "char"),
        ("int", "string"),
    ] {
        let messages = diagnostics(&format!(
            "public int F({left} a, {right} b) {{ return a << b; }}"
        ));
        assert_eq!(
            messages,
            [format!(
                "binary operator is not valid for `{left}` and `{right}`"
            )],
            "{left} << {right}"
        );
    }
}

#[test]
fn complement_promotes_small_integers_and_rejects_everything_else() {
    for integer in INTEGERS {
        let result = unary(integer);
        let compilation = compile_valid(&format!(
            "public {} Not({} a) {{ return ~a; }}",
            result.name, integer.name
        ));
        let expression = returned(&compilation, "Not");
        assert_eq!(expression.type_, (result.hir)(), "{}", integer.name);
        let hir::ExpressionKind::Unary { operator, operand } = &expression.kind else {
            panic!("expected a unary expression: {expression:?}");
        };
        assert_eq!(*operator, hir::UnaryOperator::BitwiseNot);
        assert_operand(operand, &(integer.hir)(), &(result.hir)(), integer.name);
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == "Not")
            .expect("MIR function");
        let mut found = false;
        for instruction in function.blocks.iter().flat_map(|block| &block.instructions) {
            if let mir::Instruction::Assign { value, .. } = instruction
                && let mir::RvalueKind::Unary {
                    operator: mir::UnaryOperator::BitwiseNot,
                    operand,
                } = &value.kind
            {
                found = true;
                assert_eq!(value.type_, (result.mir)(), "{}", integer.name);
                assert_eq!(operand.type_, (result.mir)(), "{}", integer.name);
            }
        }
        assert!(found, "{}: MIR keeps the complement", integer.name);
        if result.name != integer.name {
            let messages = diagnostics(&format!(
                "public {} N({} a) {{ return ~a; }}",
                integer.name, integer.name
            ));
            assert_eq!(
                messages,
                [format!("expected `{}`, found `int`", integer.name)]
            );
        }
    }
    for other in ["bool", "float", "double", "char", "string"] {
        assert_eq!(
            diagnostics(&format!("public int F({other} a) {{ return ~a; }}")),
            [format!("unary operator is not valid for `{other}`")],
            "{other}"
        );
    }
}

#[test]
fn eager_boolean_bitwise_operators_do_not_short_circuit() {
    let compilation = compile_valid(
        "public bool F(bool a, bool b) { return a & b; }
         public bool G(bool a, bool b) { return a | b; }
         public bool H(bool a, bool b) { return a ^ b; }
         public bool S(bool a, bool b) { return a && b; }",
    );
    for name in ["F", "G", "H"] {
        assert_eq!(returned(&compilation, name).type_, hir::Type::Bool);
        // A plain binary operation in one block: nothing branches around the right operand.
        let function = compilation
            .mir
            .functions
            .iter()
            .find(|function| function.name == name)
            .expect("MIR function");
        assert_eq!(function.blocks.len(), 1, "{name} is straight-line");
        let operations = binary_operations(&compilation, name);
        assert_eq!(operations.len(), 1, "{name}");
        assert_eq!(operations[0].0.type_, mir::Type::Bool);
    }
    let short_circuit = compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == "S")
        .expect("MIR function");
    assert!(short_circuit.blocks.len() > 1, "`&&` still branches");
    for (left, right) in [("bool", "int"), ("int", "bool")] {
        assert_eq!(
            diagnostics(&format!(
                "public bool F({left} a, {right} b) {{ return a & b; }}"
            )),
            [format!(
                "binary operator is not valid for `{left}` and `{right}`"
            )]
        );
    }
}

#[test]
fn compound_bitwise_assignment_requires_the_operation_type_to_equal_the_target() {
    for target in INTEGERS {
        for value in INTEGERS {
            for operator in ["&=", "|=", "^="] {
                let text = format!(
                    "public {0} Update({0} a, {1} b) {{ a {operator} b; return a; }}",
                    target.name, value.name
                );
                let context = format!("{} {operator} {}", target.name, value.name);
                match common(target, value) {
                    Some(operation) if operation == target => {
                        compile_valid(&text);
                    }
                    Some(operation) => {
                        assert_eq!(
                            diagnostics(&text),
                            [format!(
                                "compound assignment would narrow `{}` to `{}`",
                                operation.name, target.name
                            )],
                            "{context}"
                        );
                    }
                    None => {
                        assert_eq!(
                            diagnostics(&text),
                            [format!(
                                "`{}` and `{}` have no implicit common type",
                                target.name, value.name
                            )],
                            "{context}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn compound_shift_assignment_uses_the_promoted_target_and_an_int_count() {
    for target in INTEGERS {
        for count in INTEGERS {
            for operator in ["<<=", ">>=", ">>>="] {
                let text = format!(
                    "public {0} Update({0} a, {1} n) {{ a {operator} n; return a; }}",
                    target.name, count.name
                );
                let context = format!("{} {operator} {}", target.name, count.name);
                if !is_count(count) {
                    assert_eq!(
                        diagnostics(&text),
                        [format!(
                            "shift count of type `{}` does not convert to `int`",
                            count.name
                        )],
                        "{context}"
                    );
                } else if unary(target) == target {
                    compile_valid(&text);
                } else {
                    assert_eq!(
                        diagnostics(&text),
                        [format!(
                            "compound assignment would narrow `int` to `{}`",
                            target.name
                        )],
                        "{context}"
                    );
                }
            }
        }
    }
}

#[test]
fn compound_boolean_and_invalid_operand_assignments_are_controlled_errors() {
    for operator in ["&=", "|=", "^="] {
        compile_valid(&format!(
            "public bool F(bool a, bool b) {{ a {operator} b; return a; }}"
        ));
    }
    for (target, value) in [
        ("bool", "int"),
        ("int", "bool"),
        ("int", "float"),
        ("float", "int"),
        ("string", "string"),
        ("char", "char"),
    ] {
        for operator in ["&=", "|=", "^=", "<<=", ">>=", ">>>="] {
            let messages = diagnostics(&format!(
                "public void F({target} a, {value} b) {{ a {operator} b; }}"
            ));
            assert_eq!(
                messages,
                [format!(
                    "compound assignment is not valid for `{target}` and `{value}`"
                )],
                "{target} {operator} {value}"
            );
        }
    }
}

#[test]
fn compound_forms_work_on_fields_array_elements_and_list_elements() {
    let compilation = compile_valid(
        "public class Holder
         {
             public int Field;
             public ulong Wide;
             public Holder() {}
         }
         public int F(Holder h, int[] a, List<int> l)
         {
             h.Field &= 6; h.Field |= 1; h.Field ^= 3; h.Field <<= 2; h.Field >>= 1; h.Field >>>= 1;
             h.Wide <<= 40; h.Wide >>>= 8;
             a[0] |= 4; a[1] <<= 3;
             l[0] ^= 5; l[1] >>= 1;
             return h.Field;
         }",
    );
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .any(|function| function.name == "F")
    );
}

#[test]
fn constant_expressions_accept_the_new_operators() {
    let compilation = compile_valid(
        "public const int HighBit = 1 << 31;
         public const int Mask = (1 << 4) - 1;
         public const int Flags = 1 | 4;
         public const ulong Wide = 1UL << 63;
         public const bool Both = true & false;
         public int Use() { return HighBit + Mask + Flags; }",
    );
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .any(|function| function.name == "Use")
    );
    // Constant bitwise and shift evaluation never reports overflow.
    compile_valid(
        "public const int A = 1 << 40; public const long B = 1L << 70; public const uint C = ~0U;",
    );
}
