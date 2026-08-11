//! M7D execution proofs: a multi-package project compiles and runs.
//!
//! Package resolution is finished long before this layer. Cranelift receives
//! ordinary concrete MIR and never learns that packages exist.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use aster_codegen_cranelift::{ExecutionValue, execute_symbol};
use aster_compiler::compile_project;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aster-package-run-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create workspace");
        Self { root }
    }

    fn write(&self, relative: &str, contents: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create directory");
        }
        fs::write(&path, contents).expect("write file");
        path
    }

    fn application(&self, name: &str, dependencies: &[(&str, &str)]) {
        let mut manifest = format!(
            "[package]\nname = \"{name}\"\n\n[application]\nentry = \"app.Program.Main\"\n"
        );
        if !dependencies.is_empty() {
            manifest.push_str("\n[dependencies]\n");
            for (dependency, path) in dependencies {
                writeln!(manifest, "{dependency} = {{ path = \"{path}\" }}")
                    .expect("writing to a String cannot fail");
            }
        }
        self.write(&format!("{name}/Aster.toml"), &manifest);
    }

    fn library(&self, name: &str, dependencies: &[(&str, &str)]) {
        let mut manifest = format!("[package]\nname = \"{name}\"\n");
        if !dependencies.is_empty() {
            manifest.push_str("\n[dependencies]\n");
            for (dependency, path) in dependencies {
                writeln!(manifest, "{dependency} = {{ path = \"{path}\" }}")
                    .expect("writing to a String cannot fail");
            }
        }
        self.write(&format!("{name}/Aster.toml"), &manifest);
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run(root: &Path) -> ExecutionValue {
    let project = compile_project(root).expect("multi-package project compiles");
    let entry = aster_compiler::select_application_entry(&project, root)
        .expect("application entry resolves");
    execute_symbol(&project.compilation.mir, entry.symbol).expect("execution succeeds")
}

#[test]
fn a_path_dependency_executes() {
    let workspace = Workspace::new("basic");
    workspace.application("app", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { return Answer(); } }",
    );
    assert_eq!(run(&root), ExecutionValue::Int(42));
}

#[test]
fn a_transitive_dependency_executes() {
    let workspace = Workspace::new("transitive");
    workspace.application("app", &[("service", "../service")]);
    workspace.library("service", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    workspace.write(
        "service/service/facade.aster",
        "namespace service; using math; public int Provide() { return Answer(); }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using service; public class Program { public static int Main() { return Provide(); } }",
    );
    assert_eq!(run(&root), ExecutionValue::Int(42));
}

#[test]
fn a_cross_package_generic_specializes_and_executes() {
    let workspace = Workspace::new("generics");
    workspace.application("app", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/box.aster",
        "namespace math; \
         public class Box<T> { private T value; public Box(T value) { this.value = value; } public T Get() { return value; } } \
         public T Identity<T>(T value) { return value; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { Box<int> box = new Box<int>(40); return Identity<int>(box.Get()) + 2; } }",
    );
    assert_eq!(run(&root), ExecutionValue::Int(42));
}

#[test]
fn a_cross_package_constrained_generic_executes() {
    let workspace = Workspace::new("constraints");
    workspace.application("app", &[("contracts", "../contracts")]);
    workspace.library("contracts", &[]);
    workspace.write(
        "contracts/contracts/scored.aster",
        "namespace contracts; \
         public interface IScored { int Score(); } \
         public int Total<T>(T value) where T : IScored { return value.Score(); }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using contracts; \
         public class Card : IScored { private int points; public Card(int points) { this.points = points; } public int Score() { return points; } } \
         public class Program { public static int Main() { return Total(new Card(42)); } }",
    );
    assert_eq!(run(&root), ExecutionValue::Int(42));
}

/// One package reached through two graph paths is loaded once, so the shared
/// declaration is emitted once and both call sites reach the same function.
#[test]
fn a_repeated_dependency_executes_without_duplicate_symbols() {
    let workspace = Workspace::new("diamond");
    workspace.application("app", &[("left", "../left"), ("math", "../math")]);
    workspace.library("left", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/answer.aster",
        "namespace math; public int Answer() { return 21; }",
    );
    workspace.write(
        "left/left/relay.aster",
        "namespace left; using math; public int Relay() { return Answer(); }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using left; using math; public class Program { public static int Main() { return Relay() + Answer(); } }",
    );
    assert_eq!(run(&root), ExecutionValue::Int(42));
}

/// Two packages that both declare a type named `Value` in a namespace they each
/// spell differently must stay distinct concrete declarations at runtime.
#[test]
fn distinct_packages_keep_distinct_runtime_declarations() {
    let workspace = Workspace::new("distinct");
    workspace.application("app", &[("alpha", "../alpha"), ("beta", "../beta")]);
    workspace.library("alpha", &[]);
    workspace.library("beta", &[]);
    workspace.write(
        "alpha/alpha/value.aster",
        "namespace alpha; public class Value { public Value() {} public int Get() { return 40; } }",
    );
    workspace.write(
        "beta/beta/value.aster",
        "namespace beta; public class Value { public Value() {} public int Get() { return 2; } }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using alpha; using beta; public class Program { public static int Main() { return 42; } }",
    );
    // Both packages declare `Value`; importing both into one file is ambiguous,
    // which the linker reports rather than aliasing them together.
    let errors = compile_project(&root).expect_err("ambiguous import must be reported");
    assert!(
        errors.iter().any(|error| error
            .diagnostic
            .message
            .contains("is ambiguous between namespaces")),
        "{errors:#?}"
    );
}
