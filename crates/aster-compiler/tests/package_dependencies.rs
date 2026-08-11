//! M7D: package identity and deterministic local path dependencies.
//!
//! Every fixture is a real multi-package layout on disk. Nothing here touches
//! the network; a path dependency is an ordinary local filesystem input.

use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use aster_compiler::compile_project;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Workspace {
    root: PathBuf,
}

impl Workspace {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("aster-package-{label}-{}-{id}", std::process::id()));
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

    /// A package with an application entry, plus its `[dependencies]` table.
    fn application(&self, name: &str, dependencies: &[(&str, &str)]) {
        let mut manifest = format!(
            "schema = 2\n\n[package]\nname = \"{name}\"\n\n[application]\nentry = \"app.Program.Main\"\n"
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

    /// A library package: package identity, no application entry.
    fn library(&self, name: &str, dependencies: &[(&str, &str)]) {
        let mut manifest = format!("schema = 2\n\n[package]\nname = \"{name}\"\n");
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

fn messages(path: &Path) -> Vec<String> {
    compile_project(path)
        .expect_err("the package graph should be rejected")
        .into_iter()
        .map(|error| error.diagnostic.message)
        .collect()
}

fn assert_reports(path: &Path, expected: &str) {
    let messages = messages(path);
    assert!(
        messages.iter().any(|message| message.contains(expected)),
        "missing `{expected}` in {messages:#?}"
    );
}

/// Compiles the graph and resolves the application entry. Execution proofs
/// live in `aster-codegen-cranelift`, which is downstream of this crate.
fn links(root: &Path) -> aster_compiler::ProjectCompilation {
    let project = compile_project(root).expect("multi-package project compiles");
    aster_compiler::select_application_entry(&project, root).expect("application entry resolves");
    project
}

#[test]
fn a_root_package_calls_a_public_declaration_from_a_path_dependency() {
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
    links(&root);
}

#[test]
fn a_transitive_dependency_flows_through_the_graph() {
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
    links(&root);
}

/// A dependency's namespaces are reachable only from the package that declared
/// it. Transitive packages are not silently in scope.
#[test]
fn a_transitive_package_is_not_directly_usable() {
    let workspace = Workspace::new("no-transitive-scope");
    workspace.application("app", &[("service", "../service")]);
    workspace.library("service", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    workspace.write(
        "service/service/facade.aster",
        "namespace service; public int Provide() { return 1; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "was not found in package `app` or its dependencies");
}

#[test]
fn internal_still_crosses_namespaces_inside_one_package() {
    let workspace = Workspace::new("same-package-internal");
    workspace.application("app", &[]);
    workspace.write(
        "app/helpers/support.aster",
        "namespace helpers; internal int Hidden() { return 42; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using helpers; public class Program { public static int Main() { return Hidden(); } }",
    );
    links(&root);
}

#[test]
fn internal_does_not_cross_a_package_boundary() {
    let workspace = Workspace::new("cross-package-internal");
    workspace.application("app", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/answer.aster",
        "namespace math; internal int Hidden() { return 42; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { return Hidden(); } }",
    );
    assert_reports(&root, "is internal to package `math`");
}

#[test]
fn an_internal_type_does_not_cross_a_package_boundary() {
    let workspace = Workspace::new("cross-package-internal-type");
    workspace.application("app", &[("math", "../math")]);
    workspace.library("math", &[]);
    workspace.write(
        "math/math/box.aster",
        "namespace math; internal class Hidden { public Hidden() {} public int Value() { return 42; } }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { Hidden value = new Hidden(); return value.Value(); } }",
    );
    assert_reports(&root, "is internal to package `math`");
}

#[test]
fn a_public_generic_specializes_across_a_package_boundary() {
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
        "namespace app; using math; public class Program { public static int Main() { Box<int> box = new Box<int>(42); return Identity<int>(box.Get()); } }",
    );
    links(&root);
}

/// The interface-only constraint surface from M7C must keep working when the
/// constraint, the generic, and the satisfying class span packages.
#[test]
fn a_constrained_generic_specializes_across_a_package_boundary() {
    let workspace = Workspace::new("generic-constraints");
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
    links(&root);
    // The specialization is concrete: no open parameter survives to HIR.
    let project = compile_project(&root).expect("constrained generic project compiles");
    for item in &project.compilation.module.items {
        let parameters = match item {
            aster_syntax::Item::Class(value)
            | aster_syntax::Item::Struct(value)
            | aster_syntax::Item::Interface(value) => &value.type_parameters,
            aster_syntax::Item::Enum(value) => &value.type_parameters,
            aster_syntax::Item::Function(value) => &value.type_parameters,
            aster_syntax::Item::Variable(_) => continue,
        };
        assert!(parameters.is_empty(), "open type parameter reached HIR");
    }
}

/// Reaching one package through two graph paths must load it once, so no
/// duplicate symbol is generated.
#[test]
fn a_repeated_dependency_is_loaded_once() {
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
    links(&root);

    let project = compile_project(&root).expect("diamond graph compiles");
    let loaded = project
        .dependency_paths()
        .into_iter()
        .filter(|path| path.ends_with("answer.aster"))
        .count();
    assert_eq!(loaded, 1, "the shared package must be loaded once");
}

/// Two packages that spell the same namespace and type must stay distinct
/// nominal declarations rather than silently aliasing.
#[test]
fn packages_sharing_a_namespace_spelling_stay_distinct() {
    let workspace = Workspace::new("overlapping-names");
    workspace.application("app", &[("alpha", "../alpha"), ("beta", "../beta")]);
    workspace.library("alpha", &[]);
    workspace.library("beta", &[]);
    workspace.write(
        "alpha/shared/value.aster",
        "namespace shared; public int Value() { return 40; }",
    );
    workspace.write(
        "beta/shared/value.aster",
        "namespace shared; public int Value() { return 2; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using shared; public class Program { public static int Main() { return Value(); } }",
    );
    // The `using` is genuinely ambiguous and must be reported, not silently
    // resolved to whichever package happened to load first.
    assert_reports(&root, "is provided by more than one package");
}

#[test]
fn overlapping_namespaces_reached_separately_keep_distinct_identities() {
    let workspace = Workspace::new("distinct-identities");
    workspace.application("app", &[("alpha", "../alpha")]);
    workspace.library("alpha", &[("beta", "../beta")]);
    workspace.library("beta", &[]);
    workspace.write(
        "alpha/shared/value.aster",
        "namespace shared; public int Value() { return 40; }",
    );
    workspace.write(
        "beta/shared/value.aster",
        "namespace shared; public int Value() { return 2; }",
    );
    workspace.write(
        "alpha/alpha/relay.aster",
        "namespace alpha; using shared; public int FromAlpha() { return Value(); }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using alpha; public class Program { public static int Main() { return FromAlpha() + 2; } }",
    );
    // `alpha` sees its own `shared`; `beta`'s identically spelled namespace is
    // a different declaration and does not interfere.
    links(&root);
}

#[test]
fn a_missing_dependency_path_is_a_controlled_error() {
    let workspace = Workspace::new("missing-path");
    workspace.application("app", &[("math", "../math")]);
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "does not exist");
}

#[test]
fn a_dependency_without_a_manifest_is_a_controlled_error() {
    let workspace = Workspace::new("not-a-package");
    workspace.application("app", &[("math", "../math")]);
    workspace.write(
        "math/math/answer.aster",
        "namespace math; public int A() { return 1; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "is not an ASTER package: no Aster.toml");
}

#[test]
fn a_malformed_dependency_manifest_is_a_controlled_error() {
    let workspace = Workspace::new("malformed-dependency");
    workspace.application("app", &[("math", "../math")]);
    workspace.write("math/Aster.toml", "schema = 2\n\n[package\nname = ");
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "invalid Aster.toml");
}

#[test]
fn an_incompatible_dependency_schema_is_a_controlled_error() {
    let workspace = Workspace::new("dependency-schema");
    workspace.application("app", &[("math", "../math")]);
    workspace.write("math/Aster.toml", "schema = 999\n");
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "unsupported Aster.toml schema `999`");
}

#[test]
fn a_schema_one_dependency_has_no_package_identity() {
    let workspace = Workspace::new("schema-one-dependency");
    workspace.application("app", &[("math", "../math")]);
    workspace.write(
        "math/Aster.toml",
        "schema = 1\n\n[application]\nentry = \"math.Program.Main\"\n",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "schema 1, which has no package identity");
}

#[test]
fn a_dependency_cycle_is_a_controlled_error() {
    let workspace = Workspace::new("cycle");
    workspace.application("app", &[("left", "../left")]);
    workspace.library("left", &[("right", "../right")]);
    workspace.library("right", &[("left", "../left")]);
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "dependency cycle:");
}

#[test]
fn a_dependency_name_must_match_its_declared_package_name() {
    let workspace = Workspace::new("name-mismatch");
    workspace.application("app", &[("maths", "../math")]);
    workspace.library("math", &[]);
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "resolves to a package named `math`");
}

