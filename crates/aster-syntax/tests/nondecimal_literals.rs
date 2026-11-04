use aster_diagnostics::Span;
use aster_syntax::{TokenKind, lex, parse};

fn tokens(source: &str) -> Vec<(TokenKind, Span)> {
    lex(source)
        .unwrap_or_else(|diagnostics| panic!("`{source}` should lex: {diagnostics:#?}"))
        .into_iter()
        .map(|token| (token.kind, token.span))
        .take_while(|(kind, _)| *kind != TokenKind::Eof)
        .collect()
}

/// The single literal token of `source`, checking it spans the whole source.
fn literal(source: &str) -> TokenKind {
    let tokens = tokens(source);
    assert_eq!(tokens.len(), 1, "`{source}` is one token: {tokens:?}");
    assert_eq!(
        tokens[0].1,
        Span::new(0, source.len()),
        "span of `{source}`"
    );
    tokens[0].0.clone()
}

fn error(source: &str) -> (String, Option<String>, Span) {
    let diagnostics = lex(source).expect_err("source should be rejected");
    assert_eq!(
        diagnostics.len(),
        1,
        "one diagnostic for `{source}`: {diagnostics:#?}"
    );
    let diagnostic = &diagnostics[0];
    (
        diagnostic.message.clone(),
        diagnostic.help.clone(),
        diagnostic.span,
    )
}

fn int(text: &str) -> TokenKind {
    TokenKind::IntegerLiteral(text.to_owned())
}
fn uint(text: &str) -> TokenKind {
    TokenKind::UIntLiteral(text.to_owned())
}
fn long(text: &str) -> TokenKind {
    TokenKind::LongLiteral(text.to_owned())
}
fn ulong(text: &str) -> TokenKind {
    TokenKind::ULongLiteral(text.to_owned())
}

#[test]
fn unsuffixed_literals_take_the_first_fitting_type_at_every_boundary() {
    let boundaries: [(u128, &str); 9] = [
        (0, "int"),
        (0x7FFF_FFFF, "int"),
        (0x8000_0000, "uint"),
        (0xFFFF_FFFF, "uint"),
        (0x1_0000_0000, "long"),
        (0x7FFF_FFFF_FFFF_FFFF, "long"),
        (0x8000_0000_0000_0000, "ulong"),
        (0xFFFF_FFFF_FFFF_FFFF, "ulong"),
        (1, "int"),
    ];
    for (value, expected) in boundaries {
        let text = value.to_string();
        let kind = match expected {
            "int" => int(&text),
            "uint" => uint(&text),
            "long" => long(&text),
            _ => ulong(&text),
        };
        for source in [
            format!("0x{value:X}"),
            format!("0x{value:x}"),
            format!("0X{value:X}"),
            format!("0b{value:b}"),
            format!("0B{value:b}"),
        ] {
            assert_eq!(literal(&source), kind, "{source}");
        }
    }
}

#[test]
fn hexadecimal_and_binary_spellings_of_one_value_are_the_same_token() {
    for value in [
        0_u64,
        1,
        10,
        255,
        1000,
        65_535,
        0x7FFF_FFFF,
        0x8000_0000,
        u64::MAX,
    ] {
        let hexadecimal = literal(&format!("0x{value:X}"));
        let binary = literal(&format!("0b{value:b}"));
        let decimal_with_suffix = literal(&format!("{value}UL"));
        assert_eq!(hexadecimal, binary, "{value}");
        // The decimal text is the normalized value, whatever the radix.
        let (TokenKind::IntegerLiteral(text)
        | TokenKind::UIntLiteral(text)
        | TokenKind::LongLiteral(text)
        | TokenKind::ULongLiteral(text)) = &hexadecimal
        else {
            panic!("an integer token")
        };
        assert_eq!(text, &value.to_string());
        assert!(matches!(decimal_with_suffix, TokenKind::ULongLiteral(_)));
    }
}

#[test]
fn leading_zeros_and_case_do_not_change_the_value() {
    assert_eq!(literal("0x00FF"), int("255"));
    assert_eq!(literal("0xff"), int("255"));
    assert_eq!(literal("0xFf"), int("255"));
    assert_eq!(literal("0b0000_0000".replace('_', "").as_str()), int("0"));
    assert_eq!(literal("0b00001010"), int("10"));
    assert_eq!(literal("0XFFFFFFFFFFFFFFFF"), ulong("18446744073709551615"));
}

