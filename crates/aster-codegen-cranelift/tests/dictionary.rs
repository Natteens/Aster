//! End-to-end coverage for the complete nominal native Dictionary milestone.

#![allow(clippy::needless_raw_string_hashes)]

use aster_codegen_cranelift::{ExecutionValue, execute};
use std::sync::atomic::{AtomicU64, Ordering};

use aster_compiler::{compile, compile_project, mir};

fn run(source: &str) -> Result<ExecutionValue, String> {
    let compilation = compile(source).map_err(|diagnostics| format!("{diagnostics:#?}"))?;
    execute(&compilation.mir, "Main").map_err(|error| error.to_string())
}

fn errors(source: &str) -> Vec<String> {
    compile(source)
        .expect_err("source must be rejected")
        .into_iter()
        .map(|diagnostic| diagnostic.message)
        .collect()
}

fn run_project(source: &str) -> Result<ExecutionValue, String> {
    execute(&compile_project_mir(source)?, "Main").map_err(|error| error.to_string())
}

fn project_errors(source: &str) -> String {
    compile_project_mir(source).expect_err("source must be rejected")
}

fn compile_project_mir(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-dictionary-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary Dictionary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    Ok(compilation?.compilation.mir)
}

#[test]
fn empty_dictionary_has_zero_length_for_supported_keys() {
    for source in [
        "public int Main() { Dictionary<bool, int> value = new Dictionary<bool, int>(); return value.Length; }",
        "public int Main() { Dictionary<char, string> value = new Dictionary<char, string>(); return value.Length; }",
        "public int Main() { Dictionary<int, string> value = new Dictionary<int, string>(); return value.Length; }",
        "public int Main() { Dictionary<string, int> value = new Dictionary<string, int>(); return value.Length; }",
        "public int Main() { Dictionary<ulong, List<int>> value = new Dictionary<ulong, List<int>>(); return value.Length; }",
        "public int Main() { Dictionary<sbyte, int> value = new Dictionary<sbyte, int>(); return value.Length; }",
        "public int Main() { Dictionary<byte, int> value = new Dictionary<byte, int>(); return value.Length; }",
        "public int Main() { Dictionary<short, int> value = new Dictionary<short, int>(); return value.Length; }",
        "public int Main() { Dictionary<ushort, int> value = new Dictionary<ushort, int>(); return value.Length; }",
        "public int Main() { Dictionary<uint, int> value = new Dictionary<uint, int>(); return value.Length; }",
        "public int Main() { Dictionary<long, int> value = new Dictionary<long, int>(); return value.Length; }",
    ] {
        assert_eq!(run(source), Ok(ExecutionValue::Int(0)), "{source}");
    }
}

#[test]
fn dictionary_rejects_unsupported_key_types() {
    for source in [
        "public int Main() { Dictionary<float, int> value = new Dictionary<float, int>(); return 0; }",
        "public int Main() { Dictionary<double, int> value = new Dictionary<double, int>(); return 0; }",
        "public struct Key { public int value; } public int Main() { Dictionary<Key, int> value = new Dictionary<Key, int>(); return 0; }",
        "public int Main() { Dictionary<int[], int> value = new Dictionary<int[], int>(); return 0; }",
        "public int Main() { Dictionary<List<int>, int> value = new Dictionary<List<int>, int>(); return 0; }",
        "public enum E { A } public int Main() { Dictionary<E, int> value = new Dictionary<E, int>(); return 0; }",
        "public class Key { public Key() {} } public int Main() { Dictionary<Key, int> value = new Dictionary<Key, int>(); return 0; }",
        "public interface Key { int Get(); } public int Main() { Dictionary<Key, int> value = new Dictionary<Key, int>(); return 0; }",
        "public int Main() { Dictionary<Dictionary<string, int>, int> value = new Dictionary<Dictionary<string, int>, int>(); return 0; }",
        "public int Main() { Dictionary<Option<int>, int> value = new Dictionary<Option<int>, int>(); return 0; }",
        "public int Main() { Dictionary<Result<int, string>, int> value = new Dictionary<Result<int, string>, int>(); return 0; }",
    ] {
        assert!(
            errors(source)
                .iter()
                .any(|error| error.contains("not supported as a Dictionary key")),
            "{source}"
        );
    }
}

