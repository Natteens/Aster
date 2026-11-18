//! T-LANG-3 (`Q-LANG-1`): syntax, disambiguation, and formatting tests for
//! `T?`, `null`, `??`, and `?.`.

use aster_syntax::format::format_source;
use aster_syntax::{
    BinaryOperator, Expression, ExpressionKind, Item, Literal, Statement, TokenKind, VariableKind,
    lex, parse,
};

fn function_statements(source: &str) -> Vec<Statement> {
    let module = parse(lex(source).expect("source lexes")).expect("source parses");
    let Item::Function(function) = &module.items[0] else {
        panic!("expected a function")
    };
    function
        .body
        .as_ref()
        .expect("function body")
        .statements
        .clone()
}

fn returned(source: &str) -> Expression {
    let Statement::Return {
        value: Some(value), ..
    } = function_statements(source).remove(0)
    else {
        panic!("expected a return statement")
    };
    value
}

#[test]
fn lexes_null_coalesce_and_null_conditional_tokens() {
    let tokens = lex("a?.b ?? null").expect("lexes");
    assert!(matches!(tokens[0].kind, TokenKind::Identifier(ref name) if name == "a"));
    assert_eq!(tokens[1].kind, TokenKind::QuestionDot);
    assert!(matches!(tokens[2].kind, TokenKind::Identifier(ref name) if name == "b"));
    assert_eq!(tokens[3].kind, TokenKind::QuestionQuestion);
    assert_eq!(tokens[4].kind, TokenKind::Null);
}

#[test]
fn parses_nullable_type_refs_casts_and_new_arrays() {
    let statements = function_statements(
        "public void F(int? a, int?? b, int?[] c, int[]? d, Option<int?> e, List<string?> f) {
            var x = (int?)1;
            var y = (double??)2;
            var arr = new int?[3];
            void? v = null;
        }",
    );
    let Item::Function(function) =
        &parse(lex("public void F(int? a, int?? b, int?[] c, int[]? d, Option<int?> e, List<string?> f) {}").expect("lex"))
            .expect("parse")
            .items[0]
    else {
        panic!("expected function");
    };
    assert_eq!(function.parameters[0].type_ref.name, "int?");
    assert_eq!(function.parameters[1].type_ref.name, "int??");
    assert_eq!(function.parameters[2].type_ref.name, "int?[]");
    assert_eq!(function.parameters[3].type_ref.name, "int[]?");
    assert_eq!(function.parameters[4].type_ref.name, "Option<int?>");
    assert_eq!(function.parameters[5].type_ref.name, "List<string?>");

    let Statement::Variable(x) = &statements[0] else {
        panic!("expected var x")
    };
    assert!(matches!(
        &x.initializer.as_ref().expect("init").kind,
        ExpressionKind::Cast { target, .. } if target.name == "int?"
    ));
    let Statement::Variable(y) = &statements[1] else {
        panic!("expected var y")
    };
    assert!(matches!(
        &y.initializer.as_ref().expect("init").kind,
        ExpressionKind::Cast { target, .. } if target.name == "double??"
    ));
    let Statement::Variable(arr) = &statements[2] else {
        panic!("expected var arr")
    };
    assert!(matches!(
        &arr.initializer.as_ref().expect("init").kind,
        ExpressionKind::NewArray { element_type, .. } if element_type.name == "int?"
    ));
    let Statement::Variable(v) = &statements[3] else {
        panic!("expected var v")
    };
    assert!(matches!(
        &v.kind,
        VariableKind::Explicit(type_ref) if type_ref.name == "void?"
    ));
}

#[test]
fn disambiguates_nullable_declarations_and_ternary_expressions() {
    let stmts = function_statements(
        "public void F(bool flag, Foo x, Foo y) {
            Foo? a = null;
            Foo? b;
            Foo?[] cs = [];
            flag ? x : y;
            flag ? [] : [1];
            foreach (Foo? item in items) { }
        }",
    );
    assert!(matches!(
        &stmts[0],
        Statement::Variable(v) if matches!(&v.kind, VariableKind::Explicit(t) if t.name == "Foo?")
    ));
    assert!(matches!(
        &stmts[1],
        Statement::Variable(v) if matches!(&v.kind, VariableKind::Explicit(t) if t.name == "Foo?")
    ));
    assert!(matches!(
        &stmts[2],
        Statement::Variable(v) if matches!(&v.kind, VariableKind::Explicit(t) if t.name == "Foo?[]")
    ));
    assert!(matches!(
        &stmts[3],
        Statement::Expression(Expression {
            kind: ExpressionKind::Conditional { .. },
            ..
        })
    ));
    assert!(matches!(
        &stmts[4],
        Statement::Expression(Expression {
            kind: ExpressionKind::Conditional { .. },
            ..
        })
    ));
    assert!(matches!(
        &stmts[5],
        Statement::ForEach {
            element_type: Some(t),
            ..
        } if t.name == "Foo?"
    ));
}

