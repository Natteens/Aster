use std::sync::atomic::{AtomicU64, Ordering};

use aster_compiler::{ProjectCompilation, compile_project, hir, mir};
use aster_diagnostics::DiagnosticCode;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn compile(source: &str) -> Result<ProjectCompilation, Vec<(DiagnosticCode, String)>> {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("aster-nullable-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary source");
    let result = compile_project(&path).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| (diagnostic.diagnostic.code, diagnostic.diagnostic.message))
            .collect()
    });
    std::fs::remove_file(&path).ok();
    result
}

fn rejects(source: &str, expected_code: &str, expected_substring: &str) {
    let Err(errors) = compile(source) else {
        panic!("compilation unexpectedly succeeded for:\n{source}");
    };
    assert!(
        errors
            .iter()
            .any(|(code, message)| code.as_str() == expected_code
                && message.contains(expected_substring)),
        "expected diagnostic {expected_code} containing {expected_substring:?}, got {errors:?}"
    );
}

fn rejects_any(source: &str, expected_substring: &str) {
    let Err(errors) = compile(source) else {
        panic!("compilation unexpectedly succeeded for:\n{source}");
    };
    assert!(
        errors
            .iter()
            .any(|(_, message)| message.contains(expected_substring)),
        "expected diagnostic containing {expected_substring:?}, got {errors:?}"
    );
}

fn hir_function<'a>(compilation: &'a ProjectCompilation, name: &str) -> &'a hir::Function {
    compilation
        .compilation
        .hir
        .items
        .iter()
        .find_map(|item| match item {
            hir::Item::Function(function) if function.name == name => Some(function),
            _ => None,
        })
        .unwrap_or_else(|| panic!("missing HIR function `{name}`"))
}

fn mir_function<'a>(compilation: &'a ProjectCompilation, name: &str) -> &'a mir::Function {
    compilation
        .compilation
        .mir
        .functions
        .iter()
        .find(|function| function.name == name)
        .unwrap_or_else(|| panic!("missing MIR function `{name}`"))
}

#[test]
fn nullable_shorthand_works_without_using_and_matches_core_option() {
    let source = r"
        int? ReadOpt(int? x) {
            return x;
        }

        void Main() {
            int? a = 42;
            int? b = null;
            int? g = ReadOpt(null);
            int? h = ReadOpt(9);
            int?[] arr1 = [1, null, 3];
            int?[] arr2 = [null, 2];
            int[]? arr3 = null;
            arr3 = [1, 2, 3];
            int?[]? arr4 = null;
            arr4 = [null, 5];
        }
    ";
    compile(source).expect("T? should work without explicit `using aster.core;`");

    let interop = r"
        using aster.core;

        void Main() {
            int? a = 42;
            Option<int> b = a;
            int? c = b;
            int? d = Option<int>.Some(7);
            int? e = Option<int>.None;
            Option<int> f = null;
        }
    ";
    compile(interop).expect("T? and aster.core::Option<T> should interoperate seamlessly");
}

#[test]
fn implicit_lifting_preserves_exact_match_overload_priority() {
    let source = r"
        int Pick(int x) { return 1; }
        int Pick(int? x) { return 2; }

        void Main() {
            int exact = Pick(5);
            int? opt = 5;
            int lifted = Pick(opt);
            int fromNull = Pick(null);
        }
    ";
    let compilation = compile(source).expect("overloads should resolve cleanly");
    let main = hir_function(&compilation, "Main");
    let mut called_signatures = Vec::new();
    for statement in &main.body.as_ref().expect("Main body").statements {
        if let hir::Statement::Variable(var) = statement
            && let Some(init) = &var.initializer
            && let hir::ExpressionKind::Call { callee, .. } = &init.kind
            && let hir::ExpressionKind::Symbol(sym) = callee.kind
        {
            let func = compilation
                .compilation
                .hir
                .items
                .iter()
                .find_map(|item| match item {
                    hir::Item::Function(f) if f.symbol == sym => Some(f),
                    _ => None,
                })
                .expect("callee function");
            called_signatures.push((var.name.clone(), func.parameters[0].type_.clone()));
        }
    }
    assert_eq!(called_signatures.len(), 3);
    assert_eq!(called_signatures[0].0, "exact");
    assert_eq!(called_signatures[0].1, hir::Type::Int);
    assert_eq!(called_signatures[1].0, "lifted");
    assert!(matches!(called_signatures[1].1, hir::Type::Enum(_)));
    assert_eq!(called_signatures[2].0, "fromNull");
    assert!(matches!(called_signatures[2].1, hir::Type::Enum(_)));
}