#[test]
fn two_packages_cannot_claim_the_same_identity() {
    let workspace = Workspace::new("duplicate-identity");
    workspace.application("app", &[("left", "../left"), ("right", "../right")]);
    // Both dependencies declare the same package name from different roots.
    workspace.write(
        "left/Aster.toml",
        "schema = 2\n\n[package]\nname = \"left\"\n\n[dependencies]\nshared = { path = \"../shared_one\" }\n",
    );
    workspace.write(
        "right/Aster.toml",
        "schema = 2\n\n[package]\nname = \"right\"\n\n[dependencies]\nshared = { path = \"../shared_two\" }\n",
    );
    workspace.write(
        "shared_one/Aster.toml",
        "schema = 2\n\n[package]\nname = \"shared\"\n",
    );
    workspace.write(
        "shared_two/Aster.toml",
        "schema = 2\n\n[package]\nname = \"shared\"\n",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "duplicate package identity `shared`");
}

#[test]
fn a_transitive_dependency_failure_is_reported() {
    let workspace = Workspace::new("transitive-failure");
    workspace.application("app", &[("service", "../service")]);
    workspace.library("service", &[("math", "../math")]);
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "does not exist");
}

#[test]
fn a_using_cannot_escape_a_package_without_a_declared_dependency() {
    let workspace = Workspace::new("no-escape");
    workspace.application("app", &[]);
    // A sibling directory that is not declared as a dependency.
    workspace.write(
        "outside/secret.aster",
        "namespace outside; public int Secret() { return 1; }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using outside; public class Program { public static int Main() { return 0; } }",
    );
    assert_reports(&root, "was not found in package");
}