#[test]
fn disambiguates_type_pattern_and_ternary_question() {
    let ternary = returned("public int F(Shape x) { return x is Foo ? 1 : 0; }");
    let ExpressionKind::Conditional { condition, .. } = ternary.kind else {
        panic!("expected ternary around `is Foo`")
    };
    assert!(matches!(
        condition.kind,
        ExpressionKind::TypePattern {
            ref target,
            binding: None,
            ..
        } if target.name == "Foo"
    ));

    let nullable_pattern = returned("public bool F(Shape x) { return x is Foo? f; }");
    assert!(matches!(
        nullable_pattern.kind,
        ExpressionKind::TypePattern {
            ref target,
            binding: Some(ref b),
            ..
        } if target.name == "Foo?" && b.name == "f"
    ));

    let nullable_then_ternary = returned("public int F(Shape x) { return x is Foo? f ? 1 : 0; }");
    let ExpressionKind::Conditional { condition, .. } = nullable_then_ternary.kind else {
        panic!("expected ternary")
    };
    assert!(matches!(
        condition.kind,
        ExpressionKind::TypePattern {
            ref target,
            binding: Some(ref b),
            ..
        } if target.name == "Foo?" && b.name == "f"
    ));
}

#[test]
fn parses_coalesce_precedence_and_null_conditional_chains() {
    let right_assoc = returned("public int F(int? a, int? b, int c) { return a ?? b ?? c; }");
    let ExpressionKind::NullCoalesce { value, fallback } = right_assoc.kind else {
        panic!("expected NullCoalesce")
    };
    assert!(matches!(value.kind, ExpressionKind::Name(_)));
    assert!(matches!(fallback.kind, ExpressionKind::NullCoalesce { .. }));

    let with_or = returned("public bool F(bool? a, bool b, bool c) { return a || b ?? c; }");
    let ExpressionKind::NullCoalesce { value, .. } = with_or.kind else {
        panic!("expected NullCoalesce at root")
    };
    assert!(matches!(
        value.kind,
        ExpressionKind::Binary {
            operator: BinaryOperator::LogicalOr,
            ..
        }
    ));

    let with_ternary = returned("public int F(bool? a, bool b) { return a ?? b ? 1 : 2; }");
    let ExpressionKind::Conditional { condition, .. } = with_ternary.kind else {
        panic!("expected Conditional at root")
    };
    assert!(matches!(
        condition.kind,
        ExpressionKind::NullCoalesce { .. }
    ));

    let chain = returned("public int? F(A? a) { return a?.B.C(1)[0]; }");
    let ExpressionKind::NullConditional { receiver, access } = chain.kind else {
        panic!("expected NullConditional")
    };
    assert!(matches!(receiver.kind, ExpressionKind::Name(_)));
    assert!(matches!(access.kind, ExpressionKind::Index { .. }));

    let nested = returned("public string? F(A? a) { return a?.B?.C(); }");
    let ExpressionKind::NullConditional { receiver, .. } = nested.kind else {
        panic!("expected outer NullConditional")
    };
    assert!(matches!(
        receiver.kind,
        ExpressionKind::NullConditional { .. }
    ));

    let literal_null = returned("public int? F() { return null; }");
    assert!(matches!(
        literal_null.kind,
        ExpressionKind::Literal(Literal::Null)
    ));
}

