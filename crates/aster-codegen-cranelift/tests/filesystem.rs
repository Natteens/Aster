//! M2D: `aster.io.ReadAllText(string) -> Result<string, IOError>` and
//! `aster.io.WriteAllText(string, string) -> Result<int, IOError>`, host-
//! managed synchronous file I/O. No handle is ever exposed to ASTER code:
//! the host opens, reads/writes, and closes the file entirely within one
//! call. Every test here injects `MemoryFileSystemBackend`/
//! `FailingFileSystemBackend`/`PartialWriteFailureFileSystemBackend`
//! (`aster-runtime`'s injectable seam) so none of this suite touches the
//! developer's or CI's real filesystem; `aster-cli`'s own tests cover the
//! real filesystem end-to-end.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{
    ExecutionValue, MemoryStats, execute, execute_with_filesystem,
    execute_with_filesystem_and_stats,
};
use aster_compiler::{compile_project, mir};
use aster_runtime::{
    FailingFileSystemBackend, FileSystemBackend, MAX_FILE_BYTES, MemoryFileSystemBackend,
    PartialWriteFailureFileSystemBackend,
};

fn compile(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-filesystem-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    compilation.map(|compilation| compilation.compilation.mir)
}

/// The worker filesystem-I/O rejection lives in `aster-codegen-cranelift`'s
/// `validate_module`, run by `execute()`/`aster check` -- not by
/// `compile_project` alone (which only runs semantic/HIR/MIR lowering).
fn execute_errors(source: &str, function: &str) -> String {
    match execute(&compile_mir(source), function) {
        Ok(_) => String::new(),
        Err(error) => error.to_string(),
    }
}

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile")
}

fn run_fs(
    source: &str,
    function: &str,
    backend: impl FileSystemBackend + 'static,
) -> Result<ExecutionValue, String> {
    execute_with_filesystem(&compile_mir(source), function, Box::new(backend))
        .map_err(|error| error.to_string())
}

fn stats_fs(
    source: &str,
    function: &str,
    backend: impl FileSystemBackend + 'static,
) -> (Result<ExecutionValue, String>, MemoryStats) {
    match execute_with_filesystem_and_stats(&compile_mir(source), function, Box::new(backend)) {
        Ok((value, stats)) => (Ok(value), stats),
        Err(error) => (Err(error.to_string()), MemoryStats::default()),
    }
}

const READ_SWITCH: &str = "using aster.core;\nusing aster.io;\n\
    public int Main() {\n\
        switch (ReadAllText(\"a.txt\")) {\n\
            case Ok(text): return text.Length;\n\
            case Error(e): return -1000 - (e.OsCode);\n\
        }\n\
    }";

fn read_error_kind_source() -> &'static str {
    "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ReadAllText(\"a.txt\")) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) {\n\
                    case NotFound: return 1;\n\
                    case PermissionDenied: return 2;\n\
                    case AlreadyExists: return 3;\n\
                    case InvalidPath: return 4;\n\
                    case InvalidUtf8: return 5;\n\
                    case NotFile: return 6;\n\
                    case NotDirectory: return 7;\n\
                    case LimitExceeded: return 8;\n\
                    case Other: return 9;\n\
                }\n\
            }\n\
        }"
}

// --- ReadAllText ---------------------------------------------------------------

#[test]
fn read_all_text_returns_ascii_content_verbatim() {
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "hello");
    assert_eq!(
        run_fs(READ_SWITCH, "Main", backend),
        Ok(ExecutionValue::Int(5))
    );
}

#[test]
fn read_all_text_returns_unicode_content_verbatim() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (ReadAllText(\"a.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "Olá, ASTER! 🙂");
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::String("Olá, ASTER! 🙂".to_owned()))
    );
}

#[test]
fn read_all_text_returns_empty_content_for_an_empty_file() {
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "");
    assert_eq!(
        run_fs(READ_SWITCH, "Main", backend),
        Ok(ExecutionValue::Int(0))
    );
}

#[test]
fn read_all_text_preserves_lf_exactly() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (ReadAllText(\"a.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "one\ntwo\n");
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::String("one\ntwo\n".to_owned()))
    );
}

#[test]
fn read_all_text_preserves_crlf_exactly_without_normalizing() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Main() {\n\
            switch (ReadAllText(\"a.txt\")) {\n\
                case Ok(text): return text;\n\
                case Error(e): return \"error\";\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "one\r\ntwo\r\n");
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::String("one\r\ntwo\r\n".to_owned()))
    );
}

