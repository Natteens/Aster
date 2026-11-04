//! Typing of hexadecimal and binary integer literals through semantic analysis
//! and HIR, from `docs/compiler/nondecimal-literal-decision.md`.

use aster_compiler::{Compilation, compile, hir};

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

/// The type the compiler gives the literal `literal` (via `var`).
fn literal_type(literal: &str) -> hir::Type {
    let compilation = compile_valid(&format!("public void F() {{ var x = {literal}; }}"));
    let hir::Item::Function(function) = &compilation.hir.items[0] else {
        panic!("a function");
    };
    let hir::Statement::Variable(variable) = &function.body.as_ref().expect("body").statements[0]
    else {
        panic!("a variable");
    };
    variable.type_.clone()
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
        .expect("function");
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
        .expect("return")
}

#[test]
fn unsuffixed_literals_have_the_first_fitting_type_in_both_radixes() {
    let cases: [(&str, &str, hir::Type); 8] = [
        (
            "0x7FFFFFFF",
            "0b1111111111111111111111111111111",
            hir::Type::Int,
        ),
        (
            "0x80000000",
            "0b10000000000000000000000000000000",
            hir::Type::UInt,
        ),
        (
            "0xFFFFFFFF",
            "0b11111111111111111111111111111111",
            hir::Type::UInt,
        ),
        (
            "0x100000000",
            "0b100000000000000000000000000000000",
            hir::Type::Long,
        ),
        (
            "0x7FFFFFFFFFFFFFFF",
            "0b111111111111111111111111111111111111111111111111111111111111111",
            hir::Type::Long,
        ),
        (
            "0x8000000000000000",
            "0b1000000000000000000000000000000000000000000000000000000000000000",
            hir::Type::ULong,
        ),
        (
            "0xFFFFFFFFFFFFFFFF",
            "0b1111111111111111111111111111111111111111111111111111111111111111",
            hir::Type::ULong,
        ),
        ("0x0", "0b0", hir::Type::Int),
    ];
    for (hexadecimal, binary, expected) in cases {
        assert_eq!(literal_type(hexadecimal), expected, "{hexadecimal}");
        assert_eq!(literal_type(binary), expected, "{binary}");
        assert_eq!(
            literal_type(&hexadecimal.to_lowercase().replace("0x", "0X")),
            expected
        );
    }
}

#[test]
fn suffixes_keep_their_decimal_literal_meaning() {
    for (literal, expected) in [
        ("0xFFU", hir::Type::UInt),
        ("0xFFFFFFFFu", hir::Type::UInt),
        ("0x100000000U", hir::Type::ULong),
        ("0b11U", hir::Type::UInt),
        ("0xFFL", hir::Type::Long),
        ("0x7FFFFFFFFFFFFFFFl", hir::Type::Long),
        ("0xFFUL", hir::Type::ULong),
        ("0xFFLU", hir::Type::ULong),
        ("0xFFuL", hir::Type::ULong),
        ("0xFFlu", hir::Type::ULong),
        ("0b101UL", hir::Type::ULong),
    ] {
        assert_eq!(literal_type(literal), expected, "{literal}");
    }
    // `L` keeps ASTER's rule: it must fit `long`, with no fallback to `ulong`.
    assert_eq!(
        errors("public void F() { var x = 0xFFFFFFFFFFFFFFFFL; }"),
        ["integer literal `18446744073709551615L` is out of range for `long`"]
    );
    assert_eq!(
        errors("public void F() { var x = 0x8000000000000000L; }"),
        ["integer literal `9223372036854775808L` is out of range for `long`"]
    );
}

#[test]
fn existing_conversions_apply_without_new_rules() {
    compile_valid(
        "public void F()
         {
             int mask = 0xFF;
             int flags = 0b1010;
             byte low = 0xFF;
             sbyte small = 0x7F;
             ushort word = 0xFFFF;
             uint high = 0x80000000;
             long wide = 0xFFFFFFFF;
             ulong all = 0xFFFFFFFFFFFFFFFFUL;
             ulong sameAll = 0xFFFFFFFFFFFFFFFF;
             ulong small64 = 0x1;
             double ratio = 0xFF;
             int mixed = (0x0F | 0b1010);
         }",
    );
    // A constant that does not fit is reported with the same existing message as
    // the equal decimal literal.
    for (declaration, decimal, message) in [
        (
            "int bad = 0x80000000;",
            "int bad = 2147483648;",
            "constant value `2147483648` does not fit `int`",
        ),
        (
            "int bad = 0xFFFFFFFF;",
            "int bad = 4294967295;",
            "constant value `4294967295` does not fit `int`",
        ),
        (
            "uint bad = 0x100000000;",
            "uint bad = 4294967296;",
            "constant value `4294967296` does not fit `uint`",
        ),
        (
            "sbyte bad = 0xFF;",
            "sbyte bad = 255;",
            "constant value `255` does not fit `sbyte`",
        ),
        (
            "byte bad = 0x100;",
            "byte bad = 256;",
            "constant value `256` does not fit `byte`",
        ),
        (
            "ushort bad = 0x10000;",
            "ushort bad = 65536;",
            "constant value `65536` does not fit `ushort`",
        ),
        (
            "byte bad = 0b100000000;",
            "byte bad = 256;",
            "constant value `256` does not fit `byte`",
        ),
    ] {
        assert_eq!(
            errors(&format!("public void F() {{ {declaration} }}")),
            [message],
            "{declaration}"
        );
        assert_eq!(
            errors(&format!("public void F() {{ {decimal} }}")),
            [message],
            "{decimal}"
        );
    }
    assert_eq!(
        errors("public void F() { long bad = 0x8000000000000000; }"),
        ["constant value `9223372036854775808` does not fit `long`"]
    );
    // A non-constant `uint` still does not convert to `int`.
    assert_eq!(
        errors("public void F() { uint high = 0x80000000; int bad = high; }"),
        ["expected `int`, found `uint`"]
    );
}

