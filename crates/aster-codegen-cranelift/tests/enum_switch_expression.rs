use aster_codegen_cranelift::{ExecutionValue, execute};
use aster_compiler::compile;

fn run(source: &str, function: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, function).map_err(|error| error.to_string())
}

#[test]
fn executes_payload_arms_and_evaluates_the_selected_value_once() {
    let source = r"
        public enum Message { Quit, Move(int x, int y), Write(string text) }
        public class Probe {
            public int Calls;
            public Probe() { Calls = 0; }
            public Message Next() { Calls = Calls + 1; return Message.Move(20, 22); }
            public int Wrong() { Calls = Calls + 1000; return -1; }
        }
        public int Run() {
            Probe probe = new Probe();
            int result = probe.Next() switch {
                Quit => probe.Wrong(),
                Move(x, y) => x + y,
                Write(text) => probe.Wrong(),
            };
            return result + probe.Calls * 100;
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(142)));
}

#[test]
fn executes_default_and_numeric_common_type() {
    let source = r"
        public enum State { Ready, Waiting, Failed }
        public long Read(State value) {
            return value switch { Ready => 1, default => 41L, };
        }
        public long Run() { return Read(State.Failed); }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Long(41)));
}

#[test]
fn preserves_an_escaping_string_result_after_later_allocations() {
    let source = r#"
        public enum Selection { Keep(string value), Empty }
        public string Pick(Selection selection) {
            return selection switch {
                Keep(value) => value + "-kept",
                Empty => "empty",
            };
        }
        public string Run() {
            string result = Pick(Selection.Keep("payload"));
            string noise = "";
            for (int index = 0; index < 128; index = index + 1) {
                noise = noise + "x";
            }
            return result;
        }
    "#;
    assert_eq!(
        run(source, "Run"),
        Ok(ExecutionValue::String("payload-kept".to_string()))
    );
}

#[test]
fn executes_generic_enum_switch_as_a_function_argument() {
    let source = r"
        public enum Choice<T> { Some(T value), None }
        public int Twice(int value) { return value * 2; }
        public int Run() {
            return Twice(Choice<int>.Some(21) switch {
                Some(value) => value,
                None => 0,
            });
        }
    ";
    assert_eq!(run(source, "Run"), Ok(ExecutionValue::Int(42)));
}
