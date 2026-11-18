//! ARCH-005 boundary: the scheduler crate must not depend on any code
//! generator. `cargo tree -p aster-exec | grep -i cranelift` must print nothing;
//! this test pins the same rule at the manifest level.

#[test]
fn the_manifest_names_no_cranelift_crate() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("manifest is readable");
    let dependencies = manifest
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| line.to_ascii_lowercase().contains("cranelift"))
        .collect::<Vec<_>>();
    assert!(
        dependencies.is_empty(),
        "aster-exec must stay free of code generators: {dependencies:?}"
    );
}

#[test]
fn the_manifest_does_not_depend_on_the_jit_backend() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("manifest is readable");
    assert!(
        !manifest
            .lines()
            .filter(|line| !line.trim_start().starts_with('#'))
            .any(|line| line.trim_start().starts_with("aster-codegen")),
        "aster-exec must not depend on a backend crate"
    );
}
