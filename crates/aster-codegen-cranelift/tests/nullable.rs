//! End-to-end Cranelift JIT tests for M3 (`T?`, target-typed `null`,
//! short-circuiting `??`, flattening/statement-void `?.`, `== null`/`!= null`,
//! null-pointer niche `Option<Ref>` vs tagged `Option<Option<Ref>>`,
//! `Dictionary.TryGet`, `ReadLine`, and `Task.Run` transfers).

#![allow(clippy::needless_raw_string_hashes)]

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute, execute_with_console};
use aster_compiler::compile_project;
use aster_runtime::MemoryConsoleBackend;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-nullable-jit-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary source");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    execute(&compilation?.compilation.mir, function).map_err(|error| error.to_string())
}

fn int(source: &str, function: &str) -> i32 {
    match run(source, function) {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected Int for `{function}`, got {other:?}"),
    }
}

fn boolean(source: &str, function: &str) -> bool {
    match run(source, function) {
        Ok(ExecutionValue::Bool(value)) => value,
        other => panic!("expected Bool for `{function}`, got {other:?}"),
    }
}

#[test]
fn nullable_value_and_reference_basics_and_overloads_execute() {
    let source = r#"
        int Pick(int x) { return 10; }
        int Pick(int? x) { return 20 + (x ?? 5); }

        public int Main() {
            int? a = null;
            int? b = 7;
            int c = a ?? 3;
            int d = b ?? 99;
            int exact = Pick(1);
            int someArg = Pick(b);
            int nullArg = Pick(null);
            int? chained = a ?? b ?? 100;
            int finalVal = chained ?? 0;
            return c + d + exact + someArg + nullArg + finalVal;
        }
    "#;
    // 3 + 7 + 10 + 27 + 25 + 7 = 79
    assert_eq!(int(source, "Main"), 79);
}

#[test]
fn short_circuit_evaluates_lhs_and_receiver_once_and_skips_dead_branches() {
    let source = r#"
        public class Box {
            public int Value;
            public Box? Next;
            public Box(int value, Box? next) {
                Value = value;
                Next = next;
            }
        }

        public class Counter {
            public int LhsCalls;
            public int FallbackCalls;
            public int ReceiverCalls;
            public int StepCalls;

            public int? GetOpt(bool some) {
                LhsCalls += 1;
                return some ? (int?)40 : null;
            }

            public int Fallback() {
                FallbackCalls += 1;
                return 9;
            }

            public Box? GetBox(Box? b) {
                ReceiverCalls += 1;
                return b;
            }

            public int Bump(Box b) {
                StepCalls += 1;
                return b.Value;
            }
        }

        public int Main() {
            Counter c = new Counter();
            int hit = c.GetOpt(true) ?? c.Fallback();
            int miss = c.GetOpt(false) ?? c.Fallback();
            int? someVal = c.GetBox(new Box(5, null))?.Value;
            int? noneVal = c.GetBox(null)?.Value;
            int direct = (someVal ?? 0) + (noneVal ?? 1);
            // Expected:
            // hit = 40 (Lhs=1, Fallback=0)
            // miss = 9 (Lhs=2, Fallback=1)
            // direct = 6 (Receiver=2)
            // total = 40 + 9 + 6 + 2*1000 + 1*10000 + 2*100000 = 212055
            return hit + miss + direct + c.LhsCalls * 1000 + c.FallbackCalls * 10000 + c.ReceiverCalls * 100000;
        }
    "#;
    assert_eq!(int(source, "Main"), 212_055);
}

#[test]
fn null_conditional_flattening_and_statement_void_execute() {
    let source = r#"
        public class Node {
            public int Score;
            public Node? Next;

            public Node(int score, Node? next) {
                Score = score;
                Next = next;
            }

            public Node? Step() {
                return Next;
            }

            public void AddToScore(int delta) {
                Score += delta;
            }
        }

        public int Main() {
            Node leaf = new Node(12, null);
            Node mid = new Node(4, leaf);
            Node? top = new Node(1, mid);
            Node? empty = null;

            int s1 = top?.Next?.Next?.Score ?? -1;
            int s2 = top?.Step()?.Step()?.Score ?? -1;
            int s3 = empty?.Next?.Score ?? 100;
            int s4 = top?.Next?.Next?.Next?.Score ?? 200;

            Node? target = leaf;
            target?.AddToScore(5);
            empty?.AddToScore(99);

            return s1 + s2 + s3 + s4 + leaf.Score;
        }
    "#;
    // 12 + 12 + 100 + 200 + 17 = 341
    assert_eq!(int(source, "Main"), 341);
}