#[test]
fn null_coalesce_null_conditional_cast_and_try_propagation_compile() {
    let source = r#"
        struct Point {
            public int X;
        }

        interface INamed {
            string GetName();
        }

        class Person : INamed {
            public string Name { get; }
            public Person? Boss;

            public Person(string name, Person? boss) {
                this.Name = name;
                this.Boss = boss;
            }

            public string GetName() { return this.Name; }
            public void Greet() { }
            public int? Bonus() { return 10; }
        }

        int? PropagateDemo(Person? p) {
            int bonus = p?.Bonus()?;
            return bonus + 1;
        }

        void Main() {
            Person? p = new Person("Ada", null);
            string? bossName = p?.Boss?.Name;
            string fallback = bossName ?? "none";
            string? optChain = p?.Name ?? p?.Boss?.Name;
            int? b1 = p?.Bonus();
            p?.Greet();

            Point? pt = Point { X: 3 };
            int? px = pt?.X;

            INamed? named = new Person("Grace", null);
            string? iname = named?.GetName();

            int? castVal = (int?)5;
            int? castFromByte = (int?)(byte)4;
            int?? doubleOpt = (int??)null;
            int?[] zeroArr = new int?[0];

            bool isNull = p == null;
            bool notNull = null != p;
        }
    "#;
    let compilation = compile(source).expect("all nullable operators should compile to MIR");
    let main = mir_function(&compilation, "Main");
    assert!(
        main.blocks.len() > 4,
        "short-circuiting ?? and ?. should lower to branching MIR blocks"
    );
}

#[test]
fn rejects_flow_narrowing_after_null_checks() {
    rejects_any(
        r#"
        void Main() {
            string? s = "hi";
            if (s != null) {
                int len = s.Length;
            }
        }
        "#,
        "Length",
    );
    rejects_any(
        r#"
        void Main() {
            string? s = "hi";
            if (s == null) {
                return;
            } else {
                int len = s.Length;
            }
        }
        "#,
        "Length",
    );
    rejects_any(
        r#"
        void Main() {
            string? s = "hi";
            bool b = s != null && s.Length > 0;
        }
        "#,
        "Length",
    );
}

#[test]
fn rejects_invalid_null_literals_void_nullable_and_operators() {
    rejects(
        "void Main() { var x = null; }",
        "A2392",
        "`null` requires an optional type",
    );
    rejects(
        "void Main() { int x = null; }",
        "A2392",
        "`null` requires an optional type",
    );
    rejects(
        "void Main() { bool b = 5 == null; }",
        "A2392",
        "requires an optional type",
    );
    rejects(
        "void Main() { bool b = null == null; }",
        "A2392",
        "requires an optional type",
    );
    rejects(
        "void? Bad() { return null; }",
        "A2478",
        "`void` cannot be nullable",
    );
    rejects(
        "using aster.core;\nOption<void> Bad() { return Option<void>.None; }",
        "A2478",
        "`void` cannot be nullable",
    );
    rejects(
        "void Main() { int x = 5 ?? 1; }",
        "A2393",
        "`??` requires an optional left operand",
    );
    rejects(
        r#"void Main() { int? x = null; var y = x ?? "bad"; }"#,
        "A2394",
        "incompatible fallback type",
    );
    rejects(
        r"class C { public int X = 1; }
        void Main() { var c = new C(); var x = c?.X; }",
        "A2395",
        "`?.` requires an optional receiver",
    );
    rejects(
        r"class C { public void Ping() {} }
        void Main() { C? c = new C(); var x = c?.Ping(); }",
        "A2396",
        "with a `void` member can only be used as a statement",
    );
    rejects(
        r"class C { public void Ping() {} }
        void Main() { C? c = new C(); var x = c?.Ping() ?? 1; }",
        "A2396",
        "with a `void` member can only be used as a statement",
    );
    rejects(
        r"class C { public int X = 1; }
        void Main() { C? c = new C(); c?.X = 2; }",
        "A2397",
        "null-conditional expression cannot be",
    );
    rejects(
        r"class C { public int X = 1; }
        void Main() { C? c = new C(); c?.X++; }",
        "A2397",
        "null-conditional expression cannot be",
    );
    rejects_any("void Main() { int?[] a = new int?[3]; }", "default value");
    rejects_any(
        r"class C { public bool Ok() { return true; } }
        void Main() { C? c = new C(); if (c?.Ok()) {} }",
        "bool",
    );
}

#[test]
fn null_conditional_specializes_option_for_builtins_without_prior_mention() {
    // Each snippet avoids naming the resulting Option<U> anywhere else in the program.
    compile(
        r#"
        void Main() {
            Dictionary<string, double>? d = new Dictionary<string, double>();
            var added = d?.Add("x", 42.0);
            var updated = d?.Set("x", 43.0);
            d?.Clear();
        }
        "#,
    )
    .expect("Dictionary?.Add/Set/Clear should specialize Option<bool>");

    compile(
        r"
        void Main() {
            Dictionary<string, double>? d = new Dictionary<string, double>();
            var cap = d?.EnsureCapacity(16);
        }
        ",
    )
    .expect("Dictionary?.EnsureCapacity should specialize Option<int>");

    compile(
        r#"
        void Main() {
            Dictionary<string, double>? d = new Dictionary<string, double>();
            var v1 = d?.GetOr("x", 1.5);
            var v2 = d?.GetOrAdd("y", 2.5);
        }
        "#,
    )
    .expect("Dictionary?.GetOr/GetOrAdd should specialize Option<double>");

    compile(
        r"
        void Main() {
            List<double>? list = new List<double>();
            list?.Add(1.5);
            var cap = list?.EnsureCapacity(8);
        }
        ",
    )
    .expect("List?.EnsureCapacity should specialize Option<int>");

    compile(
        r"
        void Main() {
            List<double>? list = new List<double>();
            list?.Add(1.5);
            var slice = list?.GetRange(0, 1);
        }
        ",
    )
    .expect("List?.GetRange should specialize Option<double[]>");

    compile(
        r"
        void Main() {
            int? x = 42;
            var s = x?.ToString();
        }
        ",
    )
    .expect("int?.ToString() should specialize Option<string>");

    compile(
        r"
        void Main() {
            bool? b = true;
            var s = b?.ToString();
        }
        ",
    )
    .expect("bool?.ToString() should specialize Option<string>");
}