/// The graph is resolved from the declaring manifest, so the same checkout
/// produces the same result from any working directory.
#[test]
fn dependency_resolution_is_independent_of_the_working_directory() {
    let workspace = Workspace::new("cwd-independent");
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

    let absolute = links(&root).dependency_paths();
    let relative = {
        let previous = std::env::current_dir().expect("current directory");
        std::env::set_current_dir(workspace.root.join("math")).expect("enter dependency directory");
        let paths = links(&root).dependency_paths();
        std::env::set_current_dir(previous).expect("restore directory");
        paths
    };
    assert_eq!(absolute, relative);
}

#[test]
fn dependency_manifests_are_watched_inputs() {
    let workspace = Workspace::new("watch-inputs");
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
    let project = compile_project(&root).expect("project compiles");
    let paths = project.dependency_paths();
    for expected in [
        workspace.root.join("app/Aster.toml"),
        workspace.root.join("math/Aster.toml"),
        workspace.root.join("math/math/answer.aster"),
    ] {
        let expected = fs::canonicalize(&expected).expect("canonical path");
        assert!(
            paths.contains(&expected),
            "missing {expected:?} in {paths:#?}"
        );
    }
    // Deterministic and duplicate-free.
    let mut sorted = paths.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(paths, sorted);
}

#[test]
fn a_library_package_checks_without_an_application_entry() {
    let workspace = Workspace::new("library-check");
    workspace.library("math", &[]);
    let root = workspace.write(
        "math/math/answer.aster",
        "namespace math; public int Answer() { return 42; }",
    );
    let project = compile_project(&root).expect("library package compiles");
    assert!(!project.requires_application_entry());
}

/// The root application owns the entry point; a dependency that also declares
/// one must not compete for it.
#[test]
fn only_the_root_application_supplies_the_entry_point() {
    let workspace = Workspace::new("root-entry-authority");
    workspace.application("app", &[("tool", "../tool")]);
    workspace.write(
        "tool/Aster.toml",
        "schema = 2\n\n[package]\nname = \"tool\"\n\n[application]\nentry = \"tool.Program.Main\"\n",
    );
    workspace.write(
        "tool/tool/main.aster",
        "namespace tool; public class Program { public static int Main() { return 1; } public static int Helper() { return 41; } }",
    );
    let root = workspace.write(
        "app/app/main.aster",
        "namespace app; using tool; public class Program { public static int Main() { return 42; } }",
    );
    links(&root);
}

