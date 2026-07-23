//! M2E: deterministic, non-recursive `aster.io.ListFiles` through the same
//! per-ExecutionContext filesystem seam as M2D's complete file operations.

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{
    ExecutionValue, execute, execute_with_filesystem, execute_with_filesystem_and_stats,
};
use aster_compiler::{compile_project, mir};
use aster_runtime::{FailingFileSystemBackend, FileSystemBackend, MemoryFileSystemBackend};

fn compile(source: &str) -> Result<mir::Module, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "aster-list-files-{}-{id}.aster",
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write temporary project");
    let compilation = compile_project(&path).map_err(|diagnostics| format!("{diagnostics:#?}"));
    std::fs::remove_file(&path).ok();
    compilation.map(|compilation| compilation.compilation.mir)
}

fn compile_mir(source: &str) -> mir::Module {
    compile(source).expect("source should compile")
}

fn run_fs(
    source: &str,
    backend: impl FileSystemBackend + 'static,
) -> Result<ExecutionValue, String> {
    execute_with_filesystem(&compile_mir(source), "Main", Box::new(backend))
        .map_err(|error| error.to_string())
}

const LIST: &str = "using aster.core;\nusing aster.io;\n\
    public string Main() {\n\
        switch (ListFiles(\"root\")) {\n\
            case Ok(files): return files[0] + \"|\" + files[1] + \"|\" + files[2];\n\
            case Error(error): return \"error\";\n\
        }\n\
    }";

fn direct_filesystem() -> MemoryFileSystemBackend {
    MemoryFileSystemBackend::new()
        .with_directory("root")
        .with_file("root/b.txt", "b")
        .with_file("root/A.txt", "a")
        .with_file("root/a.txt", "a")
        .with_file("root/non_text.bin", [0_u8, 255])
        .with_directory("root/Sub")
        .with_file("root/Sub/nested.txt", "nested")
        .with_symlink("root/link")
        .with_other("root/special")
}

#[test]
fn list_files_returns_direct_regular_paths_in_deterministic_order() {
    assert_eq!(
        run_fs(LIST, direct_filesystem()),
        Ok(ExecutionValue::String(
            "root/A.txt|root/a.txt|root/b.txt".to_owned()
        ))
    );
}