#[test]
fn negation_of_unsigned_prefixed_literals_stays_rejected() {
    for literal in [
        "0x80000000",
        "0xFFFFFFFF",
        "0x8000000000000000",
        "0xFFU",
        "0b11UL",
    ] {
        let found = if literal.ends_with('U') {
            "uint"
        } else if literal.ends_with("UL") || literal.len() > 12 {
            "ulong"
        } else {
            "uint"
        };
        assert_eq!(
            errors(&format!("public void F() {{ var x = -{literal}; }}")),
            [format!(
                "cannot negate a value of the unsigned type `{found}`"
            )],
            "-{literal}"
        );
    }
    // Signed prefixed literals negate normally.
    assert_eq!(literal_type("-0x7FFFFFFF"), hir::Type::Int);
    assert_eq!(literal_type("-0x100000000"), hir::Type::Long);
    assert_eq!(literal_type("-0x1"), hir::Type::Int);
}

#[test]
fn bit_patterns_are_reinterpreted_with_explicit_casts() {
    let compilation = compile_valid(
        "public int Min() { return (int)0x80000000; }
         public int AllOnes() { return (int)0xFFFFFFFF; }
         public long LongMin() { return (long)0x8000000000000000; }
         public sbyte Byte() { return (sbyte)0xFF; }
         public short Word() { return (short)0xFFFF; }
         public const int HighBit = (int)0x80000000;
         public const uint Mask = 0xFF00FF00;
         public const ulong Everything = 0xFFFFFFFFFFFFFFFF;
         public const long Pattern = (long)0xFFFFFFFFFFFFFFFF;",
    );
    for (name, type_) in [
        ("Min", hir::Type::Int),
        ("AllOnes", hir::Type::Int),
        ("LongMin", hir::Type::Long),
        ("Byte", hir::Type::SByte),
        ("Word", hir::Type::Short),
    ] {
        assert_eq!(returned(&compilation, name).type_, type_, "{name}");
    }
}

#[test]
fn prefixed_literals_follow_the_existing_binary_promotion() {
    // `uint` with `int` promotes to `long`; C# keeps `uint` by converting the constant.
    assert_eq!(literal_type("0xFFFFFFFF | 1"), hir::Type::Long);
    assert_eq!(literal_type("0xFFFFFFFF | 1U"), hir::Type::UInt);
    assert_eq!(literal_type("0x0F | 0b1010"), hir::Type::Int);
    assert_eq!(literal_type("0x1 << 31"), hir::Type::Int);
    assert_eq!(literal_type("0x80000000 >> 31"), hir::Type::UInt);
    assert_eq!(literal_type("~0xFFFFFFFF"), hir::Type::UInt);
    assert_eq!(
        literal_type("0xFFFFFFFFFFFFFFFF ^ 0xFFUL"),
        hir::Type::ULong
    );
    // No constant adaptation: an `int` literal does not join a `ulong` operand.
    for expression in [
        "0xFFFFFFFFFFFFFFFF | 0xFF",
        "0xFFFFFFFFFFFFFFFF | 255",
        "0x8000000000000000 | -1",
    ] {
        assert_eq!(
            errors(&format!("public void F() {{ var x = {expression}; }}")),
            ["`ulong` and `int` have no implicit common type"],
            "{expression}"
        );
    }
    assert_eq!(
        errors("public void F() { uint flags = 0; flags |= 0xFF; }"),
        ["compound assignment would narrow `long` to `uint`"]
    );
    compile_valid("public void F() { uint flags = 0; flags |= 0xFFU; }");
}

#[test]
fn decimal_literal_typing_is_unchanged() {
    assert_eq!(literal_type("2147483647"), hir::Type::Int);
    assert_eq!(literal_type("2147483648"), hir::Type::Long);
    assert_eq!(literal_type("4294967295"), hir::Type::Long);
    assert_eq!(literal_type("9223372036854775807"), hir::Type::Long);
    assert_eq!(literal_type("4294967295U"), hir::Type::UInt);
    assert_eq!(literal_type("4294967296U"), hir::Type::ULong);
    assert_eq!(literal_type("0"), hir::Type::Int);
    assert_eq!(literal_type("0.5"), hir::Type::Float);
    assert_eq!(literal_type("2.5d"), hir::Type::Double);
    assert_eq!(literal_type("-9223372036854775808"), hir::Type::Long);
    // A decimal value above `long` is still rejected without a suffix.
    assert_eq!(
        errors("public void F() { var x = 18446744073709551615; }"),
        ["integer literal `18446744073709551615` is out of range for `long`"]
    );
}

#[test]
fn malformed_literals_are_rejected_before_semantic_analysis() {
    for (source, message) in [
        ("var x = 0x;", "hexadecimal literal has no digits"),
        ("var x = 0b;", "binary literal has no digits"),
        ("var x = 0b102;", "invalid digit `2` in binary literal"),
        ("var x = 0xFG;", "invalid digit `G` in hexadecimal literal"),
        ("var x = 0xFFLL;", "invalid integer literal suffix `LL`"),
        ("var x = 0x1m;", "invalid digit `m` in hexadecimal literal"),
        (
            "var x = 0x10000000000000000;",
            "hexadecimal literal exceeds the range of `ulong`",
        ),
    ] {
        assert_eq!(
            errors(&format!("public void F() {{ {source} }}")),
            [message],
            "{source}"
        );
    }
}
