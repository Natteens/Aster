use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use aster_compiler::{ProjectSourceOrigin, compile_project};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aster-namespace-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create project");
        Self { root }
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create source directory");
        }
        fs::write(&path, source).expect("write source");
        path
    }

    fn main(&self, source: &str) -> PathBuf {
        self.write("main.aster", source)
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove project");
    }
}

fn messages(path: &Path) -> Vec<String> {
    compile_project(path)
        .expect_err("project should be rejected")
        .into_iter()
        .map(|error| error.diagnostic.message)
        .collect()
}

#[test]
fn supports_global_inferred_and_explicit_namespaces() {
    let global = Project::new("global");
    let root = global.main("public int Run() { return 42; }");
    let compilation = compile_project(&root).expect("global namespace");
    assert_eq!(compilation.root_namespace, "");

    let inferred = Project::new("inferred");
    inferred.write("app/value.aster", "public int Value() { return 42; }");
    let root = inferred.main("using app; public int Run() { return Value(); }");
    let compilation = compile_project(&root).expect("inferred app namespace");
    assert!(
        compilation
            .sources
            .iter()
            .any(|source| source.path.ends_with("app/value.aster"))
    );

    let explicit = Project::new("explicit");
    explicit.write(
        "app/value.aster",
        "namespace app; public int Value() { return 42; }",
    );
    let root = explicit.main("using app; public int Run() { return Value(); }");
    compile_project(&root).expect("matching explicit namespace");
}

#[test]
fn rejects_namespace_that_disagrees_with_its_directory() {
    let project = Project::new("mismatch");
    project.write(
        "app/value.aster",
        "namespace wrong; public int Value() { return 1; }",
    );
    let root = project.main("using app; public int Run() { return 0; }");
    let diagnostics = messages(&root);
    assert!(diagnostics.iter().any(|message| {
        message.contains("namespace `wrong`") && message.contains("directory namespace `app`")
    }));
}

#[test]
fn using_loads_all_direct_files_in_stable_order() {
    let project = Project::new("multi-file");
    project.write(
        "app/zeta.aster",
        "namespace app; public int Zeta() { return 20; }",
    );
    project.write(
        "app/alpha.aster",
        "namespace app; public int Alpha() { return 22; }",
    );
    let root = project.main("using app; public int Run() { return Alpha() + Zeta(); }");
    let compilation = compile_project(&root).expect("multi-file namespace");
    let project_sources = compilation
        .sources
        .iter()
        .filter(|source| source.origin == ProjectSourceOrigin::Project)
        .filter_map(|source| source.path.file_name()?.to_str())
        .collect::<Vec<_>>();
    assert_eq!(
        project_sources,
        vec!["main.aster", "alpha.aster", "zeta.aster"]
    );
}

#[test]
fn diagnostics_keep_their_file_local_span_without_padded_sources() {
    let project = Project::new("diagnostic-offset");
    project.write(
        "app/alpha.aster",
        "namespace app; public int Alpha() { return 1; }",
    );
    project.write(
        "app/zeta.aster",
        "namespace app; public int Broken() { return @; }",
    );
    let root = project.main("using app; public int Run() { return Alpha(); }");

    let diagnostics = compile_project(&root).expect_err("invalid second namespace file");
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.path.ends_with("app/zeta.aster"))
        .expect("diagnostic belongs to the invalid file");
    assert_eq!(
        diagnostic.source,
        "namespace app; public int Broken() { return @; }"
    );
    assert_eq!(
        &diagnostic.source[diagnostic.diagnostic.span.start..diagnostic.diagnostic.span.end],
        "@"
    );
}

#[test]
fn distinguishes_root_source_decode_from_namespace_source_decode() {
    let root_project = Project::new("root-invalid-utf8");
    let root = root_project.root.join("main.aster");
    fs::write(&root, [0xff, 0xfe, 0xfd]).expect("write invalid root source");
    let root_diagnostic = compile_project(&root)
        .expect_err("invalid root source must fail")
        .into_iter()
        .next()
        .expect("root diagnostic");
    assert_eq!(
        root_diagnostic.path,
        fs::canonicalize(&root).expect("canonical root source path")
    );
    assert!(
        root_diagnostic
            .diagnostic
            .message
            .contains("could not load root source file")
    );
    assert_eq!(
        root_diagnostic.diagnostic.help.as_deref(),
        Some("ensure the input source file is valid UTF-8")
    );

    let namespace_project = Project::new("namespace-invalid-utf8");
    let namespace = namespace_project.write("app/bad.aster", "");
    fs::write(&namespace, [0xff, 0xfe, 0xfd]).expect("write invalid namespace source");
    let root = namespace_project.main("using app; public int Run() { return 0; }");
    let namespace_diagnostic = compile_project(&root)
        .expect_err("invalid namespace source must fail")
        .into_iter()
        .next()
        .expect("namespace diagnostic");
    assert_eq!(
        namespace_diagnostic.path,
        fs::canonicalize(&namespace).expect("canonical namespace source path")
    );
    assert!(
        namespace_diagnostic
            .diagnostic
            .message
            .contains("could not load namespace file")
    );
    assert_eq!(
        namespace_diagnostic.diagnostic.help.as_deref(),
        Some("create the namespace directory/file or fix the using")
    );
}