#[test]
fn disambiguates_is_as_with_null_coalesce() {
    let as_coalesce_new = returned("public Sub F(Base b) { return b as Sub ?? new Sub(); }");
    let ExpressionKind::NullCoalesce { value, fallback } = as_coalesce_new.kind else {
        panic!("expected NullCoalesce for `b as Sub ?? new Sub()`");
    };
    assert!(matches!(
        value.kind,
        ExpressionKind::TypeAs { ref target, .. } if target.name == "Sub"
    ));
    assert!(matches!(
        fallback.kind,
        ExpressionKind::NewObject {
            type_name: Some(ref name),
            ..
        } if name == "Sub"
    ));

    let as_coalesce_ident =
        returned("public Sub F(Base b, Sub fallback) { return b as Sub ?? fallback; }");
    let ExpressionKind::NullCoalesce { value, fallback } = as_coalesce_ident.kind else {
        panic!("expected NullCoalesce for `b as Sub ?? fallback`");
    };
    assert!(matches!(
        value.kind,
        ExpressionKind::TypeAs { ref target, .. } if target.name == "Sub"
    ));
    assert!(matches!(fallback.kind, ExpressionKind::Name(ref name) if name == "fallback"));

    let is_coalesce_false = returned("public bool F(object a) { return a is A ?? false; }");
    let ExpressionKind::NullCoalesce { value, fallback } = is_coalesce_false.kind else {
        panic!("expected NullCoalesce for `a is A ?? false`");
    };
    assert!(matches!(
        value.kind,
        ExpressionKind::TypePattern {
            ref target,
            binding: None,
            ..
        } if target.name == "A"
    ));
    assert!(matches!(
        fallback.kind,
        ExpressionKind::Literal(Literal::Boolean(false))
    ));

    let is_coalesce_ident = returned("public bool F(object a, bool b) { return a is A ?? b; }");
    let ExpressionKind::NullCoalesce { value, fallback } = is_coalesce_ident.kind else {
        panic!("expected NullCoalesce for `a is A ?? b`");
    };
    assert!(matches!(
        value.kind,
        ExpressionKind::TypePattern {
            ref target,
            binding: None,
            ..
        } if target.name == "A"
    ));
    assert!(matches!(fallback.kind, ExpressionKind::Name(ref name) if name == "b"));

    let is_double_q = returned("public bool F(object a) { return a is A??; }");
    assert!(matches!(
        is_double_q.kind,
        ExpressionKind::TypePattern {
            ref target,
            binding: None,
            ..
        } if target.name == "A??"
    ));

    let as_double_q = returned("public Sub?? F(Base b) { return b as Sub??; }");
    assert!(matches!(
        as_double_q.kind,
        ExpressionKind::TypeAs { ref target, .. } if target.name == "Sub??"
    ));
}

#[test]
fn parses_non_primitive_double_nullable_type_boundaries() {
    let stmts = function_statements(
        "public void F() {
            Foo?? x = null;
            Option<int>?? d = null;
            int[]?? e = null;
            var a1 = new Foo?[3];
            var a2 = new Foo??[3];
            var a3 = new int??[3];
        }",
    );
    assert!(matches!(
        &stmts[0],
        Statement::Variable(v) if matches!(&v.kind, VariableKind::Explicit(t) if t.name == "Foo??")
    ));
    assert!(matches!(
        &stmts[1],
        Statement::Variable(v) if matches!(&v.kind, VariableKind::Explicit(t) if t.name == "Option<int>??")
    ));
    assert!(matches!(
        &stmts[2],
        Statement::Variable(v) if matches!(&v.kind, VariableKind::Explicit(t) if t.name == "int[]??")
    ));
    assert!(matches!(
        &stmts[3],
        Statement::Variable(v) if matches!(
            v.initializer.as_ref().expect("init").kind,
            ExpressionKind::NewArray { ref element_type, .. } if element_type.name == "Foo?"
        )
    ));
    assert!(matches!(
        &stmts[4],
        Statement::Variable(v) if matches!(
            v.initializer.as_ref().expect("init").kind,
            ExpressionKind::NewArray { ref element_type, .. } if element_type.name == "Foo??"
        )
    ));
    assert!(matches!(
        &stmts[5],
        Statement::Variable(v) if matches!(
            v.initializer.as_ref().expect("init").kind,
            ExpressionKind::NewArray { ref element_type, .. } if element_type.name == "int??"
        )
    ));
}

#[test]
fn formatter_keeps_nullable_syntax_canonical_and_idempotent() {
    let source = "public class Box\n{\n    Foo?? field;\n    public Foo?? Method(Foo?? p)\n    {\n        return p;\n    }\n}\n\npublic int F(Foo? x, Foo?? y, int?[] ys, int[]? zs, bool flag)\n{\n    int? a = (int?)1;\n    int?? b = null;\n    Foo ?? c = null;\n    Option<int> ?? d = null;\n    int[] ?? e = null;\n    var arr = new Foo ?? [0];\n    Option<Foo ??> g = null;\n    var cast = (Foo ??) x;\n    var c0 = x?.Bar.Baz(1) ?? 0;\n    var d0 = x is Foo ? 1 : 2;\n    var e0 = GetOpt()? ?? 0;\n    var bin1 = a??b;\n    var bin2 = ys[0]??0;\n    var bin3 = x as Sub??new Sub();\n    var bin4 = x is Foo??false;\n    var bin5 = d?.Get()??b;\n    opt ?? DoSomething();\n    return c0 + d0 + e0;\n}\n";
    let formatted = format_source(source).expect("formats cleanly");
    assert!(formatted.contains("Foo?? field;"), "{formatted}");
    assert!(
        formatted.contains("public Foo?? Method(Foo?? p)"),
        "{formatted}"
    );
    assert!(
        formatted.contains("Foo? x, Foo?? y, int?[] ys, int[]? zs"),
        "{formatted}"
    );
    assert!(formatted.contains("(int?)1"), "{formatted}");
    assert!(formatted.contains("int?? b = null;"), "{formatted}");
    assert!(formatted.contains("Foo?? c = null;"), "{formatted}");
    assert!(formatted.contains("Option<int>?? d = null;"), "{formatted}");
    assert!(formatted.contains("int[]?? e = null;"), "{formatted}");
    assert!(formatted.contains("new Foo??[0]"), "{formatted}");
    assert!(formatted.contains("Option<Foo??> g = null;"), "{formatted}");
    assert!(formatted.contains("(Foo??)x"), "{formatted}");
    assert!(formatted.contains("x?.Bar.Baz(1) ?? 0"), "{formatted}");
    assert!(formatted.contains("x is Foo ? 1 : 2"), "{formatted}");
    assert!(formatted.contains("GetOpt()? ?? 0"), "{formatted}");
    assert!(formatted.contains("a ?? b"), "{formatted}");
    assert!(formatted.contains("ys[0] ?? 0"), "{formatted}");
    assert!(formatted.contains("x as Sub ?? new Sub()"), "{formatted}");
    assert!(formatted.contains("x is Foo ?? false"), "{formatted}");
    assert!(formatted.contains("d?.Get() ?? b"), "{formatted}");
    assert!(formatted.contains("opt ?? DoSomething();"), "{formatted}");
    assert_eq!(format_source(&formatted).expect("idempotent"), formatted);
}

