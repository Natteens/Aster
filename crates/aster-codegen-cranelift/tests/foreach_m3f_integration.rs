//! End-to-end integration coverage for all compiler-known `foreach` paths.
//!
//! The program:
//! 1. `ListFiles` → `foreach` over `string[]`
//! 2. `ReadAllText` per file, `foreach (char ...)` counts scalars
//! 3. Stores counts in `List<int>`, `foreach` to sum
//! 4. `CombinePath` + `WriteAllText` to persist the summary
//!
//! `invalid.bin` contains invalid UTF-8 and is handled as a recoverable error
//! (skipped); `nested/` is a sub-directory and is never listed by the
//! non-recursive `ListFiles`. The expected inputs (sorted by `ListFiles`) are:
//!
//! | file           | content  | scalars |
//! |----------------|----------|---------|
//! | input/a.txt    | "alpha"  | 5       |
//! | input/b.txt    | "beta"   | 4       |
//! | input/empty.txt| ""       | 0       |
//! | input/invalid.bin | 0xFF 0xFE | skipped |
//! | input/unicode.txt | "αβγ"  | 3       |
//!
//! counts List = [5, 4, 0, 3]; total = 12; summary = "files=4;total=12"

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute_with_filesystem};
use aster_compiler::{compile_project, mir};
use aster_runtime::{FileSystemBackend, MemoryFileSystemBackend};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn compile(source: &str) -> mir::Module {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aster-m3f-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).expect("source should compile");
    std::fs::remove_file(&path).ok();
    compilation.compilation.mir
}

fn run_fs(
    source: &str,
    backend: impl FileSystemBackend + 'static,
) -> Result<ExecutionValue, String> {
    execute_with_filesystem(&compile(source), "Main", Box::new(backend))
        .map_err(|error| error.to_string())
}

/// The M3F mandatory integrated program using all three `foreach` paths.
const PROGRAM: &str = "using aster.core;\nusing aster.io;\n\
    public Result<int, IOError> Run(string inputDir, string outputDir) {\n\
        string[] files = ListFiles(inputDir)?;\n\
        List<int> counts = new List<int>();\n\
        foreach (string file in files) {\n\
            switch (ReadAllText(file)) {\n\
                case Ok(text):\n\
                    int count = 0;\n\
                    foreach (char c in text) { count = count + 1; }\n\
                    counts.Add(count);\n\
                case Error(error):\n\
                    bool skip = error.Kind == IOErrorKind.InvalidUtf8;\n\
                    if (!skip) { return Result<int, IOError>.Error(error); }\n\
            }\n\
        }\n\
        int total = 0;\n\
        foreach (int count in counts) { total = total + count; }\n\
        string resultPath = CombinePath(outputDir, \"result.txt\")?;\n\
        string summary = \"files=\" + counts.Length.ToString() + \";total=\" + total.ToString();\n\
        int written = WriteAllText(resultPath, summary)?;\n\
        return Result<int, IOError>.Ok(total);\n\
    }\n\
    public int Main() {\n\
        switch (Run(\"input\", \"output\")) {\n\
            case Ok(value): return value;\n\
            case Error(error): return -1;\n\
        }\n\
    }";

/// Canonical backend shared by multiple test cases.
fn canonical_backend() -> MemoryFileSystemBackend {
    MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "alpha")
        .with_file("input/b.txt", "beta")
        .with_file("input/empty.txt", "")
        .with_file("input/invalid.bin", [0xFF_u8, 0xFE])
        .with_file("input/unicode.txt", "\u{03B1}\u{03B2}\u{03B3}") // αβγ
        .with_directory("input/nested")
        .with_file("input/nested/ignored.txt", "ignored")
        .with_directory("output")
}

// --- Main integration path -------------------------------------------------

#[test]
fn m3f_integrated_program_uses_all_three_foreach_paths_with_in_memory_filesystem() {
    // The first and only full end-to-end exercise of all three foreach paths
    // (array, string, List) together with the M2 file I/O APIs in one program.
    let backend = canonical_backend();
    let inspection = backend.clone();

    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(12)));

    // Verify the file written by WriteAllText through the shared backing map.
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=4;total=12".to_vec()),
        "WriteAllText output must match the computed summary"
    );
}