#[test]
fn an_empty_namespace_file_links_without_creating_a_symbol_table_clone() {
    let project = Project::new("empty-namespace");
    project.write("app/empty.aster", "namespace app;");
    let root = project.main("using app; public int Run() { return 42; }");

    compile_project(&root).expect("empty namespace links without panicking");
}

#[test]
fn resolves_transitive_usings_and_project_internal_symbols() {
    let project = Project::new("transitive-internal");
    project.write(
        "app/value.aster",
        "namespace app; internal int Value() { return 42; }",
    );
    project.write(
        "ui/menu.aster",
        "namespace ui; using app; internal int MenuValue() { return Value(); }",
    );
    let root = project.main("using ui; public int Run() { return MenuValue(); }");
    compile_project(&root).expect("internal is project-wide across namespaces");
}

#[test]
fn links_nested_generic_declarations_and_references_across_files() {
    let project = Project::new("shared-traversal-generics");
    project.write(
        "app/types.aster",
        "namespace app; public interface IValue<T> { T Get(); } public class Box<T> : IValue<T> { private T value; public Box(T value) { this.value = value; } public T Value { get { return value; } private set { value = value; } } public T Get() { return value; } } public struct Pair<T, U> { public T first; public U second; } public enum Choice<T> { Some(T value), None, }",
    );
    let root = project.main(
        "using app; public int Read(Choice<Box<int>> choice) { switch (choice) { case Some(box): return box.Value; case None: return 0; } } public int Run() { Pair<Box<int>, Choice<Box<int>>> pair = Pair<Box<int>, Choice<Box<int>>> { first: new Box<int>(1), second: Choice<Box<int>>.Some(new Box<int>(41)) }; return pair.first.Get() + Read(pair.second); }",
    );
    let compilation = compile_project(&root).expect("linked nested generic declarations");
    let hir = format!("{:#?}", compilation.compilation.hir);
    assert!(hir.contains("app::Box<int>"));
    assert!(hir.contains("app::Choice<app::Box<int>>"));
    assert!(hir.contains("app::Pair<app::Box<int>,app::Choice<app::Box<int>>>"));
}

/// A `where` constraint is an ordinary `TypeRef`, so the existing linker
/// rewrites it to the linked nominal name with no second resolver.
#[test]
fn links_generic_constraints_declared_in_another_namespace() {
    let project = Project::new("constraint-linking");
    project.write(
        "contracts/scored.aster",
        "namespace contracts; public interface IScored { int Score(); }",
    );
    project.write(
        "lib/helpers.aster",
        "namespace lib; using contracts; public int Total<T>(T value) where T : IScored { return value.Score(); }",
    );
    let root = project.main(
        "using contracts; using lib; public class Card : IScored { private int points; public Card(int points) { this.points = points; } public int Score() { return points; } } public int Run() { return Total(new Card(42)); }",
    );
    let compilation = compile_project(&root).expect("cross-namespace constraint");
    let hir = format!("{:#?}", compilation.compilation.hir);
    assert!(hir.contains("lib::Total#"));

    let rejected = Project::new("constraint-linking-rejected");
    rejected.write(
        "contracts/scored.aster",
        "namespace contracts; public interface IScored { int Score(); }",
    );
    rejected.write(
        "lib/helpers.aster",
        "namespace lib; using contracts; public T Keep<T>(T value) where T : IScored { return value; }",
    );
    let root = rejected.main(
        "using contracts; using lib; public class Plain { public Plain() {} } public int Run() { Keep(new Plain()); return 0; }",
    );
    // The diagnostic names the linked identity, proving the constraint was
    // qualified rather than compared as bare source text.
    assert!(messages(&root).iter().any(|message| message
        == "type argument `Plain` does not satisfy constraint `T: contracts::IScored`"));
}

#[test]
fn reports_ambiguous_and_missing_namespaces() {
    let ambiguous = Project::new("ambiguous");
    ambiguous.write(
        "one/value.aster",
        "namespace one; public int Value() { return 1; }",
    );
    ambiguous.write(
        "two/value.aster",
        "namespace two; public int Value() { return 2; }",
    );
    let root = ambiguous.main("using one; using two; public int Run() { return Value(); }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| message.contains("ambiguous"))
    );

    let missing = Project::new("missing");
    let root = missing.main("using app.missing; public int Run() { return 0; }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| { message.contains("namespace `app.missing` was not found") })
    );
}

#[test]
fn rejects_using_cycles_and_duplicate_usings() {
    let cycle = Project::new("cycle");
    cycle.write(
        "one/a.aster",
        "namespace one; using two; public int One() { return 1; }",
    );
    cycle.write(
        "two/b.aster",
        "namespace two; using one; public int Two() { return 2; }",
    );
    let root = cycle.main("using one; public int Run() { return One(); }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| message.contains("circular using"))
    );

    let duplicate = Project::new("duplicate");
    duplicate.write(
        "app/value.aster",
        "namespace app; public int Value() { return 1; }",
    );
    let root = duplicate.main("using app; using app; public int Run() { return Value(); }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| message.contains("duplicate using"))
    );
}