#[test]
fn formatter_preserves_binary_coalesce_empty_array_and_glues_nullable_array_types() {
    let source = concat!(
        "public void F(Foo??[] a, Foo ?? [] b, Foo??[]?? c)\n",
        "{\n",
        "    Foo??[] arr = [];\n",
        "    Foo??[][] multi = [];\n",
        "    Foo??[]? optArr = null;\n",
        "    Foo??[]?? doubleOptArr = null;\n",
        "    Option<Foo??[]> genOpt = null;\n",
        "    Pair<Foo??[], int> genPair = null;\n",
        "    var made = new Foo??[0];\n",
        "    var castArr = (Foo??[])x;\n",
        "    int h1 = Helper(xs??[]);\n",
        "    int h2 = Helper2(xs??[], 4);\n",
        "    int[] ys = (xs??[])??[1, 2];\n",
        "    int[][] list = [opt??[], opt2??[]];\n",
        "    int[] cond = flag ? opt??[] : [1];\n",
        "    int[] s1 = opt??[];\n",
        "    var chain1 = opt??[]??[2];\n",
        "    var chain2 = xs??[]??ys;\n",
        "}\n",
    );
    let formatted = format_source(source).expect("formats cleanly");
    assert!(
        formatted.contains("public void F(Foo??[] a, Foo??[] b, Foo??[]?? c)"),
        "{formatted}"
    );
    assert!(formatted.contains("Foo??[] arr = [];"), "{formatted}");
    assert!(formatted.contains("Foo??[][] multi = [];"), "{formatted}");
    assert!(formatted.contains("Foo??[]? optArr = null;"), "{formatted}");
    assert!(
        formatted.contains("Foo??[]?? doubleOptArr = null;"),
        "{formatted}"
    );
    assert!(
        formatted.contains("Option<Foo??[]> genOpt = null;"),
        "{formatted}"
    );
    assert!(
        formatted.contains("Pair<Foo??[], int> genPair = null;"),
        "{formatted}"
    );
    assert!(
        formatted.contains("var made = new Foo??[0];"),
        "{formatted}"
    );
    assert!(
        formatted.contains("var castArr = (Foo??[])x;"),
        "{formatted}"
    );
    assert!(
        formatted.contains("int h1 = Helper(xs ?? []);"),
        "{formatted}"
    );
    assert!(
        formatted.contains("int h2 = Helper2(xs ?? [], 4);"),
        "{formatted}"
    );
    assert!(
        formatted.contains("int[] ys = (xs ?? []) ?? [1, 2];"),
        "{formatted}"
    );
    assert!(
        formatted.contains("int[][] list = [opt ?? [], opt2 ?? []];"),
        "{formatted}"
    );
    assert!(
        formatted.contains("int[] cond = flag ? opt ?? [] : [1];"),
        "{formatted}"
    );
    assert!(formatted.contains("int[] s1 = opt ?? [];"), "{formatted}");
    assert!(
        formatted.contains("var chain1 = opt ?? [] ?? [2];"),
        "{formatted}"
    );
    assert!(
        formatted.contains("var chain2 = xs ?? [] ?? ys;"),
        "{formatted}"
    );
    assert_eq!(format_source(&formatted).expect("idempotent"), formatted);
}

#[test]
fn malformed_null_conditional_reports_syntax_error() {
    assert!(parse(lex("public int F(A? a) { return a ? .B; }").expect("lex")).is_err());
    assert!(parse(lex("public int F(A? a) { return a?. ; }").expect("lex")).is_err());
}