#[test]
fn read_all_text_preserves_an_embedded_nul_in_the_file_content() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ReadAllText(\"a.txt\")) {\n\
                case Ok(text):\n\
                    if (text.Contains(\"b\")) { return text.Length; }\n\
                    return -1;\n\
                case Error(e): return -2;\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", b"a\0b".to_vec());
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(3)));
}

#[test]
fn read_all_text_reports_not_found_for_a_missing_file() {
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(
        run_fs(read_error_kind_source(), "Main", backend),
        Ok(ExecutionValue::Int(1))
    );
}

#[test]
fn read_all_text_reports_permission_denied_simulated_via_a_controlled_backend() {
    let backend = FailingFileSystemBackend::new(io::ErrorKind::PermissionDenied);
    assert_eq!(
        run_fs(read_error_kind_source(), "Main", backend),
        Ok(ExecutionValue::Int(2))
    );
}

#[test]
fn read_all_text_reports_not_file_for_a_directory() {
    let backend = MemoryFileSystemBackend::new().with_directory("a.txt");
    assert_eq!(
        run_fs(read_error_kind_source(), "Main", backend),
        Ok(ExecutionValue::Int(6))
    );
}

#[test]
fn read_all_text_reports_invalid_utf8_for_invalid_content() {
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", vec![0xFF, 0xFE]);
    assert_eq!(
        run_fs(read_error_kind_source(), "Main", backend),
        Ok(ExecutionValue::Int(5))
    );
}

#[test]
fn read_all_text_accepts_content_exactly_at_the_limit() {
    let content = vec![b'x'; usize::try_from(MAX_FILE_BYTES).unwrap()];
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", content);
    assert_eq!(
        run_fs(READ_SWITCH, "Main", backend),
        Ok(ExecutionValue::Int(i32::try_from(MAX_FILE_BYTES).unwrap()))
    );
}

#[test]
fn read_all_text_rejects_content_one_byte_above_the_limit() {
    let content = vec![b'x'; usize::try_from(MAX_FILE_BYTES).unwrap() + 1];
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", content);
    assert_eq!(
        run_fs(read_error_kind_source(), "Main", backend),
        Ok(ExecutionValue::Int(8))
    );
}

#[test]
fn read_all_text_rejects_a_file_that_grows_past_the_limit_via_a_controlled_backend() {
    // The backend's stored content is far larger than the limit, simulating
    // a file that grows during the read; the bounded-read design (never
    // trusting metadata alone) must still classify this as `LimitExceeded`,
    // not silently accept a truncated read as success.
    let content = vec![b'x'; usize::try_from(MAX_FILE_BYTES).unwrap() * 2];
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", content);
    assert_eq!(
        run_fs(read_error_kind_source(), "Main", backend),
        Ok(ExecutionValue::Int(8))
    );
}

#[test]
fn read_all_text_rejects_an_empty_path() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ReadAllText(\"\")) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

#[test]
fn read_all_text_rejects_a_path_with_an_embedded_nul() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            char nul = (char)0;\n\
            string path = \"a\" + nul.ToString() + \"b\";\n\
            switch (ReadAllText(path)) {\n\
                case Ok(text): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

// --- WriteAllText ----------------------------------------------------------------

#[test]
fn write_all_text_creates_a_new_file_and_reports_bytes_written() {
    let backend = MemoryFileSystemBackend::new();
    let observe = backend.clone();
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"hello\")) {\n\
                case Ok(count): return count;\n\
                case Error(e): return -1;\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(5)));
    assert_eq!(observe.read("a.txt"), Some(b"hello".to_vec()));
}

#[test]
fn write_all_text_truncates_prior_content() {
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "a much longer old content");
    let observe = backend.clone();
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"new\")) {\n\
                case Ok(count): return count;\n\
                case Error(e): return -1;\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(3)));
    assert_eq!(observe.read("a.txt"), Some(b"new".to_vec()));
}

#[test]
fn write_all_text_of_empty_content_reports_zero_bytes_and_empties_the_file() {
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "old");
    let observe = backend.clone();
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"\")) {\n\
                case Ok(count): return count;\n\
                case Error(e): return -1;\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
    assert_eq!(observe.read("a.txt"), Some(Vec::new()));
}

#[test]
fn write_all_text_counts_unicode_content_in_bytes_not_scalars() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            int a = WriteAllText(\"a.txt\", \"é\")?;\n\
            int b = WriteAllText(\"b.txt\", \"🙂\")?;\n\
            return Result<int, IOError>.Ok(a * 100 + b);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::Int(2 * 100 + 4))
    );
}

