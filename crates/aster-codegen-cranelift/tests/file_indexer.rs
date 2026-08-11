//! M5: in-memory tests for the file-indexer example program.
//!
//! The permanent example at `examples/file-indexer/` is compiled from disk and
//! executed against a `MemoryFileSystemBackend` fixture. The free function
//! `file_indexer::app::Main()` in `app/main.aster` is the internal test entry;
//! the CLI uses
//! `Program.Main()` via `Aster.toml`.
//!
//! Fixture (5 direct files, 1 skipped subdirectory):
//!
//! | file                  | content     | scalars | lines | words |
//! |-----------------------|-------------|---------|-------|-------|
//! | input/a.txt           | "hello world"  | 11   | 1     | 2     |
//! | input/b.txt           | "Hello world"  | 11   | 1     | 2     |
//! | input/empty.txt       | ""          | 0       | 0     | 0     |
//! | input/invalid.bin     | 0xFF 0xFE   | skipped | —     | —     |
//! | input/unicode.txt     | "αβγ"       | 3       | 1     | 0     |
//! | input/nested/ignored  | "ignored"   | not listed        |
//!
//! Totals: scalars=25, lines=3, words=4, `unique_words`=3
//! `wordCounts` insertion order: hello=1, world=2, Hello=1

use std::path::Path;

use aster_codegen_cranelift::{ExecutionValue, execute_with_filesystem};
use aster_compiler::{compile_project, mir};
use aster_runtime::{FileSystemBackend, MemoryFileSystemBackend};

fn example_main() -> &'static Path {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/file-indexer/app/main.aster"
    ))
}

fn compile_example() -> mir::Module {
    compile_project(example_main())
        .expect("file-indexer example must compile without errors")
        .compilation
        .mir
}

fn run_fs(
    module: &mir::Module,
    backend: impl FileSystemBackend + 'static,
) -> Result<ExecutionValue, String> {
    execute_with_filesystem(module, "file_indexer::app::Main", Box::new(backend))
        .map_err(|error| error.to_string())
}

fn canonical_backend() -> MemoryFileSystemBackend {
    MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "hello world")
        .with_file("input/b.txt", "Hello world")
        .with_file("input/empty.txt", "")
        .with_file("input/invalid.bin", [0xFF_u8, 0xFE])
        .with_file("input/unicode.txt", "\u{03B1}\u{03B2}\u{03B3}") // αβγ
        .with_directory("input/nested")
        .with_file("input/nested/ignored.txt", "ignored")
        .with_directory("output")
}

// --- Main integration path -------------------------------------------------

#[test]
fn m5_indexer_returns_total_scalars_with_in_memory_backend() {
    let module = compile_example();
    let backend = canonical_backend();
    // Total scalars: a.txt(11) + b.txt(11) + empty.txt(0) + unicode.txt(3) = 25
    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(25)));
}

#[test]
fn m5_indexer_writes_deterministic_report_file() {
    let module = compile_example();
    let backend = canonical_backend();
    let inspection = backend.clone();

    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(25)));

    let report_bytes = inspection
        .read("output/report.txt")
        .expect("report.txt must be written");
    let report = std::str::from_utf8(&report_bytes).expect("report must be valid UTF-8");

    assert!(report.starts_with("ASTER FILE INDEX\n"), "report header");
    assert!(report.contains("files.total=5\n"), "total file count");
    assert!(report.contains("files.readable=4\n"), "readable count");
    assert!(report.contains("files.invalid_utf8=1\n"), "invalid count");
    assert!(report.contains("files.read_errors=0\n"), "error count");
    assert!(report.contains("scalars.total=25\n"), "total scalars");
    assert!(report.contains("lines.total=3\n"), "total lines");
    assert!(report.contains("words.total=4\n"), "total word occurrences");
    assert!(report.contains("words.unique=3\n"), "unique word count");
    assert!(report.contains("\n[files]\n"), "files section header");
    assert!(report.contains("\n[words]\n"), "words section header");
    assert!(
        report.contains("\n[characters]\n"),
        "characters section header"
    );
}

#[test]
fn m5_indexer_report_has_correct_file_entries() {
    let module = compile_example();
    let backend = canonical_backend();
    let inspection = backend.clone();

    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(25)));

    let bytes = inspection.read("output/report.txt").expect("report.txt");
    let report = std::str::from_utf8(&bytes).expect("UTF-8 report");

    assert!(
        report.contains("|readable|scalars=11|lines=1|words=2\n"),
        "a.txt or b.txt entry"
    );
    assert!(
        report.contains("|readable|scalars=0|lines=0|words=0\n"),
        "empty.txt entry"
    );
    assert!(
        report.contains("|invalid_utf8|scalars=0|lines=0|words=0\n"),
        "invalid.bin entry"
    );
    assert!(
        report.contains("|readable|scalars=3|lines=1|words=0\n"),
        "unicode.txt entry"
    );
    assert!(
        !report.contains("ignored"),
        "nested ignored.txt must not appear"
    );
}