#[test]
fn dictionary_name_is_reserved_and_lowering_is_intrinsic() {
    let reserved = errors(
        "namespace user; public class Dictionary<K, V> { public int Length; public Dictionary() {} } public int Main() { return 0; }",
    );
    assert!(
        reserved
            .iter()
            .any(|error| error.contains("reserved for the built-in")),
        "{reserved:?}"
    );
    let compilation = compile("public int Main() { Dictionary<string, int> value = new Dictionary<string, int>(); return value.Length; }").expect("official Dictionary");
    assert!(
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .any(|instruction| matches!(
                instruction,
                aster_mir::Instruction::AllocateDictionary { .. }
            ))
    );
}

#[test]
fn dictionary_accepts_every_supported_concrete_value_shape() {
    for source in [
        "public struct Point { public int X; } public int Main() { Dictionary<string, Point> d = new Dictionary<string, Point>(); return d.Length; }",
        "public enum State { Ready, Done } public int Main() { Dictionary<string, State> d = new Dictionary<string, State>(); return d.Length; }",
        "public class Player { public Player() {} } public int Main() { Dictionary<string, Player> d = new Dictionary<string, Player>(); return d.Length; }",
        "public interface Named { string Name(); } public class Player : Named { public Player() {} public string Name() { return \"player\"; } } public int Main() { Dictionary<string, Named> d = new Dictionary<string, Named>(); return d.Length; }",
        "public int Main() { Dictionary<string, int[]> d = new Dictionary<string, int[]>(); return d.Length; }",
        "public int Main() { Dictionary<string, List<int>> d = new Dictionary<string, List<int>>(); return d.Length; }",
        "using aster.core; public int Main() { Dictionary<string, Option<int>> d = new Dictionary<string, Option<int>>(); return d.Length; }",
        "using aster.core; public int Main() { Dictionary<string, Result<int, string>> d = new Dictionary<string, Result<int, string>>(); return d.Length; }",
        "public int Main() { Dictionary<int, Dictionary<string, string>> d = new Dictionary<int, Dictionary<string, string>>(); return d.Length; }",
    ] {
        assert_eq!(run_project(source), Ok(ExecutionValue::Int(0)), "{source}");
    }
}

#[test]
fn concrete_generic_dictionary_specializations_reach_the_jit() {
    assert_eq!(
        run(
            "public Dictionary<K, int> Create<K>() { return new Dictionary<K, int>(); } public int Main() { return Create<string>().Length; }"
        ),
        Ok(ExecutionValue::Int(0)),
    );
    let errors = errors(
        "public Dictionary<K, int> Create<K>() { return new Dictionary<K, int>(); } public int Main() { return Create<float>().Length; }",
    );
    assert!(
        errors
            .iter()
            .any(|error| error.contains("not supported as a Dictionary key")),
        "{errors:?}"
    );
}

#[test]
fn escape_analysis_selects_dictionary_regions() {
    let local = compile("public int Main() { Dictionary<string, int> value = new Dictionary<string, int>(); return value.Length; }").expect("local dictionary");
    let escaped = compile("public Dictionary<string, int> Create() { return new Dictionary<string, int>(); } public int Main() { return Create().Length; }").expect("escaping dictionary");
    let region = |compilation: &aster_compiler::Compilation| {
        compilation
            .mir
            .functions
            .iter()
            .flat_map(|function| &function.blocks)
            .flat_map(|block| &block.instructions)
            .find_map(|instruction| match instruction {
                aster_mir::Instruction::AllocateDictionary { region, .. } => Some(*region),
                _ => None,
            })
    };
    assert_eq!(region(&local), Some(aster_mir::AllocationRegion::Temporary));
    assert_eq!(
        region(&escaped),
        Some(aster_mir::AllocationRegion::Persistent)
    );
}