#[test]
fn null_conditional_specializes_option_for_inherited_members_without_prior_mention() {
    compile(
        r"
        class Base {
            public double BaseVal;
            public double BaseProp { get; }
            public Base(double val) {
                BaseVal = val;
                BaseProp = val + 1.0;
            }
            public double GetVal() { return BaseVal; }
        }
        class Derived : Base {
            public Derived(double val) : base(val) {}
        }
        void Main() {
            Derived? d = new Derived(42.0);
            var x = d?.BaseVal;
            var p = d?.BaseProp;
            var y = d?.GetVal();
        }
        ",
    )
    .expect("Derived?.BaseVal/BaseProp/GetVal() should specialize Option<double>");

    compile(
        r"
        interface ISuper {
            double GetVal();
        }
        interface ISub : ISuper {
            int GetCode();
        }
        class Impl : ISub {
            public Impl() {}
            public double GetVal() { return 42.0; }
            public int GetCode() { return 7; }
        }
        void Main() {
            ISub? s = new Impl();
            var z = s?.GetVal();
        }
        ",
    )
    .expect("ISub?.GetVal() should specialize Option<double>");

    compile(
        r"
        void Main() {
            Derived? d = new Derived(42.0);
            var x = d?.BaseVal;
            var y = d?.GetVal();
        }
        class Base<T> {
            public T BaseVal;
            public Base(T val) { BaseVal = val; }
            public T GetVal() { return BaseVal; }
        }
        class Derived : Base<double> {
            public Derived(double val) : base(val) {}
        }
        ",
    )
    .expect("Derived : Base<double> forward reference should specialize Option<double>");
}

#[test]
fn as_and_is_disambiguate_with_null_coalesce_in_compiler() {
    compile(
        r"
        class Base {
            public int Tag;
            public Base(int tag) { Tag = tag; }
        }
        class Sub : Base {
            public Sub() : base(42) {}
        }
        void Main() {
            Base b = new Sub();
            Sub s = b as Sub ?? new Sub();
            Sub fallback = new Sub();
            Sub s2 = b as Sub ?? fallback;
        }
        ",
    )
    .expect("`b as Sub ?? new Sub()` and `b as Sub ?? fallback` should compile");

    rejects(
        r"
        class A {}
        void Main() {
            A a = new A();
            bool ok = a is A ?? false;
        }
        ",
        "A2393",
        "`??` requires an optional left operand",
    );

    rejects(
        r"
        class A {}
        void Main() {
            A a = new A();
            bool ok = a is A??;
        }
        ",
        "A2370",
        "type pattern target must be a class",
    );
}

#[test]
fn null_conditional_discovers_option_from_constructors_and_mixed_conditionals() {
    compile(
        r#"
        using aster.core;
        public class P {
            public static int Main() {
                return Option<string>.Some("abcd")?.Length ?? -1;
            }
        }
        "#,
    )
    .expect("Option<string>.Some(...)?.Length should specialize Option<int>");

    compile(
        r"
        using aster.core;
        public class P {
            public static int Main() {
                return Option<string>.None?.Length ?? 99;
            }
        }
        ",
    )
    .expect("Option<string>.None?.Length should specialize Option<int>");

    compile(
        r#"
        using aster.core;
        public class P {
            public static int Main() {
                return (Option<int>.Some(5)?.ToString() ?? "").Length;
            }
        }
        "#,
    )
    .expect("Option<int>.Some(5)?.ToString() should specialize Option<string>");

    compile(
        r#"
        public class P {
            public static int Main() {
                bool f = true;
                string? b = "ab";
                return (f ? "xyz" : b)?.Length ?? -1;
            }
        }
        "#,
    )
    .expect("(f ? \"xyz\" : b)?.Length should specialize Option<int>");

    compile(
        r#"
        public class P {
            public static int Main() {
                bool f = false;
                string? b = "ab";
                return (f ? b : "xyz")?.Length ?? -1;
            }
        }
        "#,
    )
    .expect("(f ? b : \"xyz\")?.Length should specialize Option<int>");

    compile(
        r#"
        using aster.core;
        public class P {
            public static int Main() {
                Option<string> opt = Option<string>.Some("abc");
                string? b = "xy";
                return (opt switch {
                    Some(s) => s,
                    None => b
                })?.Length ?? -1;
            }
        }
        "#,
    )
    .expect("switch mixing T and T? should specialize Option<int> on ?.");
}