/// A schema 2 package's declared name is its nominal identity independent of
/// graph position: `math::math::Answer` names the exact same declaration
/// whether `math` is compiled as the root project or pulled in as a
/// dependency. The declaration lives in a file other than the literal CLI
/// entry file in both fixtures, so this isolates the package-identity rule
/// from the separate, unrelated `--function`/entry-file bare-name contract.
#[test]
fn a_schema_two_package_keeps_the_same_identity_as_root_or_as_a_dependency() {
    let as_dependency = Workspace::new("identity-as-dependency");
    as_dependency.application("app", &[("math", "../math")]);
    as_dependency.library("math", &[]);
    as_dependency.write(
        "math/math/answer.aster",
        "namespace math; public class Answer { public Answer() {} public int Get() { return 42; } }",
    );
    let dependency_root = as_dependency.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { return new Answer().Get(); } }",
    );
    let dependency_project =
        compile_project(&dependency_root).expect("math as a dependency compiles");
    let dependency_hir = format!("{:#?}", dependency_project.compilation.hir);
    assert!(
        dependency_hir.contains("\"math::math::Answer\""),
        "{dependency_hir}"
    );

    let as_root = Workspace::new("identity-as-root");
    as_root.library("math", &[]);
    let standalone_root = as_root.write(
        "math/math/answer.aster",
        "namespace math; public class Answer { public Answer() {} public int Get() { return 42; } }",
    );
    as_root.write(
        "math/math/entry.aster",
        "namespace math; public int Root() { return 0; }",
    );
    let standalone_project = compile_project(&standalone_root).expect("math compiled standalone");
    let standalone_hir = format!("{:#?}", standalone_project.compilation.hir);
    assert!(
        standalone_hir.contains("\"math::math::Answer\""),
        "{standalone_hir}"
    );
}

/// A specialized generic type's identity flows through the same
/// package-qualified rule as an ordinary declaration, so it stays concrete
/// and consistent whether its package is compiled as root or a dependency.
#[test]
fn a_generic_specialization_keeps_the_same_identity_as_root_or_as_a_dependency() {
    let source = |package: &str| {
        format!(
            "namespace {package}; public class Box<T> {{ private T value; public Box(T value) {{ this.value = value; }} public T Get() {{ return value; }} }}"
        )
    };

    let as_dependency = Workspace::new("generic-identity-as-dependency");
    as_dependency.application("app", &[("math", "../math")]);
    as_dependency.library("math", &[]);
    as_dependency.write("math/math/box.aster", &source("math"));
    let dependency_root = as_dependency.write(
        "app/app/main.aster",
        "namespace app; using math; public class Program { public static int Main() { Box<int> box = new Box<int>(42); return box.Get(); } }",
    );
    let dependency_hir = format!(
        "{:#?}",
        compile_project(&dependency_root)
            .expect("generic dependency compiles")
            .compilation
            .hir
    );
    assert!(
        dependency_hir.contains("\"math::math::Box<int>\""),
        "{dependency_hir}"
    );

    let as_root = Workspace::new("generic-identity-as-root");
    as_root.library("math", &[]);
    let standalone_root = as_root.write("math/math/box.aster", &source("math"));
    as_root.write(
        "math/math/entry.aster",
        "namespace math; public int Root() { Box<int> box = new Box<int>(42); return box.Get(); }",
    );
    let standalone_hir = format!(
        "{:#?}",
        compile_project(&standalone_root)
            .expect("generic package compiles standalone")
            .compilation
            .hir
    );
    assert!(
        standalone_hir.contains("\"math::math::Box<int>\""),
        "{standalone_hir}"
    );
}

/// Schema 1 has no package identity, so its non-root-file declarations keep
/// their exact pre-M7D `namespace::name` spelling. This is the compatibility
/// boundary the package-identity fix must not cross.
#[test]
fn schema_one_keeps_its_legacy_namespace_only_identity() {
    let workspace = Workspace::new("schema-one-identity");
    workspace.write(
        "Aster.toml",
        "schema = 1\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    workspace.write(
        "app/helpers.aster",
        "namespace app; public class Helper { public Helper() {} public int Get() { return 42; } }",
    );
    let root = workspace.write(
        "app/main.aster",
        "namespace app; public class Program { public static int Main() { return new Helper().Get(); } }",
    );
    let project = compile_project(&root).expect("schema 1 project compiles");
    let hir = format!("{:#?}", project.compilation.hir);
    assert!(hir.contains("\"app::Helper\""), "{hir}");
    assert!(!hir.contains("app::app::Helper"), "{hir}");
    assert!(hir.contains("name: \"Program\""), "{hir}");
    assert!(!hir.contains("name: \"app::Program\""), "{hir}");
}