#[test]
fn write_all_text_preserves_an_embedded_nul_in_content() {
    let backend = MemoryFileSystemBackend::new();
    let observe = backend.clone();
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            char nul = (char)0;\n\
            string content = \"a\" + nul.ToString() + \"b\";\n\
            switch (WriteAllText(\"a.txt\", content)) {\n\
                case Ok(count): return count;\n\
                case Error(e): return -1;\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(3)));
    assert_eq!(observe.read("a.txt"), Some(b"a\0b".to_vec()));
}

#[test]
fn write_all_text_reports_not_file_for_an_existing_directory_destination() {
    let backend = MemoryFileSystemBackend::new().with_directory("a.txt");
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"x\")) {\n\
                case Ok(count): return -1;\n\
                case Error(e): switch (e.Kind) { case NotFile: return 0; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

#[test]
fn write_all_text_reports_permission_denied_simulated_via_a_controlled_backend() {
    let backend = FailingFileSystemBackend::new(io::ErrorKind::PermissionDenied);
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"x\")) {\n\
                case Ok(count): return -1;\n\
                case Error(e): switch (e.Kind) { case PermissionDenied: return 0; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

#[test]
fn write_all_text_accepts_content_exactly_at_the_limit() {
    let content = "x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap());
    let source = format!(
        "using aster.core;\nusing aster.io;\n\
         public Result<int, IOError> Run() {{\n\
             string content = \"{content}\";\n\
             return WriteAllText(\"a.txt\", content);\n\
         }}\n\
         public int Main() {{\n\
             switch (Run()) {{ case Ok(v): return v; case Error(e): return -1; }}\n\
         }}"
    );
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(
        run_fs(&source, "Main", backend),
        Ok(ExecutionValue::Int(i32::try_from(MAX_FILE_BYTES).unwrap()))
    );
}

#[test]
fn write_all_text_rejects_content_above_the_limit_without_touching_the_destination() {
    // Built by doubling a persistent half-limit string in ASTER (avoids an
    // enormous literal in the generated source text) so the content byte
    // length is `MAX_FILE_BYTES + 2`, comfortably over the limit.
    let half = "x".repeat(usize::try_from(MAX_FILE_BYTES / 2).unwrap());
    let source = format!(
        "using aster.core;\nusing aster.io;\n\
         public int Main() {{\n\
             string half = \"{half}\";\n\
             string content = half + half + \"xx\";\n\
             switch (WriteAllText(\"a.txt\", content)) {{\n\
                 case Ok(count): return -1;\n\
                 case Error(e): switch (e.Kind) {{ case LimitExceeded: return 0; default: return -2; }}\n\
             }}\n\
         }}"
    );
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "untouched");
    let observe = backend.clone();
    assert_eq!(run_fs(&source, "Main", backend), Ok(ExecutionValue::Int(0)));
    // The destination must be untouched: the limit check happens before any
    // open/create/truncate/write.
    assert_eq!(observe.read("a.txt"), Some(b"untouched".to_vec()));
}

#[test]
fn write_all_text_partial_failure_may_leave_content_written_but_still_reports_failure() {
    // Documents the "no atomicity" contract: a failure after the file was
    // created/truncated may leave it partially (or fully) written.
    let backend = PartialWriteFailureFileSystemBackend::new();
    let observe = backend.clone();
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"content\")) {\n\
                case Ok(count): return -1;\n\
                case Error(e): switch (e.Kind) { case Other: return 0; default: return -2; }\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
    assert_eq!(observe.read("a.txt"), Some(b"content".to_vec()));
}

#[test]
fn write_all_text_flush_failure_is_reported_as_a_normal_error() {
    let backend = PartialWriteFailureFileSystemBackend::new();
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"x\")) {\n\
                case Ok(count): return -1;\n\
                case Error(e): return 0;\n\
            }\n\
        }";
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

#[test]
fn write_all_text_rejects_an_empty_path() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"\", \"x\")) {\n\
                case Ok(count): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

#[test]
fn write_all_text_rejects_a_path_with_an_embedded_nul() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            char nul = (char)0;\n\
            string path = \"a\" + nul.ToString() + \"b\";\n\
            switch (WriteAllText(path, \"x\")) {\n\
                case Ok(count): return -1;\n\
                case Error(e): switch (e.Kind) { case InvalidPath: return e.OsCode; default: return -2; }\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(0)));
}

// --- Integration -----------------------------------------------------------------

#[test]
fn read_all_text_works_with_switch_directly() {
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "switched");
    assert_eq!(
        run_fs(READ_SWITCH, "Main", backend),
        Ok(ExecutionValue::Int(8))
    );
}

