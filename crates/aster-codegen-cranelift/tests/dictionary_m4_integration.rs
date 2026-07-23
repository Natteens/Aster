//! M4F: one real ASTER program exercising the complete Dictionary surface.

use std::sync::atomic::{AtomicU64, Ordering};

use aster_codegen_cranelift::{ExecutionValue, execute_with_filesystem};
use aster_compiler::{compile_project, mir};
use aster_runtime::{FileSystemBackend, MemoryFileSystemBackend};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn compile(source: &str) -> mir::Module {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("aster-m4f-{}-{id}.aster", std::process::id()));
    std::fs::write(&path, source).expect("write temporary M4F project");
    let compilation = compile_project(&path).expect("M4F source should compile");
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

const PROGRAM: &str = r#"
    using aster.core;
    using aster.io;
    using aster.collections;

    public Result<int, IOError> Analyze(string inputDir, string outputDir)
    {
        string[] files = ListFiles(inputDir)?;
        Dictionary<string, int> fileCounts = new Dictionary<string, int>();
        Dictionary<char, int> characterCounts = new Dictionary<char, int>();
        Dictionary<string, int> categories = new Dictionary<string, int>();
        List<int> scalarCounts = new List<int>();

        categories.Add("valid", 0);
        categories.Add("invalid", 0);
        categories.Add("temporary", 1);
        categories.Remove("temporary");

        foreach (string file in files)
        {
            switch (ReadAllText(file))
            {
                case Ok(text):
                    int scalarCount = 0;
                    foreach (char scalar in text)
                    {
                        scalarCount = scalarCount + 1;
                        switch (characterCounts.TryGet(scalar))
                        {
                            case Some(current):
                                characterCounts.Set(scalar, current + 1);
                            case None:
                                characterCounts.Add(scalar, 1);
                        }
                    }
                    fileCounts.Add(file, scalarCount);
                    scalarCounts.Add(scalarCount);
                    switch (categories.TryGet("valid"))
                    {
                        case Some(current):
                            categories.Set("valid", current + 1);
                        case None:
                            return Result<int, IOError>.Ok(-10);
                    }
                case Error(error):
                    if (error.Kind != IOErrorKind.InvalidUtf8)
                    {
                        return Result<int, IOError>.Error(error);
                    }
                    switch (categories.TryGet("invalid"))
                    {
                        case Some(current):
                            categories.Set("invalid", current + 1);
                        case None:
                            return Result<int, IOError>.Ok(-11);
                    }
            }
        }

        if (fileCounts.ContainsKey("input/nested/ignored.txt"))
        {
            return Result<int, IOError>.Ok(-12);
        }
        switch (fileCounts.TryGet("input/b.txt"))
        {
            case Some(count):
                if (count != 4) { return Result<int, IOError>.Ok(-13); }
            case None:
                return Result<int, IOError>.Ok(-14);
        }

        DictionaryEntry<string, int>[] fileEntries = fileCounts.Entries();
        DictionaryEntry<char, int>[] characterEntries = characterCounts.Entries();

        categories.Remove("valid");
        categories.Add("valid", scalarCounts.Length);
        DictionaryEntry<string, int>[] categoryEntries = categories.Entries();

        int totalFromEntries = 0;
        foreach (DictionaryEntry<string, int> entry in fileEntries)
        {
            totalFromEntries = totalFromEntries + entry.Value;
        }

        int totalFromList = 0;
        foreach (int count in scalarCounts)
        {
            totalFromList = totalFromList + count;
        }
        if (totalFromEntries != totalFromList)
        {
            return Result<int, IOError>.Ok(-15);
        }

        if (categoryEntries.Length != 2) { return Result<int, IOError>.Ok(-16); }
        if (categoryEntries[0].Key != "invalid") { return Result<int, IOError>.Ok(-17); }
        if (categoryEntries[1].Key != "valid") { return Result<int, IOError>.Ok(-18); }

        fileCounts.Set("input/a.txt", 99);
        if (fileEntries[0].Value != 5)
        {
            return Result<int, IOError>.Ok(-19);
        }

        string resultPath = CombinePath(outputDir, "result.txt")?;
        string summary =
            "files=" + fileEntries.Length.ToString()
            + ";total=" + totalFromEntries.ToString()
            + ";unique=" + characterEntries.Length.ToString()
            + ";categories=" + categoryEntries[0].Key + "," + categoryEntries[1].Key;
        int written = WriteAllText(resultPath, summary)?;
        return Result<int, IOError>.Ok(
            fileEntries.Length + totalFromEntries + characterEntries.Length
            + categoryEntries.Length
        );
    }

    public int Main()
    {
        switch (Analyze("input", "output"))
        {
            case Ok(value): return value;
            case Error(error): return -1;
        }
    }
"#;

fn backend() -> MemoryFileSystemBackend {
    MemoryFileSystemBackend::new()
        .with_directory("input")
        .with_file("input/a.txt", "alpha")
        .with_file("input/b.txt", "beta")
        .with_file("input/empty.txt", "")
        .with_file("input/invalid.bin", [0xFF_u8, 0xFE])
        .with_file("input/unicode.txt", "\u{03B1}\u{03B2}\u{03B3}")
        .with_directory("input/nested")
        .with_file("input/nested/ignored.txt", "ignored")
        .with_directory("output")
}

#[test]
fn m4f_integrated_dictionary_program_runs_with_in_memory_filesystem() {
    let backend = backend();
    let inspection = backend.clone();

    assert_eq!(run_fs(PROGRAM, backend), Ok(ExecutionValue::Int(28)));
    assert_eq!(
        inspection.read("output/result.txt"),
        Some(b"files=4;total=12;unique=10;categories=invalid,valid".to_vec())
    );
}
