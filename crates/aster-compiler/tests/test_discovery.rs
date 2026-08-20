use std::{
    fmt::Write as _,
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

use aster_compiler::{compile, compile_project_for_tests};

fn fixture(name: &str) -> std::path::PathBuf {
    let id = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "aster-test-discovery-{name}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("Aster.toml"), "[package]\nname = \"sample\"\n").unwrap();
    fs::write(
        root.join("app/main.aster"),
        "namespace app; public int Value() { return 42; }",
    )
    .unwrap();
    root
}

#[test]
fn discovers_root_package_tests_in_stable_identity_order() {
    let root = fixture("order");
    fs::write(
        root.join("app/main.aster"),
        "namespace app; test void NotDiscovered() { } public int Value() { return 42; }",
    )
    .unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/z.aster"),
        "namespace tests; using aster.testing; test void Zebra() { Assert.True(true); }",
    )
    .unwrap();
    fs::write(
        root.join("tests/a.aster"),
        "namespace tests; using aster.testing; test void Alpha() { Assert.Equal(42, 40 + 2); }",
    )
    .unwrap();
    let project = compile_project_for_tests(&root.join("app/main.aster")).unwrap();
    let names = project
        .tests()
        .iter()
        .map(|test| test.display_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["sample.tests.Alpha", "sample.tests.Zebra"]);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_non_void_and_parameterized_tests() {
    let root = fixture("invalid");
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/bad.aster"),
        "namespace tests; test int Bad(int value) { return value; }",
    )
    .unwrap();
    let errors = compile_project_for_tests(&root.join("app/main.aster")).unwrap_err();
    let rendered = errors
        .iter()
        .map(aster_compiler::ProjectDiagnostic::render)
        .collect::<String>();
    assert!(rendered.contains("test functions cannot declare parameters"));
    assert!(rendered.contains("test functions must return `void`"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn rejects_public_tests_before_they_can_be_dependency_api() {
    let root = fixture("public-test");
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/public.aster"),
        "namespace tests; public test void EscapesPackage() { }",
    )
    .unwrap();

    let errors = compile_project_for_tests(&root.join("app/main.aster")).unwrap_err();
    let rendered = errors
        .iter()
        .map(aster_compiler::ProjectDiagnostic::render)
        .collect::<String>();
    assert!(rendered.contains("test functions cannot be public"));
    assert!(rendered.contains("package-owned runner metadata"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn test_remains_contextual_in_ordinary_identifier_positions() {
    compile(
        "public class Holder { public int test; public int Read() { return test; } } \
         public int test() { return 1; } \
         public int ReadLocal() { int test = 1; return test; }",
    )
    .expect("ordinary identifier uses of test compile without a test-only parser mode");
}

#[test]
fn root_tests_can_use_path_dependencies_without_discovering_dependency_tests() {
    let root = fixture("dependency");
    let dependency = root.join("math");
    fs::create_dir_all(dependency.join("math")).unwrap();
    fs::create_dir_all(dependency.join("tests")).unwrap();
    fs::write(
        root.join("Aster.toml"),
        "[package]\nname = \"sample\"\n\n[dependencies]\nmath = { path = \"math\" }\n",
    )
    .unwrap();
    fs::write(
        dependency.join("Aster.toml"),
        "[package]\nname = \"math\"\n",
    )
    .unwrap();
    fs::write(
        dependency.join("math/answer.aster"),
        "namespace math; public int Answer() { return 42; }",
    )
    .unwrap();
    // If dependency test directories were discovered as root input, this would
    // fail normal parsing instead of proving package-scoped discovery.
    fs::write(dependency.join("tests/not_root.aster"), "@").unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("tests/use_dependency.aster"),
        "namespace tests; using aster.testing; using math; test void UsesDependency() { Assert.Equal(42, Answer()); }",
    )
    .unwrap();

    let project = compile_project_for_tests(&root.join("app/main.aster")).unwrap();

    assert_eq!(
        project
            .tests()
            .iter()
            .map(|test| test.display_name.as_str())
            .collect::<Vec<_>>(),
        ["sample.tests.UsesDependency"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dependency_test_declarations_are_not_importable_api() {
    let root = fixture("dependency-test-api");
    let dependency = root.join("library");
    fs::create_dir_all(dependency.join("library")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("Aster.toml"),
        "[package]\nname = \"sample\"\n\n[dependencies]\nlibrary = { path = \"library\" }\n",
    )
    .unwrap();
    fs::write(
        dependency.join("Aster.toml"),
        "[package]\nname = \"library\"\n",
    )
    .unwrap();
    fs::write(
        dependency.join("library/hidden.aster"),
        "namespace library; test void Hidden() { }",
    )
    .unwrap();
    fs::write(
        root.join("tests/consumer.aster"),
        "namespace tests; using library; test void CannotCallDependencyTest() { Hidden(); }",
    )
    .unwrap();

    let errors = compile_project_for_tests(&root.join("app/main.aster")).unwrap_err();
    let rendered = errors
        .iter()
        .map(aster_compiler::ProjectDiagnostic::render)
        .collect::<String>();
    assert!(
        rendered.contains("not part of its public API"),
        "{rendered}"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn discovery_scales_without_order_or_descriptor_leaks() {
    let root = fixture("many");
    fs::create_dir_all(root.join("tests")).unwrap();
    let mut source = String::from("namespace tests; using aster.testing;");
    for index in (0..1_000).rev() {
        write!(
            source,
            "test void Case{index:04}() {{ Assert.True(true); }}"
        )
        .expect("writing to a String cannot fail");
    }
    fs::write(root.join("tests/many.aster"), source).unwrap();

    let project = compile_project_for_tests(&root.join("app/main.aster")).unwrap();

    assert_eq!(project.tests().len(), 1_000);
    assert_eq!(
        project.tests().first().unwrap().display_name,
        "sample.tests.Case0000"
    );
    assert_eq!(
        project.tests().last().unwrap().display_name,
        "sample.tests.Case0999"
    );
    fs::remove_dir_all(root).unwrap();
}
