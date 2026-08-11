use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use aster_compiler::{compile_project, select_application_entry};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

struct Project {
    root: PathBuf,
}

impl Project {
    fn new(label: &str) -> Self {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "aster-application-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create test project");
        Self { root }
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create test source directory");
        }
        fs::write(&path, source).expect("write test source");
        path
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove test project");
    }
}

fn select(root: &Path) -> Result<aster_compiler::ApplicationEntry, Vec<String>> {
    let project = compile_project(root).expect("source should compile before entry selection");
    select_application_entry(&project, root)
        .map_err(|diagnostics| diagnostics.into_iter().map(|value| value.message).collect())
}

fn errors(root: &Path) -> Vec<String> {
    select(root).expect_err("entry selection should fail")
}

fn manifest_case(label: &str, manifest: &str) -> (Project, PathBuf) {
    let project = Project::new(label);
    project.write("Aster.toml", manifest);
    let root = project.write(
        "app/main.aster",
        "namespace app; public class Program { public static int Main() { return 42; } }",
    );
    (project, root)
}

#[test]
fn selects_conventional_void_and_int_main_methods() {
    for (label, return_type, body) in [("void", "void", ""), ("int", "int", "return 42;")] {
        let project = Project::new(label);
        let root = project.write(
            "main.aster",
            &format!("public class Program {{ public static {return_type} Main() {{ {body} }} }}"),
        );
        let entry = select(&root).expect("valid conventional entry");
        assert_eq!(entry.display_name, "Program.Main");
    }
}

#[test]
fn reports_missing_and_ambiguous_conventional_entries() {
    let missing = Project::new("missing");
    let root = missing.write("main.aster", "public int Utility() { return 1; }");
    assert!(errors(&root)[0].contains("no application entry"));

    let ambiguous = Project::new("ambiguous");
    let root = ambiguous.write(
        "main.aster",
        "public class First { public static void Main() {} } public class Second { public static int Main() { return 0; } }",
    );
    assert!(errors(&root)[0].contains("more than one conventional"));
}

#[test]
fn explains_every_invalid_main_shape() {
    let cases = [
        (
            "private",
            "public class Program { private static void Main() {} }",
            "not public",
        ),
        (
            "instance",
            "public class Program { public void Main() {} }",
            "not static",
        ),
        (
            "parameters",
            "public class Program { public static void Main(int value) {} }",
            "has parameters",
        ),
        (
            "return",
            "public class Program { public static bool Main() { return true; } }",
            "not `void` or `int`",
        ),
    ];
    for (label, source, expected) in cases {
        let project = Project::new(label);
        let root = project.write("main.aster", source);
        assert!(
            errors(&root)
                .iter()
                .any(|message| message.contains(expected)),
            "missing `{expected}` diagnostic"
        );
    }
}

#[test]
fn manifest_selects_a_resolved_root_method_and_sets_project_root() {
    let project = Project::new("manifest");
    project.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    project.write(
        "app/math.aster",
        "namespace app; public int Answer() { return 42; }",
    );
    let root = project.write(
        "app/main.aster",
        "namespace app; public class Program { public static int Main() { return Answer(); } }",
    );
    let compilation =
        compile_project(&root).expect("manifest root resolves sibling namespace file");
    assert_eq!(compilation.sources.len(), 2);
    let entry = select_application_entry(&compilation, &root).expect("manifest entry is valid");
    assert_eq!(entry.display_name, "app.Program.Main");
}