#[test]
fn list_files_can_be_used_directly_by_read_all_text_and_postfix_question() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> ReadFirst(string directory) {\n\
            string[] files = ListFiles(directory)?;\n\
            if (files.Length == 0) { return Result<int, IOError>.Ok(0); }\n\
            string text = ReadAllText(files[0])?;\n\
            return Result<int, IOError>.Ok(text.Length);\n\
        }\n\
        public int Main() {\n\
            switch (ReadFirst(\"root\")) { case Ok(value): return value; case Error(error): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new()
        .with_directory("root")
        .with_file("root/a.txt", "hello");
    assert_eq!(run_fs(source, backend), Ok(ExecutionValue::Int(5)));
}

/// Closing proof for M2: the four host-managed operations compose without
/// exposing a host handle. `binary.dat` is deliberately invalid UTF-8 and is
/// handled as an ordinary `Result` error rather than aborting the scan.
#[test]
fn filesystem_indexer_composes_list_read_combine_and_write_with_memory_backend() {
    let source = "using aster.core;\nusing aster.io;\n\
        public Result<int, IOError> Index(string input, string output) {\n\
            string[] files = ListFiles(input)?;\n\
            if (files.Length != 4 || !files[0].EndsWith(\"a.txt\") || !files[1].EndsWith(\"b.txt\") || !files[2].EndsWith(\"binary.dat\") || !files[3].EndsWith(\"empty.txt\")) {\n\
                return Result<int, IOError>.Error(IOError { Kind: IOErrorKind.Other, OsCode: 0 });\n\
            }\n\
            int readable = 0;\n\
            int invalid = 0;\n\
            int characters = 0;\n\
            for (int i = 0; i < files.Length; i++) {\n\
                switch (ReadAllText(files[i])) {\n\
                    case Ok(text): readable = readable + 1; characters = characters + text.Length;\n\
                    case Error(error): switch (error.Kind) {\n\
                        case InvalidUtf8: invalid = invalid + 1;\n\
                        default: return Result<int, IOError>.Error(error);\n\
                    }\n\
                }\n\
            }\n\
            string summaryPath = CombinePath(output, \"summary.txt\")?;\n\
            string summary = \"readable=\" + readable.ToString() + \";invalid=\" + invalid.ToString() + \";chars=\" + characters.ToString();\n\
            int written = WriteAllText(summaryPath, summary)?;\n\
            if (written != summary.Length) { return Result<int, IOError>.Error(IOError { Kind: IOErrorKind.Other, OsCode: 0 }); }\n\
            return Result<int, IOError>.Ok(readable * 100 + invalid * 10 + characters);\n\
        }\n\
        public int Main() {\n\
            switch (Index(\"input\", \"output\")) { case Ok(value): return value; case Error(error): return -1; }\n\
        }";
    let backend = MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/b.txt", "β")
        .with_file("input/a.txt", "alfa")
        .with_file("input/empty.txt", "")
        .with_file("input/binary.dat", [0xff_u8, 0xfe])
        .with_directory("input/nested")
        .with_file("input/nested/ignored.txt", "ignored")
        .with_directory("output");
    let inspection = backend.clone();

    assert_eq!(run_fs(source, backend), Ok(ExecutionValue::Int(315)));
    assert_eq!(
        inspection.read("output/summary.txt"),
        Some(b"readable=3;invalid=1;chars=5".to_vec())
    );
}

#[test]
fn list_files_reports_invalid_missing_and_not_directory_paths() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ListFiles(\"file\")) {\n\
                case Ok(files): return -1;\n\
                case Error(error): switch (error.Kind) {\n\
                    case NotDirectory: return 7; case NotFound: return 1; case Other: return 9;\n\
                    case PermissionDenied: return 2; case AlreadyExists: return 3; case InvalidPath: return 4;\n\
                    case InvalidUtf8: return 5; case NotFile: return 6; case LimitExceeded: return 8;\n\
                }\n\
            }\n\
        }";
    let backend = MemoryFileSystemBackend::new().with_file("file", "content");
    assert_eq!(run_fs(source, backend), Ok(ExecutionValue::Int(7)));

    let missing = source.replace("\"file\"", "\"missing\"");
    assert_eq!(
        run_fs(&missing, MemoryFileSystemBackend::new()),
        Ok(ExecutionValue::Int(1))
    );
}

#[test]
fn list_files_accepts_only_its_official_single_string_signature() {
    for source in [
        "using aster.io; public int Bad() { ListFiles(); return 0; }",
        "using aster.io; public int Bad() { ListFiles(\"a\", \"b\"); return 0; }",
        "using aster.io; public int Bad() { ListFiles(1); return 0; }",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn list_files_propagates_host_iteration_failure_without_partial_result() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Main() {\n\
            switch (ListFiles(\"root\")) {\n\
                case Ok(files): return files.Length;\n\
                case Error(error): switch (error.Kind) { case PermissionDenied: return 2;\n\
                    case NotFound: return 1; case AlreadyExists: return 3; case InvalidPath: return 4;\n\
                    case InvalidUtf8: return 5; case NotFile: return 6; case NotDirectory: return 7;\n\
                    case LimitExceeded: return 8; case Other: return 9; }\n\
            }\n\
        }";
    assert_eq!(
        run_fs(
            source,
            FailingFileSystemBackend::new(io::ErrorKind::PermissionDenied)
        ),
        Ok(ExecutionValue::Int(2))
    );
}

#[test]
fn list_files_is_rejected_from_worker_targets_but_a_user_method_with_the_name_is_not() {
    let worker = "using aster.core;\nusing aster.io;\n\
        public void Helper(int index) { ListFiles(\"root\"); }\n\
        public void Body(int index) { Helper(index); }\n\
        public int Main() { Parallel.For(0, 1, Body); return 0; }";
    let error = execute(&compile_mir(worker), "Main")
        .expect_err("filesystem I/O in a worker must be rejected")
        .to_string();
    assert!(error.contains("ListFiles"), "got {error}");

    let ordinary = "public class Tool { public int ListFiles(string path) { return 7; } }\n\
        public int Main() { Tool tool = new Tool(); return tool.ListFiles(\"root\"); }";
    assert_eq!(
        execute(&compile_mir(ordinary), "Main"),
        Ok(ExecutionValue::Int(7))
    );
}

#[test]
fn list_files_selects_temporary_or_persistent_storage_with_its_result() {
    let source = "using aster.core;\nusing aster.io;\n\
        public int Local() { switch (ListFiles(\"root\")) { case Ok(files): return files.Length; case Error(e): return -1; } }\n\
        public Result<string[], IOError> Escaping() { return ListFiles(\"root\"); }\n\
        public int Main() { return Local(); }";
    let module = compile_mir(source);
    let regions = module
        .functions
        .iter()
        .map(|function| {
            let intrinsic = function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .find_map(|instruction| match instruction {
                    mir::Instruction::CallIntrinsic { intrinsic, .. } => Some(*intrinsic),
                    _ => None,
                });
            (function.name.as_str(), intrinsic)
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        regions
            .iter()
            .find(|(name, _)| *name == "Local")
            .and_then(|(_, intrinsic)| *intrinsic),
        Some(mir::Intrinsic::FileListFilesTemporary(_))
    ));
    assert!(matches!(
        regions
            .iter()
            .find(|(name, _)| *name == "Escaping")
            .and_then(|(_, intrinsic)| *intrinsic),
        Some(mir::Intrinsic::FileListFiles(_))
    ));
}

#[test]
fn list_files_rejects_adulterated_mir_before_jit() {
    let mut module = compile_mir(LIST);
    let instruction = module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::FileListFiles(_)
                        | mir::Intrinsic::FileListFilesTemporary(_),
                    ..
                }
            )
        })
        .expect("ListFiles call exists");
    let mir::Instruction::CallIntrinsic { arguments, .. } = instruction else {
        unreachable!();
    };
    arguments[0].type_ = mir::Type::Int;
    let error = execute(&module, "Main")
        .expect_err("bad ListFiles MIR must be rejected")
        .to_string();
    assert!(
        error.contains("intrinsic") || error.contains("ListFiles"),
        "got {error}"
    );
}

#[test]
fn list_files_rejects_unresolved_nominal_result_metadata_before_jit() {
    let mut module = compile_mir(LIST);
    let instruction = module
        .functions
        .iter_mut()
        .flat_map(|function| &mut function.blocks)
        .flat_map(|block| &mut block.instructions)
        .find(|instruction| {
            matches!(
                instruction,
                mir::Instruction::CallIntrinsic {
                    intrinsic: mir::Intrinsic::FileListFiles(_)
                        | mir::Intrinsic::FileListFilesTemporary(_),
                    ..
                }
            )
        })
        .expect("ListFiles call exists");
    let mir::Instruction::CallIntrinsic { intrinsic, .. } = instruction else {
        unreachable!();
    };
    match intrinsic {
        mir::Intrinsic::FileListFiles(layout) | mir::Intrinsic::FileListFilesTemporary(layout) => {
            *layout = mir::FileIoResultLayout::UNRESOLVED;
        }
        _ => unreachable!(),
    }

    let error = execute(&module, "Main")
        .expect_err("unresolved nominal metadata must not reach the filesystem ABI")
        .to_string();
    assert!(
        error.contains("Result") || error.contains("ListFiles") || error.contains("symbol"),
        "got {error}"
    );
}

#[test]
fn list_files_local_repetition_rewinds_temporary_arrays_and_failure_allocates_no_result() {
    let local = "using aster.core;\nusing aster.io;\n\
        public int One() { switch (ListFiles(\"root\")) { case Ok(files): return files.Length; case Error(e): return -1; } }\n\
        public int Main() { int total = 0; for (int i = 0; i < 8; i = i + 1) { total = total + One(); } return total; }";
    let backend = MemoryFileSystemBackend::new()
        .with_directory("root")
        .with_file("root/a.txt", "a");
    let (result, stats) =
        execute_with_filesystem_and_stats(&compile_mir(local), "Main", Box::new(backend))
            .expect("local enumeration runs");
    assert_eq!(result, ExecutionValue::Int(8));
    assert_eq!(stats.total_allocations, 16);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.array_allocations, 8);
    assert_eq!(stats.string_allocations, 8);
    assert_eq!(stats.used_bytes, 0);
    assert!(
        stats.reserved_bytes > 0,
        "temporary arena keeps reusable pages"
    );

    let failure = "using aster.core;\nusing aster.io;\n\
        public int Main() { switch (ListFiles(\"missing\")) { case Ok(files): return -1; case Error(e): return 0; } }";
    let (_, stats) = execute_with_filesystem_and_stats(
        &compile_mir(failure),
        "Main",
        Box::new(MemoryFileSystemBackend::new()),
    )
    .expect("filesystem error is an ASTER Result, not a runtime failure");
    assert_eq!(stats.total_allocations, 0);
    assert_eq!(stats.object_allocations, 0);
    assert_eq!(stats.array_allocations, 0);
    assert_eq!(stats.string_allocations, 0);
    assert_eq!(stats.used_bytes, 0);
}