#[test]
fn nested_optional_class_and_string_preserve_none_vs_some_none_vs_some_some() {
    let source = r#"
        using aster.core;

        public class Item {
            public int Id;
            public Item(int id) { Id = id; }
        }

        int ClassifyClass(Item?? v) {
            switch (v) {
                case None:
                    return 1;
                case Some(inner):
                    switch (inner) {
                        case None:
                            return 2;
                        case Some(item):
                            return 100 + item.Id;
                    }
            }
        }

        int ClassifyString(string?? v) {
            switch (v) {
                case None:
                    return 10;
                case Some(inner):
                    switch (inner) {
                        case None:
                            return 20;
                        case Some(text):
                            return 30 + text.Length;
                    }
            }
        }

        public int Main() {
            Item?? cNone = Option<Option<Item>>.None;
            Item?? cSomeNone = Option<Option<Item>>.Some(null);
            Item?? cSomeSome = Option<Option<Item>>.Some(Option<Item>.Some(new Item(7)));

            string?? sNone = null;
            string?? sSomeNone = Option<string?>.Some(null);
            string?? sSomeSome = Option<string?>.Some("abcd");

            return ClassifyClass(cNone) * 1
                + ClassifyClass(cSomeNone) * 10
                + ClassifyClass(cSomeSome) * 100
                + ClassifyString(sNone)
                + ClassifyString(sSomeNone)
                + ClassifyString(sSomeSome);
        }
    "#;
    // 1 + 20 + 10700 + 10 + 20 + 34 = 10785
    assert_eq!(int(source, "Main"), 10_785);
}

#[test]
fn niche_option_equality_handles_null_and_non_null_strings_and_classes() {
    let source = r#"
        public class Tag {
            public int X;
            public Tag(int x) { X = x; }
        }

        public bool CheckAll() {
            string? sNull1 = null;
            string? sNull2 = null;
            string? sA1 = "hello";
            string? sA2 = "hel" + "lo";
            string? sB = "world";

            if (!(sNull1 == sNull2)) { return false; }
            if (sNull1 != sNull2) { return false; }
            if (sNull1 == sA1) { return false; }
            if (sA1 == sNull1) { return false; }
            if (!(sA1 == sA2)) { return false; }
            if (sA1 == sB) { return false; }
            if (!(sA1 == "hello")) { return false; }
            if (sNull1 == "hello") { return false; }
            if (!(sNull1 == null)) { return false; }
            if (!(sA1 != null)) { return false; }

            Tag t1 = new Tag(1);
            Tag t2 = new Tag(1);
            Tag? ot1 = t1;
            Tag? ot1b = t1;
            Tag? ot2 = t2;
            Tag? oNull = null;

            if (!(oNull == null)) { return false; }
            if (!(ot1 == ot1b)) { return false; }
            if (ot1 == ot2) { return false; }
            if (ot1 == oNull) { return false; }
            if (!(ot1 == t1)) { return false; }
            return true;
        }
    "#;
    assert!(boolean(source, "CheckAll"));
}

#[test]
fn dictionary_try_get_on_reference_values_uses_niche_without_clobbering_pointers() {
    let source = r#"
        using aster.collections;

        public class Record {
            public int Code;
            public Record(int code) { Code = code; }
        }

        public int Main() {
            Dictionary<string, Record> map = new Dictionary<string, Record>();
            map.Add("alpha", new Record(42));
            Record? hit = map.TryGet("alpha");
            Record? miss = map.TryGet("missing");
            int hitCode = hit?.Code ?? -1;
            int missCode = miss?.Code ?? 99;

            Dictionary<string, string> labels = new Dictionary<string, string>();
            labels.Add("k", "aster");
            string? sHit = labels.TryGet("k");
            string? sMiss = labels.TryGet("nope");
            int lenHit = sHit?.Length ?? -1;
            int lenMiss = sMiss?.Length ?? 10;

            return hitCode + missCode + lenHit + lenMiss;
        }
    "#;
    // 42 + 99 + 5 + 10 = 156
    assert_eq!(int(source, "Main"), 156);
}

#[test]
fn console_read_line_works_with_niche_option_string() {
    let source = r#"
        using aster.io;

        public int Main() {
            string? first = ReadLine();
            string? second = ReadLine();
            int len1 = first?.Length ?? -1;
            int len2 = second?.Length ?? 100;
            bool secondIsNull = second == null;
            return len1 + len2 + (secondIsNull ? 1 : 0);
        }
    "#;
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-nullable-io-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary source");
    let compilation = compile_project(&path).expect("compile");
    std::fs::remove_file(&path).ok();

    let backend = MemoryConsoleBackend::new(b"hello\n".to_vec());
    let result = execute_with_console(&compilation.compilation.mir, "Main", Box::new(backend))
        .expect("execute");
    assert_eq!(result, ExecutionValue::Int(106));
}