#[test]
fn m3f_program_file_ordering_is_deterministic_and_correct() {
    // Confirm the specific counts by position, not just the total: if the
    // order or skip logic were wrong, individual count offsets would differ.
    // We run a variant that produces per-file counts rather than a summary.
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ListFiles(\"input\")) {\n\
                case Ok(files):\n\
                    List<int> counts = new List<int>();\n\
                    foreach (string file in files) {\n\
                        switch (ReadAllText(file)) {\n\
                            case Ok(text):\n\
                                int count = 0;\n\
                                foreach (char c in text) { count = count + 1; }\n\
                                counts.Add(count);\n\
                            case Error(error):\n\
                                bool skip = error.Kind == IOErrorKind.InvalidUtf8;\n\
                                if (!skip) { return -1; }\n\
                        }\n\
                    }\n\
                    // counts = [5, 4, 0, 3]; encode as 5000 + 400 + 0 + 3\n\
                    int result = 0;\n\
                    int pos = 0;\n\
                    foreach (int count in counts) {\n\
                        if (pos == 0) { result = result + count * 1000; }\n\
                        if (pos == 1) { result = result + count * 100; }\n\
                        if (pos == 2) { result = result + count * 10; }\n\
                        if (pos == 3) { result = result + count; }\n\
                        pos = pos + 1;\n\
                    }\n\
                    return result;\n\
                case Error(error): return -2;\n\
            }\n\
        }";
    // a.txt=5,b.txt=4,empty.txt=0,unicode.txt=3 → 5*1000 + 4*100 + 0*10 + 3 = 5403
    assert_eq!(
        run_fs(source, canonical_backend()),
        Ok(ExecutionValue::Int(5403))
    );
}

#[test]
fn m3f_program_unicode_scalar_count_is_precise() {
    // "αβγ" is 3 Unicode scalar values (each encoded as 2 UTF-8 bytes).
    // The foreach-over-string path must decode scalars, not bytes.
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "\u{03B1}\u{03B2}\u{03B3}") // αβγ: 3 scalars, 6 bytes
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(3)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=1;total=3".to_vec())
    );
}

#[test]
fn m3f_program_treats_invalid_utf8_as_recoverable_and_continues() {
    // The program must skip `invalid.bin` and continue, not abort.
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "ok")
        .with_file("input/bad.bin", [0xFF_u8, 0xFE])
        .with_file("input/z.txt", "z")
        .with_directory("output");
    let inspection = backend.clone();

    // sorted: a.txt (2), bad.bin (skip), z.txt (1) → total = 3
    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(3)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=2;total=3".to_vec())
    );
}

#[test]
fn m3f_program_handles_an_empty_directory_gracefully() {
    // No files to iterate → counts empty → total 0 → WriteAllText with "files=0;total=0".
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(0)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=0;total=0".to_vec())
    );
}

#[test]
fn m3f_program_handles_all_empty_files() {
    // All files readable but all have 0 scalars.
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "")
        .with_file("input/b.txt", "")
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(0)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=2;total=0".to_vec())
    );
}

#[test]
fn m3f_program_handles_all_invalid_files() {
    // Every file has invalid UTF-8 → all skipped → counts empty → total 0.
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.bin", [0xFF_u8, 0xFE])
        .with_file("input/b.bin", [0x80_u8])
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(0)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=0;total=0".to_vec())
    );
}

#[test]
fn m3f_program_nested_subdirectory_is_never_iterated() {
    // ListFiles is non-recursive; nested/deep.txt must not appear.
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/only.txt", "hi")
        .with_directory("input/sub")
        .with_file("input/sub/deep.txt", "deep-content-123456")
        .with_directory("output");
    let inspection = backend.clone();

    // Only "input/only.txt" is listed → 2 scalars → total=2
    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(2)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=1;total=2".to_vec())
    );
}
