//! T-OBJ-5 (object model B2): constructor overloads and `this(...)`
//! delegation. Compile-time rules, the shape of the lowered MIR (initializers
//! once, in the constructor that does not delegate) and the negative cases.
//! Execution order is observed in
//! `aster-codegen-cranelift/tests/constructor_overloads.rs`.

use aster_mir as mir;

fn compile(source: &str) -> aster_compiler::Compilation {
    aster_compiler::compile(source)
        .unwrap_or_else(|diagnostics| panic!("expected a clean compile, got {diagnostics:#?}"))
}

fn rejects(source: &str, expected: &str) {
    let Err(diagnostics) = aster_compiler::compile(source) else {
        panic!("expected a diagnostic containing `{expected}` for:\n{source}");
    };
    let messages = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>();
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "expected a diagnostic containing `{expected}`, found {messages:#?}"
    );
}

/// The constructors of class `name`, in source order, with their parameter
/// count excluding the receiver.
fn constructors<'a>(
    compilation: &'a aster_compiler::Compilation,
    name: &str,
) -> Vec<(usize, &'a mir::Function)> {
    let class = compilation
        .mir
        .classes
        .iter()
        .find(|class| class.name == name)
        .expect("class");
    compilation
        .mir
        .functions
        .iter()
        .filter(|function| function.constructor && function.owner == Some(class.symbol))
        .map(|function| (function.parameters.len() - 1, function))
        .collect()
}

fn field_stores(function: &mir::Function) -> Vec<usize> {
    function.blocks[0]
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, instruction)| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    target: mir::Place::ObjectField { .. },
                    ..
                }
            )
        })
        .map(|(index, _)| index)
        .collect()
}

fn calls_a_constructor(
    compilation: &aster_compiler::Compilation,
    function: &mir::Function,
) -> Vec<usize> {
    function.blocks[0]
        .instructions
        .iter()
        .enumerate()
        .filter(|(_, instruction)| {
            matches!(instruction, mir::Instruction::Call { function, .. }
                if compilation.mir.functions.iter().any(|f| f.symbol == *function && f.constructor))
        })
        .map(|(index, _)| index)
        .collect()
}

// ---------------------------------------------------------------------------
// Positive shapes
// ---------------------------------------------------------------------------

#[test]
fn a_class_may_declare_several_constructors() {
    let compilation = compile(
        "public class C { public int v; public C() {} public C(int a) { v = a; } \
         public C(string s, int a) { v = a; } }",
    );
    let arities = constructors(&compilation, "C")
        .iter()
        .map(|(arity, _)| *arity)
        .collect::<Vec<_>>();
    assert_eq!(arities, [0, 1, 2]);
}

#[test]
fn initializers_are_emitted_once_in_the_constructor_that_does_not_delegate() {
    let compilation = compile(
        "public class C { public int a = 1; public int b; \
         public C() {} \
         public C(int x) : this() { b = x; } }",
    );
    let all = constructors(&compilation, "C");
    let plain = all.iter().find(|(arity, _)| *arity == 0).expect("C()").1;
    let delegating = all.iter().find(|(arity, _)| *arity == 1).expect("C(int)").1;

    // `C()` runs the initializer `a = 1`; nothing else.
    assert_eq!(field_stores(plain).len(), 1);
    assert!(calls_a_constructor(&compilation, plain).is_empty());

    // `C(int)` calls its target first and only then stores `b = x`: it has
    // no initializer store of its own.
    let calls = calls_a_constructor(&compilation, delegating);
    let stores = field_stores(delegating);
    assert_eq!(calls.len(), 1, "one delegation call");
    assert_eq!(stores.len(), 1, "only the body store, no initializer");
    assert!(calls[0] < stores[0], "the target runs before the body");
}

#[test]
fn a_chain_of_three_runs_the_initializers_in_the_last_constructor_only() {
    let compilation = compile(
        "public class C { public int a = 1; public int b = 2; \
         public C(int x, int y, int z) {} \
         public C(int x, int y) : this(x, y, 0) {} \
         public C(int x) : this(x, 0) {} }",
    );
    let all = constructors(&compilation, "C");
    let stores =
        |arity: usize| field_stores(all.iter().find(|(a, _)| *a == arity).expect("ctor").1).len();
    assert_eq!(stores(3), 2, "both initializers");
    assert_eq!(stores(2), 0);
    assert_eq!(stores(1), 0);
}

#[test]
fn a_derived_chain_reaches_the_base_constructor_once() {
    let compilation = compile(
        "public class A { public A() {} public A(int x) {} } \
         public class B : A { public int f = 1; \
           public B(int x) : base(x) {} \
           public B() : this(0) {} }",
    );
    let all = constructors(&compilation, "B");
    let with_base = all.iter().find(|(arity, _)| *arity == 1).expect("B(int)").1;
    let delegating = all.iter().find(|(arity, _)| *arity == 0).expect("B()").1;
    // Both call exactly one constructor; only `B(int)` calls the base one
    // and stores the initializer.
    assert_eq!(calls_a_constructor(&compilation, with_base).len(), 1);
    assert_eq!(field_stores(with_base).len(), 1);
    assert_eq!(calls_a_constructor(&compilation, delegating).len(), 1);
    assert!(field_stores(delegating).is_empty());
}