#[test]
fn read_all_text_works_with_postfix_try() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            string text = ReadAllText(\"a.txt\")?;\n\
            return Result<int, IOError>.Ok(text.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "tried");
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(5)));
}

#[test]
fn write_all_text_works_with_postfix_try() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            int count = WriteAllText(\"a.txt\", \"tried\")?;\n\
            return Result<int, IOError>.Ok(count);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(5)));
}

#[test]
fn a_string_read_and_returned_directly_stays_correct() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<string, IOError> Run() { return ReadAllText(\"a.txt\"); }\n\
        public string Main() {\n\
            switch (Run()) { case Ok(text): return text; case Error(e): return \"error\"; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "returned");
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::String("returned".to_owned()))
    );
}

#[test]
fn a_read_result_stored_in_a_class_field_survives_a_round_trip() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Holder { public string stored; public Holder() { stored = \"\"; } }\n\
        public Result<int, IOError> Run() {\n\
            Holder holder = new Holder();\n\
            holder.stored = ReadAllText(\"a.txt\")?;\n\
            return Result<int, IOError>.Ok(holder.stored.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "in-a-field");
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(10)));
}

#[test]
fn read_all_text_used_through_a_helper_function() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> ReadLength(string path) {\n\
            string text = ReadAllText(path)?;\n\
            return Result<int, IOError>.Ok(text.Length);\n\
        }\n\
        public int Main() {\n\
            switch (ReadLength(\"a.txt\")) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "via-helper");
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(10)));
}

#[test]
fn read_all_text_result_flows_through_a_generic_function() {
    let source = "using aster.core;\nusing aster.io;\n\
        public T First<T>(T a, T b) { return a; }\n\
        public Result<int, IOError> Run() {\n\
            string first = ReadAllText(\"a.txt\")?;\n\
            string second = ReadAllText(\"b.txt\")?;\n\
            string chosen = First(first, second);\n\
            return Result<int, IOError>.Ok(chosen.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new()
        .with_file("a.txt", "picked")
        .with_file("b.txt", "other-longer");
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(6)));
}

#[test]
fn read_all_text_and_write_all_text_work_across_two_files_in_a_namespace() {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aster-filesystem-multifile-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root).expect("create project root");
    std::fs::write(
        root.join("Aster.toml"),
        "[package]\nname = \"filesystem_test\"\n",
    )
    .expect("write manifest");
    let app_dir = root.join("app");
    std::fs::create_dir_all(&app_dir).expect("create app dir");
    std::fs::write(
        app_dir.join("main.aster"),
        "namespace app;\n\
         using aster.core;\n\
         using aster.io;\n\
         public Result<int, IOError> Run() {\n\
             string text = Helpers.ReadIt(\"a.txt\")?;\n\
             return Result<int, IOError>.Ok(text.Length);\n\
         }\n\
         public int Main() {\n\
             switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
         }",
    )
    .expect("write main.aster");
    std::fs::write(
        app_dir.join("helpers.aster"),
        "namespace app;\n\
         using aster.core;\n\
         using aster.io;\n\
         public class Helpers {\n\
             public static Result<string, IOError> ReadIt(string path) { return ReadAllText(path); }\n\
         }",
    )
    .expect("write helpers.aster");
    let compilation = compile_project(&app_dir.join("main.aster"))
        .map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_dir_all(&root).ok();
    let module = compilation
        .expect("multifile project using ReadAllText should compile")
        .compilation
        .mir;
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "cross-file-nine");
    assert_eq!(
        execute_with_filesystem(&module, "filesystem_test::app::Main", Box::new(backend)),
        Ok(ExecutionValue::Int(15))
    );
}

#[test]
fn combine_path_and_read_all_text_chain_together() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            string path = CombinePath(\"dir\", \"a.txt\")?;\n\
            string text = ReadAllText(path)?;\n\
            return Result<int, IOError>.Ok(text.Length);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("dir/a.txt", "combined");
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(8)));
}