#[test]
fn task_run_transfers_niche_and_nested_optionals() {
    let source = r#"
        using aster.core;
        using aster.collections;

        string? WorkerEcho(string? input) {
            switch (input) {
                case Some(text): return text + "!";
                case None: return null;
            }
        }

        string?? WorkerNested(string?? input) {
            return input;
        }

        int WorkerDict(Dictionary<string, string?> dict) {
            string? a = dict.TryGet("a") ?? null;
            string? b = dict.TryGet("b") ?? null;
            return (a?.Length ?? 10) + (b?.Length ?? 20);
        }

        public int Main() {
            string? s1 = Task.Run(WorkerEcho, "hi").Wait();
            string? s2 = Task.Run(WorkerEcho, null).Wait();

            string?? n1 = Task.Run(WorkerNested, Option<string?>.Some("ok")).Wait();
            string?? n2 = Task.Run(WorkerNested, Option<string?>.Some(null)).Wait();
            string?? n3 = Task.Run(WorkerNested, (string??)null).Wait();

            Dictionary<string, string?> d = new Dictionary<string, string?>();
            d.Add("a", "xyz");
            d.Add("b", null);
            int dSum = Task.Run(WorkerDict, d).Wait();

            int checkN1 = 0;
            switch (n1) {
                case Some(inner): checkN1 = inner?.Length ?? 0;
                case None: checkN1 = -100;
            }
            int checkN2 = 0;
            switch (n2) {
                case Some(inner): checkN2 = inner == null ? 5 : -100;
                case None: checkN2 = -100;
            }
            int checkN3 = n3 == null ? 7 : -100;

            return (s1?.Length ?? -100)
                + (s2 == null ? 1 : -100)
                + checkN1 + checkN2 + checkN3 + dSum;
        }
    "#;
    // s1="hi!" (3) + s2=null (1) + checkN1=2 + checkN2=5 + checkN3=7 + dSum=(3+20)=23 -> 41
    assert_eq!(int(source, "Main"), 41);
}

#[test]
fn null_conditional_dictionary_methods_execute_without_prior_option_mention() {
    let bool_source = r#"
        public int Main() {
            Dictionary<string, double>? d = new Dictionary<string, double>();
            Dictionary<string, double>? none = null;
            bool added = d?.Add("x", 42.0) ?? false;
            bool updated = d?.Set("x", 43.0) ?? false;
            bool missed = none?.Add("x", 1.0) ?? false;
            return (added ? 10 : 0) + (updated ? 20 : 0) + (missed ? 100 : 1);
        }
    "#;
    assert_eq!(int(bool_source, "Main"), 31);

    let cap_source = r#"
        public int Main() {
            Dictionary<string, double>? d = new Dictionary<string, double>();
            Dictionary<string, double>? none = null;
            int c1 = d?.EnsureCapacity(16) ?? -1;
            int c2 = none?.EnsureCapacity(16) ?? 5;
            return (c1 >= 16 ? 40 : 0) + c2;
        }
    "#;
    assert_eq!(int(cap_source, "Main"), 45);

    let val_source = r#"
        public int Main() {
            Dictionary<string, double>? d = new Dictionary<string, double>();
            d?.Add("x", 10.0);
            Dictionary<string, double>? none = null;
            double v1 = d?.GetOr("x", 1.0) ?? -1.0;
            double v2 = d?.GetOrAdd("y", 20.0) ?? -1.0;
            double v3 = d?.GetOr("y", 99.0) ?? -1.0;
            double v4 = none?.GetOr("x", 1.0) ?? 2.0;
            d?.Clear();
            return (int)(v1 + v2 + v3 + v4) + (d?.Length ?? 99);
        }
    "#;
    // 10 + 20 + 20 + 2 + 0 = 52
    assert_eq!(int(val_source, "Main"), 52);
}

