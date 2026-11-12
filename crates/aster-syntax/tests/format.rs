//! Formatter tests (TOOL-001): per-construct behavior and a corpus check over
//! every `.aster` file in the repository.

use std::path::{Path, PathBuf};

use aster_syntax::{TokenKind, format::format_source, lex, parse};

fn fmt(source: &str) -> String {
    format_source(source).expect("source formats")
}

fn assert_formats(input: &str, expected: &str) {
    let output = fmt(input);
    assert_eq!(output, expected, "formatting\n{input}");
    assert_eq!(fmt(&output), output, "idempotence for\n{input}");
}

fn kinds(source: &str) -> Vec<TokenKind> {
    lex(source)
        .expect("lexes")
        .into_iter()
        .map(|token| token.kind)
        .collect()
}

#[test]
fn indents_with_four_spaces_and_moves_braces_to_their_own_line() {
    assert_formats(
        "public int Run() {\n  int x = 1;\n    if (x > 0) {\n\treturn x;\n  }\n  return 0;\n}\n",
        "public int Run()\n{\n    int x = 1;\n    if (x > 0)\n    {\n        return x;\n    }\n    return 0;\n}\n",
    );
}

#[test]
fn keeps_single_line_blocks_and_initializers_on_their_line() {
    assert_formats(
        "public int F(int a)\n{\n  if (a>0) { return a; }\n  Pair<int, string> p = Pair<int, string> { First: 2, Second: \"x\" };\n  return 0;\n}",
        "public int F(int a)\n{\n    if (a > 0) { return a; }\n    Pair<int, string> p = Pair<int, string> { First: 2, Second: \"x\" };\n    return 0;\n}\n",
    );
}

#[test]
fn types_properties_and_accessors_use_allman_braces() {
    assert_formats(
        "public class A : B {\n private int x;\n public int X {\n get { return x; }\n set { x = value; }\n }\n}\n",
        "public class A : B\n{\n    private int x;\n    public int X\n    {\n        get { return x; }\n        set { x = value; }\n    }\n}\n",
    );
}

#[test]
fn else_starts_a_new_line_after_a_multiline_block() {
    assert_formats(
        "public int F(int a) {\n if (a > 0) {\n return 1;\n } else if (a < 0) {\n return 2;\n } else {\n return 3;\n }\n}\n",
        "public int F(int a)\n{\n    if (a > 0)\n    {\n        return 1;\n    }\n    else if (a < 0)\n    {\n        return 2;\n    }\n    else\n    {\n        return 3;\n    }\n}\n",
    );
}

#[test]
fn switch_labels_follow_the_repository_style() {
    assert_formats(
        "public int F(int a) {\nswitch (a) {\ncase Quit:\nreturn 0;\ndefault:\nreturn 1;\n}\n}\n",
        "public int F(int a)\n{\n    switch (a)\n    {\n        case Quit:\n            return 0;\n        default:\n            return 1;\n    }\n}\n",
    );
}

#[test]
fn spaces_operators_and_keeps_casts_generics_and_calls_tight() {
    assert_formats(
        "public long F(int a,int b){return (long)a*b+-a>>>1 ;}",
        "public long F(int a, int b) { return (long)a * b + -a >>> 1; }\n",
    );
    assert_formats(
        "void F() { List<List<int>> v = new List<List<int>>(); x = a<b; y = c ? d:e; i ++; --j; z = !flag; w = v [0]; int r = Parse(t) ?; }",
        "void F() { List<List<int>> v = new List<List<int>>(); x = a < b; y = c ? d : e; i++; --j; z = !flag; w = v[0]; int r = Parse(t)?; }\n",
    );
    assert_formats(
        "void F() { if(a){ while(b) {} } }",
        "void F() { if (a) { while (b) {} } }\n",
    );
}

#[test]
fn unary_minus_never_merges_into_decrement() {
    assert_formats(
        "void F() { x = - -y; z = a - -b; }",
        "void F() { x = - -y; z = a - -b; }\n",
    );
}

#[test]
fn continuation_lines_are_indented_one_extra_level() {
    assert_formats(
        "void F()\n{\nstring s = \"a\"\n+ \"b\"\n+ \"c\";\nif (x)\nreturn;\n}\n",
        "void F()\n{\n    string s = \"a\"\n        + \"b\"\n        + \"c\";\n    if (x)\n        return;\n}\n",
    );
}

#[test]
fn blank_lines_collapse_and_edges_are_trimmed() {
    assert_formats(
        "\n\nvoid F()\n{\n\n    a();\n\n\n\n    b();   \n\n}\n\n\n",
        "void F()\n{\n    a();\n\n    b();\n}\n",
    );
    assert_eq!(fmt(""), "");
    assert_eq!(fmt("  \n\n"), "");
}