#[test]
fn resolves_official_standard_library_without_watching_it() {
    let project = Project::new("stdlib");
    let root =
        project.main("using aster.math; public int Run() { return Math.Clamp(150, 0, 100); }");
    let compilation = compile_project(&root).expect("official namespace");
    assert!(
        compilation
            .sources
            .iter()
            .any(|source| { source.origin == ProjectSourceOrigin::StandardLibrary })
    );
    assert_eq!(
        compilation.dependency_paths(),
        vec![root.canonicalize().unwrap()]
    );
}

#[test]
fn project_cannot_declare_or_shadow_aster_namespaces() {
    let project = Project::new("reserved");
    project.write(
        "Aster.toml",
        "[package]\nname = \"reserved\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    let reserved = project.write(
        "aster/math/replacement.aster",
        "namespace aster.math; public class Replacement {}",
    );
    assert!(
        messages(&reserved)
            .iter()
            .any(|message| message.contains("reserved"))
    );

    let shadow = Project::new("shadow");
    let root = shadow.main("using aster.math; public class Math {} public int Run() { return 0; }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| message.contains("official export"))
    );

    let text = Project::new("reserved-text");
    text.write(
        "Aster.toml",
        "[package]\nname = \"reserved_text\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    let replacement = text.write(
        "aster/text/text.aster",
        "namespace aster.text; public static class String {}",
    );
    assert!(
        messages(&replacement)
            .iter()
            .any(|message| message.contains("reserved"))
    );
}

#[test]
fn missing_official_namespace_reports_incomplete_distribution() {
    let project = Project::new("missing-official");
    let root = project.main("using aster.missing; public int Run() { return 0; }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| { message.contains("installation is incomplete") })
    );
}

#[test]
fn closed_generic_constraints_preserve_cross_namespace_nominal_identity() {
    let project = Project::new("constraint-identity");
    project.write("Aster.toml", "[package]\nname = \"constraint_identity\"\n");
    project.write(
        "left/contracts.aster",
        "namespace left; public interface IBox<T> { T Get(); } public class LeftBox : IBox<int> { public LeftBox() {} public int Get() { return 42; } }",
    );
    project.write(
        "right/contracts.aster",
        "namespace right; public interface IBox<T> { T Get(); } public T Keep<T>(T value) where T : IBox<int> { return value; }",
    );
    let root = project.write(
        "left/main.aster",
        "namespace left; using right; public int Run() { Keep(new LeftBox()); return 0; }",
    );
    let diagnostics = messages(&root);
    assert!(
        diagnostics.iter().any(|message| {
            message.contains("left::LeftBox` does not satisfy constraint")
                && message.contains("right::IBox<int>")
        }),
        "unexpected diagnostics: {diagnostics:?}"
    );
}

#[test]
fn decimal_in_a_linked_namespace_is_rejected_by_the_shared_pre_ir_gate() {
    let project = Project::new("linked-decimal");
    project.write(
        "money/value.aster",
        "namespace money; public decimal Amount() { return 1.25m; }",
    );
    let root = project.main("using money; public int Run() { return 0; }");
    assert!(
        messages(&root)
            .iter()
            .any(|message| message.contains("`decimal` is reserved but not supported"))
    );
}

#[test]
fn generic_method_identity_is_distinct_across_linked_namespaces() {
    let project = Project::new("method-identity");
    project.write("Aster.toml", "[package]\nname = \"method_identity\"\n");
    project.write(
        "left/tools.aster",
        "namespace left; public class Tools { public Tools() {} public T Identity<T>(T value) { return value; } } public int LeftValue() { return new Tools().Identity<int>(20); }",
    );
    project.write(
        "right/tools.aster",
        "namespace right; public class Tools { public Tools() {} public T Identity<T>(T value) { return value; } } public int RightValue() { return new Tools().Identity<int>(22); }",
    );
    let root = project.write(
        "left/main.aster",
        "namespace left; using right; public int Run() { return LeftValue() + RightValue(); }",
    );
    let compilation = compile_project(&root).expect("linked generic methods compile");
    let methods = compilation
        .compilation
        .hir
        .items
        .iter()
        .filter_map(|item| match item {
            aster_compiler::hir::Item::Class(class) => Some(
                class
                    .methods
                    .iter()
                    .filter(|method| method.name.contains("#method#Identity#"))
                    .map(move |method| (class.name.clone(), method.name.clone())),
            ),
            _ => None,
        })
        .flatten()
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 2, "{methods:#?}");
    assert_eq!(
        methods
            .iter()
            .map(|(_, method)| method)
            .collect::<HashSet<_>>()
            .len(),
        2,
        "{methods:#?}"
    );
    assert!(
        methods
            .iter()
            .any(|(owner, _)| owner.contains("left::Tools"))
    );
    assert!(
        methods
            .iter()
            .any(|(owner, _)| owner.contains("right::Tools"))
    );
}
