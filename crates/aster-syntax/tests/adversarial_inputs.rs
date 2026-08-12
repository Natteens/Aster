use aster_syntax::{lex, parse};

const LARGE_LEXEME_BYTES: usize = 64 * 1024;

#[test]
fn empty_whitespace_and_large_comments_remain_controlled() {
    parse(lex("").expect("empty source lexes")).expect("empty source parses");
    parse(lex(" \r\n\t").expect("whitespace lexes")).expect("whitespace parses");

    let source = format!(
        "//{}\npublic int Main() {{ return 0; }}",
        "x".repeat(LARGE_LEXEME_BYTES)
    );
    parse(lex(&source).expect("large comment lexes")).expect("large comment parses");
}

#[test]
fn long_identifiers_numbers_and_strings_remain_controlled() {
    let identifier = "a".repeat(LARGE_LEXEME_BYTES);
    let source = format!("public int {identifier}() {{ return 0; }}");
    parse(lex(&source).expect("long identifier lexes")).expect("long identifier parses");

    let number = "9".repeat(LARGE_LEXEME_BYTES);
    let source = format!("public int Main() {{ return {number}; }}");
    parse(lex(&source).expect("long number lexes")).expect("long number parses");

    let text = "x".repeat(LARGE_LEXEME_BYTES);
    let source = format!("public string Main() {{ return \"{text}\"; }}");
    parse(lex(&source).expect("long string lexes")).expect("long string parses");
}

#[test]
fn truncated_and_malformed_inputs_return_diagnostics() {
    for source in [
        "public int Main(",
        "public int Main() { return (1 + 2;",
        "public int Main() { if (true) { return 1; }",
    ] {
        let first = parse(lex(source).expect("malformed delimiters still lex"))
            .expect_err("malformed source must be rejected");
        let second = parse(lex(source).expect("malformed delimiters still lex"))
            .expect_err("malformed source must be rejected");
        assert_eq!(first, second, "diagnostics changed for: {source}");
    }

    for source in ["\"unterminated", "$\"{value"] {
        assert!(lex(source).is_err(), "source unexpectedly lexed: {source}");
    }
}