#[test]
fn every_suffix_spelling_selects_the_documented_type() {
    for suffix in ["U", "u"] {
        assert_eq!(literal(&format!("0xFF{suffix}")), uint("255"));
        assert_eq!(literal(&format!("0b11111111{suffix}")), uint("255"));
        // The `U` token kind is shared with decimal literals; semantic analysis
        // classifies a value above `uint` as `ulong` (see the compiler tests).
        assert_eq!(literal(&format!("0x100000000{suffix}")), uint("4294967296"));
        assert_eq!(literal(&format!("0xFFFFFFFF{suffix}")), uint("4294967295"));
    }
    for suffix in ["L", "l"] {
        assert_eq!(literal(&format!("0xFF{suffix}")), long("255"));
        assert_eq!(
            literal(&format!("0x7FFFFFFFFFFFFFFF{suffix}")),
            long("9223372036854775807")
        );
        // `L` never becomes `ulong`: the semantic range check owns the error.
        assert_eq!(
            literal(&format!("0xFFFFFFFFFFFFFFFF{suffix}")),
            long("18446744073709551615")
        );
    }
    for suffix in ["UL", "Ul", "uL", "ul", "LU", "Lu", "lU", "lu"] {
        assert_eq!(literal(&format!("0xFF{suffix}")), ulong("255"));
        assert_eq!(literal(&format!("0b101{suffix}")), ulong("5"));
    }
}

#[test]
fn literal_is_followed_by_ordinary_tokens() {
    // A member call on a prefixed literal, like `255.ToString()`.
    assert_eq!(
        tokens("0xFF.ToString()")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>(),
        [
            int("255"),
            TokenKind::Dot,
            TokenKind::Identifier("ToString".into()),
            TokenKind::LeftParen,
            TokenKind::RightParen,
        ]
    );
    let spans: Vec<Span> = tokens("0xFF.ToString()")
        .into_iter()
        .map(|(_, span)| span)
        .collect();
    assert_eq!(spans[0], Span::new(0, 4));
    assert_eq!(spans[1], Span::new(4, 5));
    // Operators and separators end the literal.
    assert_eq!(
        tokens("0x0F|0b1010")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>(),
        [int("15"), TokenKind::Pipe, int("10")]
    );
    assert_eq!(
        tokens("[0x1,0b10]")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>(),
        [
            TokenKind::LeftBracket,
            int("1"),
            TokenKind::Comma,
            int("2"),
            TokenKind::RightBracket,
        ]
    );
    // Spans are exact inside a larger source.
    let all = tokens("x = 0xFFUL + 0b11;");
    assert_eq!(all[2], (ulong("255"), Span::new(4, 10)));
    assert_eq!(all[4], (int("3"), Span::new(13, 17)));
}

#[test]
fn literals_lex_inside_string_interpolation() {
    let kinds: Vec<TokenKind> = tokens("$\"v={0xFF} w={0b101}\"")
        .into_iter()
        .map(|(kind, _)| kind)
        .collect();
    assert!(kinds.contains(&int("255")), "{kinds:?}");
    assert!(kinds.contains(&int("5")), "{kinds:?}");
}

#[test]
fn decimal_and_floating_point_lexing_is_unchanged() {
    assert_eq!(literal("0"), int("0"));
    assert_eq!(literal("42"), int("42"));
    assert_eq!(literal("4294967295"), int("4294967295"));
    assert_eq!(literal("007"), int("007"));
    assert_eq!(literal("10U"), uint("10"));
    assert_eq!(literal("10L"), long("10"));
    assert_eq!(literal("0.5"), TokenKind::FloatLiteral("0.5".into()));
    assert_eq!(literal("1.5d"), TokenKind::DoubleLiteral("1.5".into()));
    assert_eq!(literal("2f"), TokenKind::FloatLiteral("2".into()));
    assert_eq!(literal("0f"), TokenKind::FloatLiteral("0".into()));
    assert_eq!(literal("0d"), TokenKind::DoubleLiteral("0".into()));
    assert_eq!(literal("0m"), TokenKind::DecimalLiteral("0".into()));
    // Only a literal that is exactly `0` followed by a prefix letter is prefixed.
    assert_eq!(
        tokens("10x5")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>(),
        [int("10"), TokenKind::Identifier("x5".into())]
    );
    assert_eq!(
        tokens("00x1")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>(),
        [int("00"), TokenKind::Identifier("x1".into())]
    );
    assert_eq!(
        tokens("0 x")
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>(),
        [int("0"), TokenKind::Identifier("x".into())]
    );
}

