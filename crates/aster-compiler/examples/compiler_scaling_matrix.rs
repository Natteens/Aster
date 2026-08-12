//! Manual compiler-scaling matrix for large enum switches and many-file projects.
//! Timings are machine-local evidence only; no threshold is asserted in tests.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

use aster_compiler::{compile, compile_project};

const SAMPLES: usize = 3;

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn enum_source(cases: usize) -> String {
    let mut source = String::from("public enum Big { ");
    for index in 0..cases {
        write!(source, "C{index}, ").expect("write source");
    }
    write!(
        source,
        "}} public int Main() {{ Big value = Big.C{}; switch (value) {{ ",
        cases - 1
    )
    .expect("write source");
    for index in 0..cases {
        write!(source, "case C{index}: return {index}; ").expect("write source");
    }
    source.push_str("} }");
    source
}

fn enum_case(cases: usize) {
    let source = enum_source(cases);
    let mut parse_ms = Vec::with_capacity(SAMPLES);
    let mut compile_ms = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        let tokens = aster_syntax::lex(&source).expect("enum lexes");
        aster_syntax::parse(tokens).expect("enum parses");
        parse_ms.push(start.elapsed().as_secs_f64() * 1000.0);

        let start = Instant::now();
        let output = compile(&source).expect("enum compiles");
        assert_eq!(output.mir.enums[0].cases.len(), cases);
        compile_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "enum cases={cases:<5} parse_ms={:>9.3} compile_ms={:>9.3}",
        median(parse_ms),
        median(compile_ms)
    );
}

struct TempProject(PathBuf);

impl TempProject {
    fn new(files: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "aster-compiler-scaling-{}-{files}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("app")).expect("create benchmark project");
        fs::write(
            root.join("main.aster"),
            format!(
                "using app; public int Main() {{ return Value{}(); }}",
                files - 1
            ),
        )
        .expect("write root source");
        for index in 0..files {
            fs::write(
                root.join("app").join(format!("value{index:05}.aster")),
                format!("namespace app; public int Value{index}() {{ return {index}; }}"),
            )
            .expect("write namespace source");
        }
        Self(root)
    }

    fn root_source(&self) -> PathBuf {
        self.0.join("main.aster")
    }
}

impl Drop for TempProject {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).expect("remove benchmark project");
    }
}

fn read_and_parse(directory: &Path) {
    let mut files = fs::read_dir(directory)
        .expect("read namespace")
        .map(|entry| entry.expect("read entry").path())
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let source = fs::read_to_string(file).expect("read source");
        let tokens = aster_syntax::lex(&source).expect("source lexes");
        aster_syntax::parse(tokens).expect("source parses");
    }
}

fn file_case(files: usize) {
    let project = TempProject::new(files);
    let mut parse_ms = Vec::with_capacity(SAMPLES);
    let mut compile_ms = Vec::with_capacity(SAMPLES);
    for _ in 0..SAMPLES {
        let start = Instant::now();
        read_and_parse(&project.0.join("app"));
        parse_ms.push(start.elapsed().as_secs_f64() * 1000.0);

        let start = Instant::now();
        let output = compile_project(&project.root_source()).expect("project compiles");
        assert_eq!(
            output
                .sources
                .iter()
                .filter(|source| source.path.extension().is_some_and(|ext| ext == "aster"))
                .count(),
            files + 1
        );
        compile_ms.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    println!(
        "files count={files:<5} read_parse_ms={:>9.3} compile_ms={:>9.3}",
        median(parse_ms),
        median(compile_ms)
    );
}

fn main() {
    for cases in [500, 1_000, 2_000, 5_000] {
        enum_case(cases);
    }
    for files in [100, 500, 1_000, 1_500, 2_000] {
        file_case(files);
    }
}