#[test]
fn read_all_text_argument_is_evaluated_exactly_once() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Counter {\n\
            public int calls;\n\
            public string GetPath() { calls = calls + 1; return \"a.txt\"; }\n\
        }\n\
        public Result<int, IOError> Wrap(Counter counter) {\n\
            string text = ReadAllText(counter.GetPath())?;\n\
            return Result<int, IOError>.Ok(text.Length);\n\
        }\n\
        public int Main() {\n\
            Counter counter = new Counter();\n\
            Wrap(counter);\n\
            return counter.calls;\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "once");
    assert_eq!(run_fs(source, "Main", backend), Ok(ExecutionValue::Int(1)));
}

#[test]
fn write_all_text_evaluates_path_then_content_exactly_once_each_in_order() {
    let source = "using aster.core;\nusing aster.io;\n\
        public class Tracker {\n\
            public string log;\n\
            public Tracker() { log = \"\"; }\n\
            public string GetPath() { log = log + \"P\"; return \"a.txt\"; }\n\
            public string GetContent() { log = log + \"C\"; return \"x\"; }\n\
        }\n\
        public Result<int, IOError> Wrap(Tracker tracker) {\n\
            int count = WriteAllText(tracker.GetPath(), tracker.GetContent())?;\n\
            return Result<int, IOError>.Ok(count);\n\
        }\n\
        public string Main() {\n\
            Tracker tracker = new Tracker();\n\
            Wrap(tracker);\n\
            return tracker.log;\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::String("PC".to_owned()))
    );
}

// --- Isolation ---------------------------------------------------------------------

#[test]
fn independent_contexts_never_share_a_filesystem_backend() {
    let module = compile_mir(READ_SWITCH);
    let first = MemoryFileSystemBackend::new().with_file("a.txt", "first-content");
    let second = MemoryFileSystemBackend::new().with_file("a.txt", "second");
    let first_result =
        execute_with_filesystem(&module, "Main", Box::new(first)).expect("first context runs");
    let second_result =
        execute_with_filesystem(&module, "Main", Box::new(second)).expect("second context runs");
    assert_eq!(first_result, ExecutionValue::Int(13));
    assert_eq!(second_result, ExecutionValue::Int(6));
}

#[test]
fn an_error_in_one_context_does_not_contaminate_a_later_context() {
    // A missing file is a normal `Result::Error`, not an `execute()` failure
    // (see `a_normal_read_failure_is_a_result_error_never_an_execution_context_failure`),
    // so the first context still returns `Ok` with the negative "not found"
    // sentinel `READ_SWITCH`'s `Error` arm produces.
    let module = compile_mir(READ_SWITCH);
    let missing = MemoryFileSystemBackend::new();
    let first = execute_with_filesystem(&module, "Main", Box::new(missing))
        .expect("a Result::Error is a normal, successful execution");
    assert!(matches!(first, ExecutionValue::Int(v) if v < 0));
    let present = MemoryFileSystemBackend::new().with_file("a.txt", "recovered");
    let recovered = execute_with_filesystem(&module, "Main", Box::new(present))
        .expect("a fresh context is unaffected by the previous one's error");
    assert_eq!(recovered, ExecutionValue::Int(9));
}

#[test]
fn a_normal_read_failure_is_a_result_error_never_an_execution_context_failure() {
    // `execute_with_filesystem` returning `Ok` (not `Err`) for a missing file
    // is itself the proof: `ExecutionContext::fail` would have surfaced as a
    // `BackendError` ("Aster runtime error: ..."), not a normal `Ok` result.
    let backend = MemoryFileSystemBackend::new();
    let result = run_fs(read_error_kind_source(), "Main", backend);
    assert_eq!(result, Ok(ExecutionValue::Int(1)));
}

// --- Workers -----------------------------------------------------------------------