#[test]
fn malformed_prefixed_literals_are_controlled_lexical_errors() {
    // Empty prefixes report the prefix.
    for (source, name) in [
        ("0x", "hexadecimal"),
        ("0X;", "hexadecimal"),
        ("0b", "binary"),
        ("0B ", "binary"),
    ] {
        let (message, help, span) = error(source);
        assert_eq!(message, format!("{name} literal has no digits"), "{source}");
        assert!(
            help.is_some_and(|help| help.contains("at least one digit")),
            "{source}"
        );
        assert_eq!(span, Span::new(0, 2), "{source}");
    }
    // An invalid digit reports exactly that character.
    for (source, message, span) in [
        (
            "0b102",
            "invalid digit `2` in binary literal",
            Span::new(4, 5),
        ),
        (
            "0b1F",
            "invalid digit `F` in binary literal",
            Span::new(3, 4),
        ),
        (
            "0xFG",
            "invalid digit `G` in hexadecimal literal",
            Span::new(3, 4),
        ),
        (
            "0x1z",
            "invalid digit `z` in hexadecimal literal",
            Span::new(3, 4),
        ),
    ] {
        let (found, _, found_span) = error(source);
        assert_eq!(found, message, "{source}");
        assert_eq!(found_span, span, "{source}");
    }
    // Floating-point and decimal suffixes do not apply.
    for (source, digit) in [("0x1m", 'm'), ("0b1d", 'd'), ("0b1f", 'f'), ("0b1M", 'M')] {
        let (message, help, _) = error(source);
        assert!(
            message.starts_with(&format!("invalid digit `{digit}`")),
            "{source}: {message}"
        );
        assert!(
            help.is_some_and(|help| help.contains("only to decimal literals")),
            "{source}"
        );
    }
    // Malformed integer suffixes report the suffix.
    for (source, suffix, span) in [
        ("0xFFLL", "LL", Span::new(4, 6)),
        ("0xFFUU", "UU", Span::new(4, 6)),
        ("0xFFULL", "ULL", Span::new(4, 7)),
        ("0b1LUL", "LUL", Span::new(3, 6)),
    ] {
        let (message, help, found_span) = error(source);
        assert_eq!(
            message,
            format!("invalid integer literal suffix `{suffix}`"),
            "{source}"
        );
        assert!(help.is_some_and(|help| help.contains("`UL`")), "{source}");
        assert_eq!(found_span, span, "{source}");
    }
    // Values above `ulong` report the whole literal.
    for source in [
        "0x10000000000000000",
        "0xFFFFFFFFFFFFFFFFF",
        "0b10000000000000000000000000000000000000000000000000000000000000000",
    ] {
        let (message, _, span) = error(source);
        assert!(
            message.ends_with("literal exceeds the range of `ulong`"),
            "{source}: {message}"
        );
        assert_eq!(span, Span::new(0, source.len()), "{source}");
    }
    // A suffixed value above `ulong` is the same error.
    assert!(
        error("0x10000000000000000UL")
            .0
            .contains("exceeds the range")
    );
}

#[test]
fn a_prefixed_literal_error_does_not_cascade() {
    for source in ["0xFFLL + 1", "0b102 + 1", "0x + 1", "x = 0x1g;"] {
        let diagnostics = lex(source).expect_err("rejected");
        assert_eq!(diagnostics.len(), 1, "`{source}`: {diagnostics:#?}");
    }
}

#[test]
fn hexadecimal_floating_point_does_not_exist() {
    let tokens = lex("public int F() { return 0x1.5; }").expect("lexes");
    assert!(parse(tokens).is_err());
}