#[test]
fn preserves_every_comment_form() {
    let source = "// header\n\n/* block */ void F() // trailing\n{\n        // own line\n    int x = 1; /* inline */ int y = 2;\n  /* multi\n     line */\n    return; // end\n}\n// footer";
    let output = fmt(source);
    for comment in [
        "// header",
        "/* block */",
        "// trailing",
        "// own line",
        "/* inline */",
        "/* multi\n     line */",
        "// end",
        "// footer",
    ] {
        assert!(output.contains(comment), "lost `{comment}` in\n{output}");
    }
    assert_eq!(fmt(&output), output);
    assert_eq!(kinds(source), kinds(&output));
}

#[test]
fn comments_are_reindented_and_a_comment_before_a_label_is_a_label() {
    assert_formats(
        "void F()\n{\nswitch (a)\n{\n// first\ncase A:\nfoo();\n// second\ncase B:\nbar();\n}\n// done\n}\n",
        "void F()\n{\n    switch (a)\n    {\n        // first\n        case A:\n            foo();\n        // second\n        case B:\n            bar();\n    }\n    // done\n}\n",
    );
}

#[test]
fn interpolated_strings_are_left_verbatim() {
    assert_formats(
        "void F() { string s = $\"a {x+1} b {{c}}\"; }",
        "void F() { string s = $\"a {x+1} b {{c}}\"; }\n",
    );
}

#[test]
fn crlf_input_keeps_crlf_output() {
    let output = fmt("void F()\r\n{\r\n  a();\r\n}\r\n");
    assert_eq!(output, "void F()\r\n{\r\n    a();\r\n}\r\n");
    assert_eq!(fmt(&output), output);
}

#[test]
fn lexical_errors_are_reported_not_rewritten() {
    assert!(format_source("void F() { string s = \"open; }").is_err());
    assert!(format_source("/* never closed").is_err());
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name != "target" && name != "node_modules" && !name.starts_with('.') {
                collect(&path, files);
            }
        } else if path
            .extension()
            .is_some_and(|extension| extension == "aster")
        {
            files.push(path);
        }
    }
}

/// Comments outside string literals, recovered from the gaps between tokens.
fn comments_of(source: &str) -> Vec<String> {
    let tokens = lex(source).expect("lexes");
    let mut gaps: Vec<&str> = Vec::new();
    let mut cursor = 0;
    for token in &tokens {
        gaps.push(&source[cursor..token.span.start]);
        cursor = cursor.max(token.span.end);
    }
    gaps.push(&source[cursor..]);
    let mut comments = Vec::new();
    for gap in gaps {
        let mut rest = gap;
        while let Some(index) = [rest.find("//"), rest.find("/*")]
            .into_iter()
            .flatten()
            .min()
        {
            rest = &rest[index..];
            let length = if rest.starts_with("//") {
                rest.find('\n').unwrap_or(rest.len())
            } else {
                rest.find("*/").expect("closed") + 2
            };
            comments.push(rest[..length].trim_end().to_owned());
            rest = &rest[length..];
        }
    }
    comments
}

#[test]
fn every_repository_source_formats_safely_and_idempotently() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    collect(&root, &mut files);
    assert!(files.len() > 50, "found only {} .aster files", files.len());
    let mut total_comments = 0;
    for file in &files {
        let source = std::fs::read_to_string(file).expect("read source");
        let Ok(tokens) = lex(&source) else {
            // Deliberately invalid sources are not formatted.
            continue;
        };
        let original_parses = parse(tokens).is_ok();
        let output =
            format_source(&source).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        assert_eq!(
            kinds(&source),
            kinds(&output),
            "{}: tokens changed",
            file.display()
        );
        assert_eq!(
            parse(lex(&output).expect("relex")).is_ok(),
            original_parses,
            "{}: parse outcome changed",
            file.display()
        );
        let twice = format_source(&output).expect("second pass");
        assert_eq!(twice, output, "{}: not idempotent", file.display());
        let (before, after) = (comments_of(&source), comments_of(&output));
        assert_eq!(before, after, "{}: comments changed", file.display());
        total_comments += before.len();
    }
    assert!(
        total_comments > 20,
        "corpus lost its comments ({total_comments})"
    );
}

/// The repository's own sources are the style reference, so the formatter
/// must leave nearly all of them untouched. Remaining differences are
/// deliberate normalizations (misindented bodies, aligned trailing comments,
/// final newline, blank lines before a closing brace).
#[test]
fn repository_sources_are_nearly_canonical() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    collect(&root, &mut files);
    let changed = files
        .iter()
        .filter(|file| {
            let source = std::fs::read_to_string(file).expect("read source");
            format_source(&source).is_ok_and(|output| output != source)
        })
        .count();
    assert!(
        changed * 10 <= files.len(),
        "the formatter would change {changed} of {} repository sources",
        files.len()
    );
}

#[test]
fn where_constraints_space_their_colon() {
    assert_formats(
        "public T F<T>(T v) where T:IValue<int> { return v; }",
        "public T F<T>(T v) where T : IValue<int> { return v; }
",
    );
}