#[test]
fn task_run_reaching_read_all_text_directly_is_rejected() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Body() { switch (ReadAllText(\"a.txt\")) { case Ok(t): return 0; case Error(e): return 1; } }\n\
        public int Main() { Task<int> t = Task.Run(Body); return t.Wait(); }";
    let errors = execute_errors(source, "Main");
    assert!(
        errors.contains("ReadAllText"),
        "expected the worker filesystem-I/O rejection, got {errors}"
    );
}

#[test]
fn parallel_for_reaching_write_all_text_directly_is_rejected() {
    let source = "using aster.core;\nusing aster.io;\n\
        public void Body(int i) { WriteAllText(\"a.txt\", \"x\"); }\n\
        public int Main() { Parallel.For(0, 4, Body); return 0; }";
    let errors = execute_errors(source, "Main");
    assert!(
        errors.contains("WriteAllText"),
        "expected the worker filesystem-I/O rejection, got {errors}"
    );
}

#[test]
fn parallel_for_each_reaching_read_all_text_directly_is_rejected() {
    let source = "using aster.core;\nusing aster.io;\n\
        public void Body(int i) { ReadAllText(\"a.txt\"); }\n\
        public int Main() { int[] values = [1, 2, 3]; Parallel.ForEach(values, Body); return 0; }";
    let errors = execute_errors(source, "Main");
    assert!(errors.contains("ReadAllText"), "got {errors}");
}

#[test]
fn parallel_reduce_reaching_write_all_text_directly_is_rejected() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Accumulate(int acc, int value) { WriteAllText(\"a.txt\", \"x\"); return acc + value; }\n\
        public int Combine(int a, int b) { return a + b; }\n\
        public int Main() { int[] values = [1, 2, 3]; return Parallel.Reduce(values, 0, Accumulate, Combine); }";
    let errors = execute_errors(source, "Main");
    assert!(errors.contains("WriteAllText"), "got {errors}");
}

#[test]
fn parallel_for_rejects_a_worker_body_that_transitively_calls_read_all_text() {
    let source = "using aster.core;\nusing aster.io;\n\
        public string Helper() { switch (ReadAllText(\"a.txt\")) { case Ok(t): return t; case Error(e): return \"\"; } }\n\
        public void Body(int i) { Helper(); }\n\
        public int Main() { Parallel.For(0, 4, Body); return 0; }";
    let errors = execute_errors(source, "Main");
    assert!(
        errors.contains("ReadAllText"),
        "transitive worker rejection failed, got {errors}"
    );
}

#[test]
fn a_worker_body_that_never_calls_file_io_still_compiles() {
    let source = "using aster.core;\n\
        public void Body(int i) { int x = i * 2; }\n\
        public int Main() { Parallel.For(0, 4, Body); return 0; }";
    assert!(
        compile(source).is_ok(),
        "an I/O-free worker body must not be rejected"
    );
}

// --- MIR adulteration --------------------------------------------------------------

fn find_first_matching(
    module: &mut mir::Module,
    matches: impl Fn(&mir::Instruction) -> bool,
) -> &mut mir::Instruction {
    module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| matches(instruction))
        .expect("a matching instruction exists")
}

fn find_read_all_text_call(module: &mut mir::Module) -> &mut mir::Instruction {
    find_first_matching(module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::FileReadAllText(_)
                    | mir::Intrinsic::FileReadAllTextTemporary(_),
                ..
            }
        )
    })
}

