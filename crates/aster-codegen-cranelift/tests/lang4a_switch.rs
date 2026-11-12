//! T-LANG-4a: `switch` on integer, `char` and `string` subjects.

use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

fn run_int(source: &str) -> i32 {
    match run(source, "Run") {
        Ok(ExecutionValue::Int(value)) => value,
        other => panic!("expected an int result, found {other:?}"),
    }
}

#[test]
fn switches_on_int_with_negative_and_constant_expression_labels() {
    let source = r"
        public int Pick(int value) {
            switch (value) {
                case 1: return 10;
                case -7: return 70;
                case 2 + 3: return 50;
                default: return -1;
            }
        }
        public int Run() {
            return Pick(1) + Pick(-7) + Pick(5) + Pick(99);
        }
    ";
    assert_eq!(run_int(source), 10 + 70 + 50 - 1);
}

#[test]
fn default_must_be_last_like_for_enum_switches() {
    let source = r"
        public int Pick(int value) {
            switch (value) {
                default: return -1;
                case 1: return 10;
            }
        }
        public int Run() { return Pick(1); }
    ";
    assert!(run(source, "Run").is_err());
}

#[test]
fn switches_on_long_byte_and_unsigned_subjects() {
    let source = r"
        public int PickLong(long value) {
            switch (value) {
                case 5000000000L: return 1;
                case 7: return 2;
                default: return 3;
            }
        }
        public int PickByte(byte value) {
            switch (value) {
                case 255: return 1;
                case 0: return 2;
                default: return 3;
            }
        }
        public int PickUInt(uint value) {
            switch (value) {
                case 4000000000u: return 1;
                default: return 3;
            }
        }
        public int Run() {
            byte top = 255;
            byte mid = 100;
            return PickLong(5000000000L) * 1 + PickLong(7) * 10 + PickLong(8) * 100
                + PickByte(top) * 1000 + PickByte(mid) * 10000 + PickUInt(4000000000u) * 100000;
        }
    ";
    assert_eq!(run_int(source), 1 + 20 + 300 + 1000 + 30000 + 100_000);
}

#[test]
fn switches_on_char_including_non_ascii() {
    let source = r"
        public int Classify(char value) {
            switch (value) {
                case 'a': return 1;
                case 'é': return 2;
                case '日': return 3;
                default: return 0;
            }
        }
        public int Run() {
            return Classify('a') + Classify('é') * 10 + Classify('日') * 100 + Classify('z') * 1000;
        }
    ";
    assert_eq!(run_int(source), 1 + 20 + 300);
}

#[test]
fn switches_on_string_ordinally_with_empty_and_non_ascii_cases() {
    let source = r#"
        public int Pick(string text) {
            switch (text) {
                case "": return 1;
                case "abc": return 2;
                case "ABC": return 3;
                case "héllo": return 4;
                case "日本語": return 5;
                default: return 0;
            }
        }
        public int Run() {
            string built = "ab" + "c";
            return Pick("") + Pick(built) * 10 + Pick("ABC") * 100
                + Pick("héllo") * 1000 + Pick("日本語") * 10000 + Pick("hello") * 100000
                + Pick("abcd") * 1000000;
        }
    "#;
    assert_eq!(run_int(source), 1 + 20 + 300 + 4000 + 50000);
}

#[test]
fn string_comparison_is_byte_wise_not_normalized() {
    // Precomposed U+00E9 versus `e` followed by combining U+0301.
    let source = "
        public int Pick(string text) {
            switch (text) {
                case \"\u{e9}\": return 1;
                default: return 2;
            }
        }
        public int Run() { return Pick(\"e\u{301}\") * 10 + Pick(\"\u{e9}\"); }
    ";
    assert_eq!(run_int(source), 21);
}

#[test]
fn constants_label_cases_and_subject_is_evaluated_once() {
    let source = r#"
        const int Low = 1;
        const string Name = "bob";
        public class Probe {
            public int Calls;
            public Probe() { Calls = 0; }
            public int Next() { Calls = Calls + 1; return 2; }
        }
        public int Run() {
            Probe probe = new Probe();
            int result = 0;
            switch (probe.Next()) {
                case Low: result = 1;
                case Low + 1: result = 20;
                default: result = 300;
            }
            switch (Name) {
                case Name: result = result + 4000;
                default: result = result + 50000;
            }
            return result + probe.Calls * 100000;
        }
    "#;
    assert_eq!(run_int(source), 20 + 4000 + 100_000);
}

#[test]
fn cases_do_not_fall_through_and_nest() {
    let source = r"
        public int Run() {
            int total = 0;
            for (int outer = 0; outer < 3; outer = outer + 1) {
                switch (outer) {
                    case 0:
                        total = total + 1;
                    case 1:
                        switch (outer + 10) {
                            case 11: total = total + 20;
                            default: total = total + 400;
                        }
                    default:
                        total = total + 8000;
                }
            }
            return total;
        }
    ";
    assert_eq!(run_int(source), 1 + 20 + 8000);
}

#[test]
fn break_and_continue_inside_switch_target_the_enclosing_loop() {
    let source = r#"
        public int Run() {
            string[] words = ["skip", "add", "add", "stop", "add"];
            int total = 0;
            for (int index = 0; index < 5; index = index + 1) {
                switch (words[index]) {
                    case "skip":
                        continue;
                    case "stop":
                        break;
                    default:
                        total = total + 1;
                }
                total = total + 10;
            }
            return total;
        }
    "#;
    // skip: continue; add: +1 +10; add: +1 +10; stop: break.
    assert_eq!(run_int(source), 22);
}

#[test]
fn switch_expression_on_int_char_and_string() {
    let source = r#"
        public int Number(int value) { return value switch { 1 => 10, 2 => 20, default => 0, }; }
        public int Letter(char value) { return value switch { 'x' => 1, 'y' => 2, default => 3, }; }
        public int Word(string value) { return value switch { "" => 7, "yes" => 1, default => 0, }; }
        public long Widen(byte value) { return value switch { 1 => 1, default => 41L, }; }
        public int Run() {
            return Number(2) + Number(1) * 10 + Number(3) * 100
                + Letter('y') * 1000 + Letter('q') * 10000
                + Word("") * 100000 + Word("yes") * 1000000 + (int)Widen(9);
        }
    "#;
    assert_eq!(
        run_int(source),
        20 + 100 + 2000 + 30000 + 700_000 + 1_000_000 + 41
    );
}

#[test]
fn switch_expression_evaluates_the_subject_once() {
    let source = r"
        public class Probe {
            public int Calls;
            public Probe() { Calls = 0; }
            public int Next() { Calls = Calls + 1; return 3; }
        }
        public int Run() {
            Probe probe = new Probe();
            int result = probe.Next() switch { 1 => 1, 2 => 2, 3 => 30, default => 4, };
            return result + probe.Calls * 100;
        }
    ";
    assert_eq!(run_int(source), 130);
}

#[test]
fn enum_switches_keep_working() {
    let source = r"
        public enum Shape { Circle(int r), Square(int s), Dot }
        public int Area(Shape shape) {
            switch (shape) {
                case Circle(r): return r * 3;
                case Square(s): return s * s;
                case Dot: return 1;
            }
        }
        public int Run() { return Area(Shape.Circle(2)) + Area(Shape.Square(3)) + Area(Shape.Dot); }
    ";
    assert_eq!(run_int(source), 6 + 9 + 1);
}