#[test]
fn m5_indexer_word_counts_are_correct_and_ordered_by_insertion() {
    let module = compile_example();
    let backend = canonical_backend();
    let inspection = backend.clone();

    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(25)));

    let bytes = inspection.read("output/report.txt").expect("report.txt");
    let report = std::str::from_utf8(&bytes).expect("UTF-8 report");

    // Words section: insertion order is hello(from a.txt), world(from a.txt), Hello(from b.txt)
    assert!(report.contains("hello=1\n"), "hello count");
    assert!(
        report.contains("world=2\n"),
        "world count (appears in both files)"
    );
    assert!(report.contains("Hello=1\n"), "Hello count (case-sensitive)");

    // Verify ordering: hello appears before world which appears before Hello
    let hello_pos = report.find("hello=1\n").unwrap();
    let world_pos = report.find("world=2\n").unwrap();
    let big_hello_pos = report.find("Hello=1\n").unwrap();
    assert!(
        hello_pos < world_pos,
        "hello before world in insertion order"
    );
    assert!(
        world_pos < big_hello_pos,
        "world before Hello in insertion order"
    );
}

#[test]
fn m5_indexer_is_deterministic_across_repeated_executions() {
    let module = compile_example();

    let first_backend = canonical_backend();
    let first_inspect = first_backend.clone();
    assert_eq!(run_fs(&module, first_backend), Ok(ExecutionValue::Int(25)));
    let first_report = first_inspect
        .read("output/report.txt")
        .expect("first report");

    let second_backend = canonical_backend();
    let second_inspect = second_backend.clone();
    assert_eq!(run_fs(&module, second_backend), Ok(ExecutionValue::Int(25)));
    let second_report = second_inspect
        .read("output/report.txt")
        .expect("second report");

    assert_eq!(
        first_report, second_report,
        "report must be byte-for-byte identical across runs"
    );
}

#[test]
fn m5_indexer_handles_empty_input_directory() {
    let module = compile_example();
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(0)));

    let bytes = inspection.read("output/report.txt").expect("report.txt");
    let report = std::str::from_utf8(&bytes).expect("UTF-8 report");
    assert!(report.contains("files.total=0\n"));
    assert!(report.contains("files.readable=0\n"));
    assert!(report.contains("scalars.total=0\n"));
    assert!(report.contains("words.total=0\n"));
    assert!(report.contains("words.unique=0\n"));
}

#[test]
fn m5_indexer_all_invalid_files_produces_zero_scalars() {
    let module = compile_example();
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.bin", [0xFF_u8, 0xFE])
        .with_file("input/b.bin", [0x80_u8])
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(0)));

    let bytes = inspection.read("output/report.txt").expect("report.txt");
    let report = std::str::from_utf8(&bytes).expect("UTF-8 report");
    assert!(report.contains("files.total=2\n"));
    assert!(report.contains("files.readable=0\n"));
    assert!(report.contains("files.invalid_utf8=2\n"));
    assert!(report.contains("scalars.total=0\n"));
}

#[test]
fn m5_indexer_word_boundary_detection_is_correct() {
    let module = compile_example();
    // "abc_123" → one word; "  " → zero words; "a b" → two words
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "abc_123")
        .with_file("input/b.txt", "  ")
        .with_file("input/c.txt", "a b")
        .with_directory("output");
    let inspection = backend.clone();

    // scalars: 7 + 2 + 3 = 12
    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(12)));

    let bytes = inspection.read("output/report.txt").expect("report.txt");
    let report = std::str::from_utf8(&bytes).expect("UTF-8 report");
    // a.txt: abc_123 = 1 word; b.txt: "  " = 0 words; c.txt: "a b" = 2 words → total=3
    assert!(
        report.contains("words.total=3\n"),
        "abc_123(1) + spaces(0) + a(1)+b(1)=3"
    );
    assert!(report.contains("words.unique=3\n"), "abc_123, a, b");
    assert!(report.contains("abc_123=1\n"));
    assert!(report.contains("a=1\n"));
    assert!(report.contains("b=1\n"));
}

#[test]
fn m5_indexer_unicode_scalars_act_as_word_separators() {
    let module = compile_example();
    // "caf\u{00e9}" → "caf" is a word, 'é' (U+00E9) is a separator
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "caf\u{00E9}")
        .with_directory("output");
    let inspection = backend.clone();

    // 4 scalars (c, a, f, é), 1 word (caf), 1 line
    assert_eq!(run_fs(&module, backend), Ok(ExecutionValue::Int(4)));

    let bytes = inspection.read("output/report.txt").expect("report.txt");
    let report = std::str::from_utf8(&bytes).expect("UTF-8 report");
    assert!(report.contains("scalars.total=4\n"));
    assert!(report.contains("words.total=1\n"));
    assert!(report.contains("words.unique=1\n"));
    assert!(report.contains("caf=1\n"));
}