#[test]
fn a_class_with_an_explicit_constructor_has_no_implicit_default() {
    rejects(
        "public class C { public int v; public C(int a) { v = a; } } \
         public int Run() { C c = new C(); return c.v; }",
        "missing required argument",
    );
    // Without any constructor the implicit default still exists.
    compile("public class D { public int v; } public int Run() { D d = new D(); return d.v; }");
}

#[test]
fn a_derived_constructor_may_pick_a_base_overload_and_the_base_default() {
    compile(
        "public class A { public int x; public A() { x = 1; } public A(int v) { x = v; } } \
         public class B : A { public B() {} public B(int v) : base(v) {} } \
         public class C : A { public C() : base() {} }",
    );
}

#[test]
fn a_delegating_constructor_may_assign_get_only_auto_properties() {
    compile(
        "public class P { public int W { get; } public int H { get; init; } \
         public P(int w) { W = w; H = 1; } \
         public P() : this(1) { W = 2; H = 3; } }",
    );
}

#[test]
fn a_private_constructor_is_a_candidate_only_inside_its_class() {
    compile("public class C { private C(int a, int b) {} public C(int a) : this(a, 1) {} }");
    rejects(
        "public class C { private C(int a, int b) {} public C(int a) : this(a, 1) {} } \
         public void Run() { C c = new C(1, 2); }",
        "too many arguments",
    );
    rejects(
        "public class C { private C(int a) {} } public void Run() { C c = new C(1); }",
        "constructor `C` is private",
    );
}

// ---------------------------------------------------------------------------
// Negative cases
// ---------------------------------------------------------------------------

#[test]
fn a_direct_delegation_cycle_is_rejected() {
    rejects(
        "public class C { public C() : this() {} }",
        "in a cycle through `this(...)`",
    );
    rejects(
        "public class C { public C(int x) : this(x) {} public C() {} }",
        "in a cycle through `this(...)`",
    );
}

#[test]
fn an_indirect_delegation_cycle_is_rejected() {
    rejects(
        "public class C { public C(int x) : this(\"a\") {} \
         public C(string s) : this(1, 2) {} \
         public C(int a, int b) : this(1) {} }",
        "in a cycle through `this(...)`",
    );
}

#[test]
fn a_chain_that_ends_is_not_a_cycle() {
    compile(
        "public class C { public C(int a, int b) {} \
         public C(int a) : this(a, 1) {} public C() : this(0) {} }",
    );
}

#[test]
fn this_and_base_on_the_same_constructor_are_rejected() {
    rejects(
        "public class A { public A() {} } \
         public class B : A { public B() {} public B(int x) : this() : base() {} }",
        "only one initializer",
    );
    rejects(
        "public class A { public A() {} } \
         public class B : A { public B() {} public B(int x) : base() : this() {} }",
        "only one initializer",
    );
}

#[test]
fn an_ambiguous_overload_is_an_error() {
    rejects(
        "public class C { public C(int a, long b) {} public C(long a, int b) {} } \
         public void Run() { C c = new C(1, 2); }",
        "call to `C` is ambiguous",
    );
    // The same rule applies to `this(...)` and `base(...)`.
    rejects(
        "public class C { public C(int a, long b) {} public C(long a, int b) {} \
         public C() : this(1, 2) {} }",
        "call to `C` is ambiguous",
    );
    rejects(
        "public class A { public A(int a, long b) {} public A(long a, int b) {} } \
         public class B : A { public B() : base(1, 2) {} }",
        "call to `A` is ambiguous",
    );
}

#[test]
fn no_matching_constructor_is_an_error() {
    rejects(
        "public class C { public C(int a) {} public C(string s) {} } \
         public void Run() { C c = new C(true); }",
        "no overload of `C` accepts these arguments",
    );
    rejects(
        "public class C { public C(int a) {} public C(string s) {} } \
         public void Run() { C c = new C(); }",
        "no overload of `C` accepts these arguments",
    );
    rejects(
        "public class C { public C(int a) {} public C(string s) {} public C() : this(true) {} }",
        "no overload of `C` accepts these arguments",
    );
}

#[test]
fn this_cannot_be_used_in_the_arguments_of_this_delegation() {
    rejects(
        "public class C { public int v; public C(int a) {} public C() : this(this.v) {} }",
        "`this` cannot be used in the arguments of `this(...)`",
    );
    rejects(
        "public class C { public int v; public C(int a) {} public C() : this(v) {} }",
        "cannot be used in the arguments of `this(...)`",
    );
    // An instance method is not callable either.
    rejects(
        "public class C { public int M() { return 1; } public C(int a) {} \
         public C() : this(M()) {} }",
        "instance method `M` requires an object",
    );
}

#[test]
fn a_duplicate_constructor_signature_is_an_error() {
    rejects(
        "public class C { public C(int a) {} public C(int b) {} }",
        "duplicate constructor `C` with the same parameter types",
    );
}

#[test]
fn this_delegation_is_valid_only_in_a_constructor() {
    rejects(
        "public class C { public C() {} public int M() : this() { return 1; } }",
        "expected `{`",
    );
}

#[test]
fn this_confinement_applies_to_the_constructor_a_delegation_reaches() {
    // Rule B holds in the target: it passes `this` out in its body.
    rejects(
        "public class C { public C() { Take(this); } public C(int x) : this() {} \
         public static void Take(C c) {} }",
        "`this` cannot be passed, stored, returned or converted",
    );
    // The delegating body sees every field of the object as assigned.
    compile(
        "public class C { public string s; public int n; public C(string s) { this.s = s; } \
         public C() : this(\"x\") { n = s.Length; } }",
    );
}