#[test]
fn adulterated_mir_rejects_a_read_all_text_call_with_a_non_string_path() {
    let mut module = compile_mir(READ_SWITCH);
    let instruction = find_read_all_text_call(&mut module);
    let mir::Instruction::CallIntrinsic { arguments, .. } = instruction else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Int;
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_read_all_text_call_with_wrong_arity() {
    let mut module = compile_mir(READ_SWITCH);
    let instruction = find_read_all_text_call(&mut module);
    let mir::Instruction::CallIntrinsic { arguments, .. } = instruction else {
        unreachable!();
    };
    arguments.clear();
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_read_all_text_call_returning_a_non_enum_type() {
    let mut module = compile_mir(READ_SWITCH);
    let instruction = find_read_all_text_call(&mut module);
    let mir::Instruction::CallIntrinsic { return_type, .. } = instruction else {
        unreachable!();
    };
    *return_type = mir::Type::String;
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_read_all_text_call_retargeted_to_an_unknown_enum_symbol() {
    let mut module = compile_mir(READ_SWITCH);
    let instruction = find_read_all_text_call(&mut module);
    let mir::Instruction::CallIntrinsic { return_type, .. } = instruction else {
        unreachable!();
    };
    *return_type = mir::Type::Enum(mir::SymbolId(u32::MAX));
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_write_all_text_call_with_a_non_string_content_argument() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (WriteAllText(\"a.txt\", \"x\")) {\n\
                case Ok(count): return count;\n\
                case Error(e): return -1;\n\
            }\n\
        }";
    let mut module = compile_mir(source);
    let instruction = find_first_matching(&mut module, |instruction| {
        matches!(
            instruction,
            mir::Instruction::CallIntrinsic {
                intrinsic: mir::Intrinsic::FileWriteAllText(_),
                ..
            }
        )
    });
    let mir::Instruction::CallIntrinsic { arguments, .. } = instruction else {
        unreachable!();
    };
    arguments[1].type_ = mir::Type::Bool;
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

// --- Nominal identity: symbols only, never a name comparison at codegen ------

/// Mutates the concrete `FileIoResultLayout` payload HIR lowering resolved
/// for `ReadAllText`'s call in `READ_SWITCH`, proving the backend rejects a
/// mismatched *symbol* exactly as it would a mismatched name -- the payload
/// carries `SymbolId`s, never "Ok"/"Error"/"Kind"/"OsCode" text, so this is
/// the only way left to adulterate "which case/field" post-HIR-lowering.
fn mutate_read_all_text_layout(
    module: &mut mir::Module,
    mutate: impl FnOnce(&mut mir::FileIoResultLayout),
) {
    let instruction = find_read_all_text_call(module);
    let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
        unreachable!();
    };
    let (mir::Intrinsic::FileReadAllText(layout)
    | mir::Intrinsic::FileReadAllTextTemporary(layout)) = intrinsic
    else {
        unreachable!();
    };
    mutate(layout);
}

#[test]
fn adulterated_mir_rejects_an_ok_case_symbol_that_does_not_match_the_resolved_result() {
    let mut module = compile_mir(READ_SWITCH);
    mutate_read_all_text_layout(&mut module, |layout| {
        layout.ok_case = mir::SymbolId(u32::MAX);
    });
    // `validate_file_io_result_shapes` (part of `validate_module`, so it
    // runs before codegen ever sees this MIR) rejects the mismatch first,
    // with its own generic "not shaped like Result<T, IOError>" message;
    // `Codegen::result_io_error_layout`'s more specific "Ok case symbol does
    // not match..." message only fires if that earlier check is somehow
    // bypassed. Either layer rejecting is the requirement here.
    let error = compile_mir_execute_error(&module);
    assert!(
        error.contains("not `Result<string, IOError>`"),
        "expected a symbol-mismatch diagnostic, got {error}"
    );
}

#[test]
fn adulterated_mir_rejects_an_error_field_symbol_that_does_not_match_the_resolved_error_case() {
    let mut module = compile_mir(READ_SWITCH);
    mutate_read_all_text_layout(&mut module, |layout| {
        layout.error_field = mir::SymbolId(u32::MAX);
    });
    let error = compile_mir_execute_error(&module);
    assert!(
        error.contains("not `Result<string, IOError>`"),
        "expected a symbol-mismatch diagnostic, got {error}"
    );
}

#[test]
fn adulterated_mir_rejects_an_io_error_kind_field_symbol_that_does_not_match_the_resolved_struct() {
    let mut module = compile_mir(READ_SWITCH);
    mutate_read_all_text_layout(&mut module, |layout| {
        layout.io_error_kind_field = mir::SymbolId(u32::MAX);
    });
    let error = compile_mir_execute_error(&module);
    assert!(
        error.contains("not `Result<string, IOError>`"),
        "expected a symbol-mismatch diagnostic, got {error}"
    );
}

#[test]
fn adulterated_mir_rejects_an_io_error_os_code_field_symbol_that_does_not_match_the_resolved_struct()
 {
    let mut module = compile_mir(READ_SWITCH);
    mutate_read_all_text_layout(&mut module, |layout| {
        layout.io_error_os_code_field = mir::SymbolId(u32::MAX);
    });
    let error = compile_mir_execute_error(&module);
    assert!(
        error.contains("not `Result<string, IOError>`"),
        "expected a symbol-mismatch diagnostic, got {error}"
    );
}

#[test]
fn adulterated_mir_rejects_a_portable_kind_case_symbol_that_does_not_match_the_resolved_enum() {
    let mut module = compile_mir(READ_SWITCH);
    mutate_read_all_text_layout(&mut module, |layout| {
        layout.portable_kind_cases[0] = mir::SymbolId(u32::MAX);
    });
    let error = compile_mir_execute_error(&module);
    assert!(
        error.contains("IOErrorKind case symbol"),
        "expected a symbol-mismatch diagnostic, got {error}"
    );
}

#[test]
fn adulterated_mir_rejects_a_wholly_unresolved_placeholder_layout() {
    // `hir::FileIoResultLayout::UNRESOLVED` (all-zero symbols) is the marker
    // `StandardLibrary::intrinsic_bindings()` uses before HIR lowering
    // replaces it; it must never reach execution as-is.
    let mut module = compile_mir(READ_SWITCH);
    mutate_read_all_text_layout(&mut module, |layout| {
        *layout = mir::FileIoResultLayout {
            ok_case: mir::SymbolId(0),
            ok_field: mir::SymbolId(0),
            error_case: mir::SymbolId(0),
            error_field: mir::SymbolId(0),
            io_error_kind_field: mir::SymbolId(0),
            io_error_os_code_field: mir::SymbolId(0),
            portable_kind_cases: [mir::SymbolId(0); 9],
        };
    });
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn adulterated_mir_rejects_a_kind_field_retyped_to_a_non_enum() {
    // Item 8 of the nominal-identity audit: an offset is never taken on
    // faith -- if the struct definition's `Kind` field is not actually the
    // enum type the resolved symbol should have, the backend must reject it
    // rather than silently reading a wrong-shaped payload at that offset.
    let mut module = compile_mir(READ_SWITCH);
    let io_error_kind_field = {
        let instruction = find_read_all_text_call(&mut module);
        let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
            unreachable!();
        };
        let (mir::Intrinsic::FileReadAllText(layout)
        | mir::Intrinsic::FileReadAllTextTemporary(layout)) = intrinsic
        else {
            unreachable!();
        };
        layout.io_error_kind_field
    };
    for definition in &mut module.structs {
        for field in &mut definition.fields {
            if field.symbol == io_error_kind_field {
                field.type_ = mir::Type::Int;
            }
        }
    }
    let error = compile_mir_execute_error(&module);
    assert!(!error.is_empty());
}

#[test]
fn a_user_declared_lookalike_result_io_error_and_io_error_kind_do_not_affect_read_all_text() {
    // Nominal-identity audit items 1-3: a program can declare its own
    // structurally-identical `Result`-shaped enum, `IOError`-shaped struct,
    // and `IOErrorKind`-shaped enum (same case/field *names*) without ever
    // influencing `ReadAllText`, since its resolved symbols point only at
    // the official `aster.core`/`aster.io` declarations.
    let source = "using aster.core;\nusing aster.io;\n\
        public enum FakeResult { Ok, Error }\n\
        public enum FakeIOErrorKind {\n\
            NotFound, PermissionDenied, AlreadyExists, InvalidPath, InvalidUtf8,\n\
            NotFile, NotDirectory, LimitExceeded, Other,\n\
        }\n\
        public struct FakeIOError { public FakeIOErrorKind Kind; public int OsCode; }\n\
        public int Main() {\n\
            FakeIOError fake = FakeIOError { Kind: FakeIOErrorKind.Other, OsCode: 7 };\n\
            switch (ReadAllText(\"a.txt\")) {\n\
                case Ok(text): return text.Length + fake.OsCode;\n\
                case Error(e): return -1000 - e.OsCode + fake.OsCode;\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "lookalike");
    assert_eq!(
        run_fs(source, "Main", backend),
        Ok(ExecutionValue::Int(9 + 7))
    );
}

fn compile_mir_execute_error(module: &mir::Module) -> String {
    execute_with_filesystem(module, "Main", Box::new(MemoryFileSystemBackend::new()))
        .expect_err("adulterated MIR must be rejected, never executed")
        .to_string()
}

// --- Memory ------------------------------------------------------------------------

#[test]
fn thousands_of_small_local_reads_recover_used_bytes() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                string text = ReadAllText(\"a.txt\")?;\n\
                total = total + text.Length;\n\
            }\n\
            return Result<int, IOError>.Ok(total);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "fixed");
    let (result, stats) = stats_fs(source, "Main", backend);
    assert_eq!(result, Ok(ExecutionValue::Int(5 * 5000)));
    assert_eq!(stats.used_bytes, 0);
}

#[test]
fn thousands_of_write_all_text_calls_allocate_no_new_aster_strings_or_objects() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Run() {\n\
            int total = 0;\n\
            for (int i = 0; i < 5000; i++) {\n\
                int count = WriteAllText(\"a.txt\", \"fixed\")?;\n\
                total = total + count;\n\
            }\n\
            return Result<int, IOError>.Ok(total);\n\
        }\n\
        public int Main() {\n\
            switch (Run()) { case Ok(v): return v; case Error(e): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new();
    let (result, stats) = stats_fs(source, "Main", backend);
    assert_eq!(result, Ok(ExecutionValue::Int(5 * 5000)));
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
}

#[test]
fn a_returned_read_all_text_string_survives_and_stays_valid() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<string, IOError> Run() { return ReadAllText(\"a.txt\"); }\n\
        public string Main() {\n\
            switch (Run()) { case Ok(text): return text; case Error(e): return \"error\"; }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("a.txt", "persisted-content");
    let (result, _) = stats_fs(source, "Main", backend);
    assert_eq!(
        result,
        Ok(ExecutionValue::String("persisted-content".to_owned()))
    );
}

#[test]
fn failures_do_not_allocate_aster_strings_and_do_not_mark_a_runtime_error() {
    let backend = MemoryFileSystemBackend::new();
    let (result, stats) = stats_fs(READ_SWITCH, "Main", backend);
    // The negative return value proves the switch reached `Error`, and
    // `result` being `Ok` (not the `BackendError` `execute*` would report for
    // `ExecutionContext::fail`) proves this failure never used that channel.
    assert!(matches!(result, Ok(ExecutionValue::Int(v)) if v < 0));
    assert_eq!(stats.string_allocations, 0);
}