/// The entry class does not have to live in the literal file passed to
/// `compile_project`. Every manifest-backed root uses the same
/// package-qualified identity.
#[test]
fn manifest_entry_resolves_from_a_sibling_file_of_the_root_package() {
    let project = Project::new("sibling-entry");
    project.write(
        "Aster.toml",
        "[package]\nname = \"greeter\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    // `Program` lives in a sibling file, never the literal root file.
    project.write(
        "app/program.aster",
        "namespace app; public class Program { public static int Main() { return 42; } }",
    );
    let root = project.write("app/main.aster", "namespace app;");
    let compilation =
        compile_project(&root).expect("manifest root resolves sibling namespace file");
    let entry =
        select_application_entry(&compilation, &root).expect("sibling entry class resolves");
    assert_eq!(entry.display_name, "app.Program.Main");
}

#[test]
fn manifest_entry_resolves_from_the_literal_root_file() {
    let project = Project::new("root-entry");
    project.write(
        "Aster.toml",
        "[package]\nname = \"greeter\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    let root = project.write(
        "app/main.aster",
        "namespace app; public class Program { public static int Main() { return 42; } }",
    );
    let compilation = compile_project(&root).expect("root entry compiles");
    let entry = select_application_entry(&compilation, &root).expect("root entry resolves");
    assert_eq!(entry.display_name, "app.Program.Main");
}

#[test]
fn removed_manifest_schema_fields_are_rejected() {
    for (label, manifest) in [
        ("schema-one", "schema = 1\n\n[package]\nname = \"app\"\n"),
        ("schema-two", "schema = 2\n\n[package]\nname = \"app\"\n"),
    ] {
        let (_project, root) = manifest_case(label, manifest);
        assert!(errors(&root).iter().any(|message| {
            message.contains("Aster.toml no longer uses a `schema` field; remove it")
        }));
    }
}

#[test]
fn manifest_requires_package_identity_and_rejects_unknown_fields() {
    let cases = [
        (
            "without-package",
            "[application]\nentry = \"app.Program.Main\"\n",
            "does not define a `[package]` table",
        ),
        (
            "unknown-top-level",
            "name = \"demo\"\n\n[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
            "unknown top-level Aster.toml field `name`",
        ),
        (
            "unknown-application",
            "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\nmode = \"debug\"\n",
            "unknown Aster.toml application field `mode`",
        ),
    ];
    for (label, manifest, expected) in cases {
        let (_project, root) = manifest_case(label, manifest);
        assert!(
            errors(&root)
                .iter()
                .any(|message| message.contains(expected)),
            "missing `{expected}` diagnostic"
        );
    }
}

#[test]
fn manifest_requires_a_typed_application_entry() {
    let cases = [
        (
            "missing-application",
            "[package]\nname = \"app\"\n",
            "does not define an `[application]` entry",
        ),
        (
            "wrong-application",
            "application = \"app.Program.Main\"\n\n[package]\nname = \"app\"\n",
            "`application` must be a table",
        ),
        (
            "missing-entry",
            "[package]\nname = \"app\"\n\n[application]\n",
            "does not define `application.entry`",
        ),
        (
            "wrong-entry-type",
            "[package]\nname = \"app\"\n\n[application]\nentry = 1\n",
            "`application.entry` must be a string",
        ),
        (
            "empty-entry",
            "[package]\nname = \"app\"\n\n[application]\nentry = \"\"\n",
            "invalid format",
        ),
        (
            "wrong-method",
            "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Run\"\n",
            "must end in `.Main`",
        ),
    ];
    for (label, manifest, expected) in cases {
        let (_project, root) = manifest_case(label, manifest);
        assert!(
            errors(&root)
                .iter()
                .any(|message| message.contains(expected)),
            "missing `{expected}` diagnostic"
        );
    }
}

#[test]
fn manifest_selects_a_void_main_entry() {
    let project = Project::new("manifest-void");
    project.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    let root = project.write(
        "app/main.aster",
        "namespace app; public class Program { public static void Main() {} }",
    );
    let entry = select(&root).expect("void manifest entry is valid");
    assert_eq!(entry.display_name, "app.Program.Main");
}

#[test]
fn manifest_reports_invalid_toml_and_entry_format() {
    let invalid_toml = Project::new("invalid-toml");
    invalid_toml.write("Aster.toml", "[application\nentry = 1");
    let root = invalid_toml.write(
        "main.aster",
        "public class Program { public static void Main() {} }",
    );
    assert!(errors(&root)[0].contains("invalid Aster.toml"));

    let invalid_entry = Project::new("invalid-entry");
    invalid_entry.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"Program.Run\"\n",
    );
    let root = invalid_entry.write(
        "main.aster",
        "public class Program { public static void Main() {} }",
    );
    assert!(errors(&root)[0].contains("invalid format"));
}

#[test]
fn manifest_reports_missing_inaccessible_and_invalid_targets() {
    let missing = Project::new("manifest-missing");
    missing.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Missing.Main\"\n",
    );
    let root = missing.write(
        "app/main.aster",
        "namespace app; public int Value() { return 1; }",
    );
    assert!(errors(&root)[0].contains("was not found"));

    let inaccessible = Project::new("manifest-internal");
    inaccessible.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    let root = inaccessible.write(
        "app/main.aster",
        "namespace app; internal class Program { public static void Main() {} }",
    );
    assert!(errors(&root)[0].contains("not public"));

    let imported_internal = Project::new("manifest-imported-internal");
    imported_internal.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.hidden.Program.Main\"\n",
    );
    imported_internal.write(
        "app/hidden/program.aster",
        "namespace app.hidden; internal class Program { public static void Main() {} }",
    );
    let root = imported_internal.write(
        "app/main.aster",
        "namespace app; using app.hidden; public int Utility() { return 1; }",
    );
    assert!(errors(&root)[0].contains("not public"));

    let invalid = Project::new("manifest-invalid-method");
    invalid.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    );
    let root = invalid.write(
        "app/main.aster",
        "namespace app; public class Program { public int Main() { return 1; } }",
    );
    assert!(errors(&root)[0].contains("not static"));
}

#[test]
fn manifest_cannot_select_an_official_standard_library_module() {
    let project = Project::new("manifest-stdlib");
    project.write(
        "Aster.toml",
        "[package]\nname = \"app\"\n\n[application]\nentry = \"aster.math.Math.Main\"\n",
    );
    let root = project.write(
        "app/main.aster",
        "namespace app; using aster.math; public int Utility() { return Math.Max(1, 2); }",
    );
    assert!(errors(&root)[0].contains("cannot point into standard library"));
}