#[test]
fn dictionary_core_operations_follow_the_declared_boolean_contracts() {
    let source = r#"
        using aster.core;
        public int Main()
        {
            Dictionary<string, int> values = new Dictionary<string, int>();
            if (!values.Add("aster", 1)) { return 1; }
            if (values.Add("aster", 2)) { return 2; }
            if (values.Length != 1) { return 3; }
            if (!values.ContainsKey("aster")) { return 4; }
            if (values.ContainsKey("missing")) { return 5; }
            if (!values.Set("aster", 42)) { return 6; }
            if (values.Set("new", 7)) { return 7; }
            if (values.Length != 2) { return 8; }
            switch (values.TryGet("aster"))
            {
                case Some(value): if (value != 42) { return 9; }
                case None: return 10;
            }
            switch (values.TryGet("missing"))
            {
                case Some(value): return 11;
                case None:
            }
            if (!values.Remove("aster")) { return 12; }
            if (values.Remove("aster")) { return 13; }
            if (values.Length != 1) { return 14; }
            return 42;
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn dictionary_arguments_are_evaluated_once_from_left_to_right() {
    let source = r#"
        public class Probe
        {
            public int Order;
            private Dictionary<string, int> values;

            public Probe()
            {
                Order = 0;
                values = new Dictionary<string, int>();
            }

            public Dictionary<string, int> GetDictionary()
            {
                Order = Order * 10 + 1;
                return values;
            }

            public string GetKey()
            {
                Order = Order * 10 + 2;
                return "key";
            }

            public int GetValue()
            {
                Order = Order * 10 + 3;
                return 42;
            }
        }

        public int Main()
        {
            Probe probe = new Probe();
            bool inserted = probe.GetDictionary().Add(probe.GetKey(), probe.GetValue());
            return inserted ? probe.Order : 0;
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(123)));
}

#[test]
fn dictionary_api_is_exact_and_dictionary_is_not_directly_iterable() {
    for source in [
        "public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); d.Add(\"a\"); return 0; }",
        "public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); d.Set(\"a\", 1, 2); return 0; }",
        "public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); d.TryGet(1); return 0; }",
        "public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); d.ContainsKey(); return 0; }",
        "public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); d.Remove(1); return 0; }",
        "using aster.collections; public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); d.Entries(1); return 0; }",
        "public int Main() { Dictionary<string, int> d = new Dictionary<string, int>(); foreach (int value in d) { } return 0; }",
    ] {
        assert!(!project_errors(source).is_empty(), "{source}");
    }
}

#[test]
fn dictionary_entries_are_an_insertion_order_snapshot() {
    let source = r#"
        using aster.collections;
        public int Main()
        {
            Dictionary<int, int> values = new Dictionary<int, int>();
            values.Add(2, 20);
            values.Add(1, 10);
            values.Add(3, 30);
            values.Remove(1);
            values.Add(1, 11);
            DictionaryEntry<int, int>[] entries = values.Entries();
            if (entries.Length != 3) { return 1; }
            if (entries[0].Key != 2 || entries[0].Value != 20) { return 2; }
            if (entries[1].Key != 3 || entries[1].Value != 30) { return 3; }
            if (entries[2].Key != 1 || entries[2].Value != 11) { return 4; }
            values.Set(2, 200);
            if (entries[0].Value != 20) { return 5; }
            return 42;
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn dictionary_string_keys_are_ordinal_and_values_are_copied() {
    let source = r#"
        using aster.core;
        public struct Point { public int X; }
        public int Main()
        {
            Dictionary<string, Point> values = new Dictionary<string, Point>();
            values.Add("é", Point { X: 40 });
            values.Add("É", Point { X: 2 });
            Point copy = Point { X: 0 };
            switch (values.TryGet("é"))
            {
                case Some(value): copy = value;
                case None: return 1;
            }
            copy.X = copy.X + 1;
            switch (values.TryGet("é"))
            {
                case Some(value): return value.X + values.Length;
                case None: return 2;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn dictionary_preserves_temporary_string_keys_and_values_after_return() {
    let source = r#"
        using aster.core;

        public Dictionary<string, string> Create()
        {
            Dictionary<string, string> values = new Dictionary<string, string>();
            string key = "--key--".Substring(2, 3);
            string value = "--value--".Substring(2, 5);
            values.Add(key, value);
            return values;
        }

        public int BurnTemporaryMemory()
        {
            int index = 0;
            int total = 0;
            while (index < 2000)
            {
                string text = "--temporary--".Substring(2, 9);
                total = total + text.Length;
                index = index + 1;
            }
            return total;
        }

        public int Main()
        {
            Dictionary<string, string> values = Create();
            int ignored = BurnTemporaryMemory();
            switch (values.TryGet("key"))
            {
                case Some(value): return value == "value" ? 42 : 1;
                case None: return 2;
            }
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn dictionary_headers_escape_through_fields_arrays_lists_options_results_and_values() {
    let source = r#"
        using aster.core;

        public class Holder
        {
            public Dictionary<string, int> Values;
            public Holder() { Values = new Dictionary<string, int>(); }
        }

        public int BurnTemporaryMemory()
        {
            int index = 0;
            int total = 0;
            while (index < 2000)
            {
                string text = "--temporary--".Substring(2, 9);
                total = total + text.Length;
                index = index + 1;
            }
            return total;
        }

        public int Main()
        {
            Holder holder = new Holder();
            holder.Values.Add("holder", 1);

            Dictionary<string, int> arrayValue = new Dictionary<string, int>();
            arrayValue.Add("array", 1);
            Dictionary<string, int>[] array = [arrayValue];

            Dictionary<string, int> listValue = new Dictionary<string, int>();
            listValue.Add("list", 1);
            List<Dictionary<string, int>> list =
                new List<Dictionary<string, int>>();
            list.Add(listValue);

            Dictionary<string, int> optionValue = new Dictionary<string, int>();
            optionValue.Add("option", 1);
            Option<Dictionary<string, int>> option =
                Option<Dictionary<string, int>>.Some(optionValue);

            Dictionary<string, int> resultValue = new Dictionary<string, int>();
            resultValue.Add("result", 1);
            Result<Dictionary<string, int>, string> result =
                Result<Dictionary<string, int>, string>.Ok(resultValue);

            Dictionary<string, Dictionary<string, int>> outer =
                new Dictionary<string, Dictionary<string, int>>();
            Dictionary<string, int> nested = new Dictionary<string, int>();
            nested.Add("nested", 1);
            outer.Add("nested", nested);

            int ignored = BurnTemporaryMemory();
            int total = holder.Values.Length + array[0].Length
                + list.Get(0).Length + outer.Length;
            switch (option)
            {
                case Some(value): total = total + value.Length;
                case None: return -1;
            }
            switch (result)
            {
                case Ok(value): total = total + value.Length;
                case Error(error): return -2;
            }
            switch (outer.TryGet("nested"))
            {
                case Some(value): total = total + value.Length;
                case None: return -3;
            }
            return total + 35;
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(42)));
}

#[test]
fn dictionary_operations_copy_all_supported_reference_and_enum_value_shapes() {
    let source = r#"
        using aster.core;
        using aster.collections;

        public enum State { Ready, Done }
        public interface IValue { int Get(); }
        public class Value : IValue
        {
            private int value;
            public Value(int value) { this.value = value; }
            public int Get() { return value; }
        }

        public int Main()
        {
            Dictionary<string, State> states = new Dictionary<string, State>();
            states.Add("state", State.Ready);

            Dictionary<string, IValue> objects = new Dictionary<string, IValue>();
            objects.Add("object", new Value(5));

            Dictionary<string, int[]> arrays = new Dictionary<string, int[]>();
            arrays.Add("array", [1, 2, 3]);

            List<int> list = new List<int>();
            list.Add(4);
            Dictionary<string, List<int>> lists = new Dictionary<string, List<int>>();
            lists.Add("list", list);

            Dictionary<string, int> inner = new Dictionary<string, int>();
            Dictionary<string, Dictionary<string, int>> nested =
                new Dictionary<string, Dictionary<string, int>>();
            nested.Add("inner", inner);

            Dictionary<string, Option<int>> options =
                new Dictionary<string, Option<int>>();
            options.Add("option", Option<int>.Some(6));

            Dictionary<string, Result<int, string>> results =
                new Dictionary<string, Result<int, string>>();
            results.Add("result", Result<int, string>.Ok(7));

            int total = 0;
            switch (states.TryGet("state"))
            {
                case Some(state):
                    switch (state) { case Ready: total = total + 1; case Done: }
                case None: return -1;
            }
            switch (objects.TryGet("object"))
            {
                case Some(value): total = total + value.Get();
                case None: return -2;
            }
            switch (arrays.TryGet("array"))
            {
                case Some(value): total = total + value.Length;
                case None: return -3;
            }
            switch (lists.TryGet("list"))
            {
                case Some(value): total = total + value.Length;
                case None: return -4;
            }
            switch (nested.TryGet("inner"))
            {
                case Some(value): total = total + value.Length;
                case None: return -5;
            }
            DictionaryEntry<string, Dictionary<string, int>>[] nestedEntries =
                nested.Entries();
            total = total + nestedEntries.Length;
            switch (options.TryGet("option"))
            {
                case Some(value):
                    switch (value)
                    {
                        case Some(number): total = total + number;
                        case None: return -6;
                    }
                case None: return -7;
            }
            switch (results.TryGet("result"))
            {
                case Some(value):
                    switch (value)
                    {
                        case Ok(number): total = total + number;
                        case Error(error): return -8;
                    }
                case None: return -9;
            }
            return total + 18;
        }
    "#;
    assert_eq!(run_project(source), Ok(ExecutionValue::Int(42)));
}

#[test]
#[allow(clippy::too_many_lines)]
fn malformed_dictionary_mir_is_rejected_before_jit() {
    let source = r#"
        using aster.core;
        using aster.collections;
        public class Ordinary { public int Length; public Ordinary() { Length = 0; } }
        public int Main()
        {
            Dictionary<string, int> values = new Dictionary<string, int>();
            Dictionary<int, int> other = new Dictionary<int, int>();
            int[] array = [1];
            List<int> list = new List<int>();
            Ordinary ordinary = new Ordinary();
            int dictionaryLength = values.Length;
            values.Add("a", 1);
            values.Set("a", 2);
            bool contains = values.ContainsKey("a");
            Option<int> found = values.TryGet("a");
            bool removed = values.Remove("a");
            DictionaryEntry<string, int>[] entries = values.Entries();
            return contains && removed ? entries.Length : 0;
        }
    "#;
    let module = compile_project_mir(source).expect("valid Dictionary MIR");
    let reject = |module: &mir::Module| {
        execute(module, "Main")
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default()
    };
    let local = |module: &mir::Module, name: &str| {
        module
            .functions
            .iter()
            .find(|function| function.name == "Main")
            .and_then(|function| function.locals.iter().find(|local| local.name == name))
            .map_or_else(
                || panic!("local `{name}`"),
                |local| (local.id, local.type_.clone()),
            )
    };

    let mut wrong_key = module.clone();
    let instruction = wrong_key
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::DictionaryAdd { .. }))
        .expect("DictionaryAdd");
    let mir::Instruction::DictionaryAdd { key, .. } = instruction else {
        unreachable!()
    };
    key.type_ = mir::Type::Int;
    assert!(!reject(&wrong_key).is_empty());

    let mut wrong_value = module.clone();
    let instruction = wrong_value
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::DictionarySet { .. }))
        .expect("DictionarySet");
    let mir::Instruction::DictionarySet { value, .. } = instruction else {
        unreachable!()
    };
    value.type_ = mir::Type::String;
    assert!(!reject(&wrong_value).is_empty());

    let mut fake_option = module.clone();
    let instruction = fake_option
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::DictionaryTryGet { .. }))
        .expect("DictionaryTryGet");
    let mir::Instruction::DictionaryTryGet { option_layout, .. } = instruction else {
        unreachable!()
    };
    option_layout.some_case = mir::SymbolId(u32::MAX);
    assert!(!reject(&fake_option).is_empty());

    let mut fake_entry = module.clone();
    let instruction = fake_entry
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::DictionaryEntries { .. }))
        .expect("DictionaryEntries");
    let mir::Instruction::DictionaryEntries { entry_layout, .. } = instruction else {
        unreachable!()
    };
    entry_layout.key_field = mir::SymbolId(u32::MAX);
    assert!(!reject(&fake_entry).is_empty());

    let mut missing_destination = module.clone();
    let instruction = missing_destination
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches!(instruction, mir::Instruction::DictionaryContainsKey { .. }))
        .expect("DictionaryContainsKey");
    let mir::Instruction::DictionaryContainsKey { destination, .. } = instruction else {
        unreachable!()
    };
    *destination = mir::Place::Local(mir::LocalId(u32::MAX));
    assert!(!reject(&missing_destination).is_empty());

    for key_type in [mir::Type::Int, mir::Type::Float, mir::Type::Unknown] {
        let mut adulterated = module.clone();
        let instruction = adulterated
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::AllocateDictionary {
                        key_type: mir::Type::String,
                        ..
                    }
                )
            })
            .expect("string-key AllocateDictionary");
        let mir::Instruction::AllocateDictionary {
            key_type: actual, ..
        } = instruction
        else {
            unreachable!()
        };
        *actual = key_type;
        assert!(!reject(&adulterated).is_empty());
    }

    let mut wrong_allocate_value = module.clone();
    let instruction = wrong_allocate_value
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::AllocateDictionary {
                    key_type: mir::Type::String,
                    ..
                }
            )
        })
        .expect("string-key AllocateDictionary");
    let mir::Instruction::AllocateDictionary { value_type, .. } = instruction else {
        unreachable!()
    };
    *value_type = mir::Type::String;
    assert!(!reject(&wrong_allocate_value).is_empty());

    for destination in [
        mir::Place::Local(mir::LocalId(u32::MAX)),
        mir::Place::Local(local(&module, "array").0),
    ] {
        let mut adulterated = module.clone();
        let instruction = adulterated
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::AllocateDictionary {
                        key_type: mir::Type::String,
                        ..
                    }
                )
            })
            .expect("string-key AllocateDictionary");
        let mir::Instruction::AllocateDictionary {
            destination: actual,
            ..
        } = instruction
        else {
            unreachable!()
        };
        *actual = destination;
        assert!(!reject(&adulterated).is_empty());
    }

    let length_case = |mut adulterated: mir::Module,
                       replacement: Option<(mir::LocalId, mir::Type)>,
                       destination: Option<mir::Place>| {
        let instruction = adulterated
            .functions
            .iter_mut()
            .flat_map(|function| &mut function.blocks)
            .flat_map(|block| &mut block.instructions)
            .find(|instruction| {
                matches!(
                    instruction,
                    mir::Instruction::Assign {
                        value: mir::Rvalue {
                            kind: mir::RvalueKind::DictionaryLength(_),
                            ..
                        },
                        ..
                    }
                )
            })
            .expect("DictionaryLength assignment");
        let mir::Instruction::Assign { target, value } = instruction else {
            unreachable!()
        };
        let mir::RvalueKind::DictionaryLength(receiver) = &mut value.kind else {
            unreachable!()
        };
        if let Some((id, type_)) = replacement {
            receiver.kind = mir::OperandKind::Copy(mir::Place::Local(id));
            receiver.type_ = type_;
        }
        if let Some(destination) = destination {
            *target = destination;
        }
        adulterated
    };

    for name in ["array", "list", "ordinary"] {
        let replacement = local(&module, name);
        assert!(
            !reject(&length_case(module.clone(), Some(replacement), None)).is_empty(),
            "{name}"
        );
    }
    assert!(
        !reject(&length_case(
            module.clone(),
            Some((
                mir::LocalId(u32::MAX),
                mir::Type::Dictionary(Box::new(mir::Type::String), Box::new(mir::Type::Int)),
            )),
            None,
        ))
        .is_empty()
    );
    assert!(
        !reject(&length_case(
            module.clone(),
            None,
            Some(mir::Place::Local(mir::LocalId(u32::MAX))),
        ))
        .is_empty()
    );
    assert!(
        !reject(&length_case(
            module.clone(),
            None,
            Some(mir::Place::Local(local(&module, "ordinary").0)),
        ))
        .is_empty()
    );

    let mut declared_type_divergence = module;
    let instruction = declared_type_divergence
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::Assign {
                    value: mir::Rvalue {
                        kind: mir::RvalueKind::DictionaryLength(_),
                        ..
                    },
                    ..
                }
            )
        })
        .expect("DictionaryLength assignment");
    let mir::Instruction::Assign { value, .. } = instruction else {
        unreachable!()
    };
    let mir::RvalueKind::DictionaryLength(receiver) = &mut value.kind else {
        unreachable!()
    };
    receiver.type_ = mir::Type::Dictionary(Box::new(mir::Type::Int), Box::new(mir::Type::Int));
    assert!(!reject(&declared_type_divergence).is_empty());
}