#[test]
fn null_conditional_inherited_members_and_primitive_to_string_execute() {
    let derived_source = r#"
        public class Base {
            public double BaseVal;
            public double BaseProp { get; }
            public Base(double val) {
                BaseVal = val;
                BaseProp = val + 2.0;
            }
            public double GetVal() { return BaseVal + 3.0; }
        }

        public class Derived : Base {
            public Derived(double val) : base(val) {}
        }

        public int Main() {
            Derived? some = new Derived(10.0);
            Derived? none = null;
            double f = some?.BaseVal ?? -1.0;
            double p = some?.BaseProp ?? -1.0;
            double m = some?.GetVal() ?? -1.0;
            double n = none?.GetVal() ?? 7.0;
            return (int)(f + p + m + n);
        }
    "#;
    // 10 + 12 + 13 + 7 = 42
    assert_eq!(int(derived_source, "Main"), 42);

    let iface_source = r#"
        public interface ISuper {
            double GetVal();
        }

        public interface ISub : ISuper {
            int GetCode();
        }

        public class Impl : ISub {
            public Impl() {}
            public double GetVal() { return 35.0; }
            public int GetCode() { return 7; }
        }

        public int Main() {
            ISub? some = new Impl();
            ISub? none = null;
            double v1 = some?.GetVal() ?? -1.0;
            double v2 = none?.GetVal() ?? 7.0;
            return (int)(v1 + v2);
        }
    "#;
    assert_eq!(int(iface_source, "Main"), 42);

    let int_to_string_source = r#"
        public int Main() {
            int? x = 1234;
            int? y = null;
            string s1 = x?.ToString() ?? "";
            string s2 = y?.ToString() ?? "fallback";
            return s1.Length * 10 + s2.Length;
        }
    "#;
    // 4 * 10 + 8 = 48
    assert_eq!(int(int_to_string_source, "Main"), 48);

    let bool_to_string_source = r#"
        public int Main() {
            bool? b = true;
            string s = b?.ToString() ?? "";
            return s.Length;
        }
    "#;
    assert_eq!(int(bool_to_string_source, "Main"), 4);
}

#[test]
fn as_pattern_null_coalesce_executes_and_short_circuits_fallback() {
    let source = r#"
        public class Base {
            public int Kind;
            public Base(int kind) { Kind = kind; }
        }

        public class Sub : Base {
            public int Value;
            public Sub(int value) : base(1) { Value = value; }
        }

        public class Other : Base {
            public Other() : base(2) {}
        }

        public class Tracker {
            public int FallbackBuilt;
            public Sub MakeFallback() {
                FallbackBuilt += 1;
                return new Sub(99);
            }
        }

        public int Main() {
            Tracker t = new Tracker();
            Base hitBase = new Sub(42);
            Base missBase = new Other();

            Sub s1 = hitBase as Sub ?? new Sub(999);
            Sub s2 = hitBase as Sub ?? t.MakeFallback();
            Sub s3 = missBase as Sub ?? new Sub(7);
            Sub s4 = missBase as Sub ?? t.MakeFallback();

            // 42 + 42 + 7 + 99 + 1*1000 = 1190
            return s1.Value + s2.Value + s3.Value + s4.Value + t.FallbackBuilt * 1000;
        }
    "#;
    assert_eq!(int(source, "Main"), 1190);
}

#[test]
fn option_constructor_and_mixed_ternary_null_conditional_execute() {
    let some_len = r#"
        using aster.core;
        public int Main() {
            return Option<string>.Some("abcd")?.Length ?? -1;
        }
    "#;
    assert_eq!(int(some_len, "Main"), 4);

    let none_len = r#"
        using aster.core;
        public int Main() {
            return Option<string>.None?.Length ?? 99;
        }
    "#;
    assert_eq!(int(none_len, "Main"), 99);

    let int_to_str = r#"
        using aster.core;
        public int Main() {
            int someLen = (Option<int>.Some(5)?.ToString() ?? "").Length;
            int noneLen = (Option<int>.None?.ToString() ?? "").Length;
            return someLen * 10 + noneLen;
        }
    "#;
    assert_eq!(int(int_to_str, "Main"), 10);

    let ternary_some = r#"
        public int Main() {
            bool f = true;
            string? b = "ab";
            return (f ? "xyz" : b)?.Length ?? -1;
        }
    "#;
    assert_eq!(int(ternary_some, "Main"), 3);

    let ternary_none = r#"
        public int Main() {
            bool f = false;
            string? b = null;
            return (f ? "xyz" : b)?.Length ?? -1;
        }
    "#;
    assert_eq!(int(ternary_none, "Main"), -1);

    let switch_mixed = r#"
        using aster.core;
        public int Main() {
            Option<string> someOpt = Option<string>.Some("hello");
            Option<string> noneOpt = Option<string>.None;
            string? fallback = "xy";
            string? noneFallback = null;
            int s1 = (someOpt switch {
                Some(s) => s,
                None => fallback
            })?.Length ?? -1;
            int s2 = (noneOpt switch {
                Some(s) => s,
                None => noneFallback
            })?.Length ?? 99;
            return s1 * 100 + s2;
        }
    "#;
    assert_eq!(int(switch_mixed, "Main"), 599);
}