#[test]
fn dictionary_cannot_cross_workers_but_local_worker_use_is_allowed() {
    for source in [
        r#"using aster.core;
            public Dictionary<string, int> Work() { return new Dictionary<string, int>(); }
            public int Main() { Task<Dictionary<string, int>> task = Task.Run(Work); return 0; }"#,
        r#"using aster.core;
            public Dictionary<string, int> Helper() { return new Dictionary<string, int>(); }
            public Dictionary<string, int> Work() { return Helper(); }
            public int Main() { Task<Dictionary<string, int>> task = Task.Run(Work); return 0; }"#,
        r#"using aster.core;
            public Dictionary<string, int> Work() {
                Dictionary<string, int> first = new Dictionary<string, int>();
                Dictionary<string, int> alias = first;
                return alias;
            }
            public int Main() { Task<Dictionary<string, int>> task = Task.Run(Work); return 0; }"#,
        r#"using aster.core;
            public List<Dictionary<string, int>> Work() { return new List<Dictionary<string, int>>(); }
            public int Main() { Task<List<Dictionary<string, int>>> task = Task.Run(Work); return 0; }"#,
        r#"using aster.core;
            public Dictionary<string, List<int>> Work() { return new Dictionary<string, List<int>>(); }
            public int Main() { Task<Dictionary<string, List<int>>> task = Task.Run(Work); return 0; }"#,
        r#"public void Body(Dictionary<string, int> value) {}
            public int Main() {
                Dictionary<string, int>[] values = new Dictionary<string, int>[0];
                Parallel.ForEach(values, Body);
                return 0;
            }"#,
        r#"public int AddValue(int accumulator, Dictionary<string, int> value) {
                return accumulator + value.Length;
            }
            public int AddPartial(int left, int right) { return left + right; }
            public int Main() {
                Dictionary<string, int>[] values = new Dictionary<string, int>[1];
                return Parallel.Reduce(values, 0, AddValue, AddPartial);
            }"#,
        r#"using aster.core;
            public int Compute() { return 1; }
            public async Task<int> Work() {
                Dictionary<string, int> captured = new Dictionary<string, int>();
                int value = await Task.Run(Compute);
                return captured.Length + value;
            }
            public int Main() { return 0; }"#,
    ] {
        let errors = project_errors(source);
        assert!(
            errors.contains("worker") || errors.contains("transfer") || errors.contains("scalar"),
            "{errors}"
        );
    }

    let local = r#"
        public void Body(int index)
        {
            Dictionary<string, int> dictionary = new Dictionary<string, int>();
            int length = dictionary.Length;
        }
        public int Main()
        {
            Parallel.For(0, 4, Body);
            return 42;
        }
    "#;
    assert_eq!(run(local), Ok(ExecutionValue::Int(42)));

    let task_local = r#"
        using aster.core;
        public int Work()
        {
            Dictionary<string, int> local = new Dictionary<string, int>();
            local.Add("answer", 42);
            return local.ContainsKey("answer") ? local.Length + 41 : 0;
        }
        public int Main()
        {
            Task<int> task = Task.Run(Work);
            return task.Wait();
        }
    "#;
    assert_eq!(run_project(task_local), Ok(ExecutionValue::Int(42)));
}
