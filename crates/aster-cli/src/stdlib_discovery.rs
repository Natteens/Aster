//! Standard library discovery for the `aster` binary.
//!
//! Priority chain (first match wins):
//!
//! 1. `ASTER_STDLIB` environment variable — explicit user override. If set and
//!    the path is invalid the process exits with an error; the env var is
//!    never silently ignored.
//! 2. `<exe-dir>/../stdlib/` relative to the running executable — installed
//!    layout (`bin/aster.exe` + `stdlib/aster/`). Used when the directory is
//!    present and passes validation.
//! 3. Embedded sources compiled into the binary — always available, used as
//!    the development fallback when neither of the above applies.

use std::{env, path::PathBuf};

use aster_compiler::StandardLibrary;

/// Environment variable that overrides stdlib discovery.
pub const ASTER_STDLIB_ENV: &str = "ASTER_STDLIB";

/// Discover the standard library using the priority chain documented in the
/// module-level comment. Prints a diagnostic and returns `Err(())` only when
/// `ASTER_STDLIB` is set but the path fails validation, or when a candidate
/// directory is found via exe-relative lookup but is invalid (incomplete install).
pub fn discover() -> Result<StandardLibrary, ()> {
    // 1. ASTER_STDLIB env var — explicit, must succeed or fail loudly.
    if let Some(env_path) = env::var_os(ASTER_STDLIB_ENV) {
        let path = PathBuf::from(&env_path);
        return StandardLibrary::from_path(&path).map_err(|error| {
            eprintln!(
                "error: {ASTER_STDLIB_ENV} is set but the stdlib is invalid:\n  {error}\n\nSet {ASTER_STDLIB_ENV} to a valid stdlib directory or unset it to use the embedded fallback."
            );
        });
    }

    // 2. stdlib relative to the running executable: <exe>/../stdlib/
    if let Some(candidate) = exe_relative_stdlib() {
        if candidate.is_dir() {
            match StandardLibrary::from_path(&candidate) {
                Ok(stdlib) => return Ok(stdlib),
                Err(error) => {
                    // A directory was found but it is incomplete — this is a
                    // broken installation. Error immediately rather than
                    // silently falling back, so the user knows to fix it.
                    eprintln!(
                        "error: an stdlib directory was found at `{}` but is incomplete:\n  {error}\n\nChecked:\n  - {}\n  - {ASTER_STDLIB_ENV} was not set\n\nReinstall ASTER or set {ASTER_STDLIB_ENV} to a valid stdlib directory.",
                        candidate.display(),
                        candidate.display(),
                    );
                    return Err(());
                }
            }
        }
    }

    // 3. Embedded dev fallback — always succeeds.
    Ok(StandardLibrary::embedded())
}

/// Returns the exe-relative candidate path, or `None` if the exe path cannot
/// be determined. The layout is `<exe-dir>/../stdlib/`.
pub fn exe_relative_stdlib() -> Option<PathBuf> {
    let exe = env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let install_root = exe_dir.parent()?;
    Some(install_root.join("stdlib"))
}

/// Validate a stdlib root directory without constructing a full
/// [`StandardLibrary`]. Returns a human-readable error on failure.
#[cfg(test)]
pub(crate) fn validate_stdlib_path(path: &std::path::Path) -> Result<(), String> {
    StandardLibrary::from_path(path).map(|_| ())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::validate_stdlib_path;

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aster-stdlib-{label}-{}-{id}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// Build a minimal but valid stdlib at `root`.
    fn write_valid_stdlib(root: &std::path::Path) {
        let modules = [
            "aster/math.aster",
            "aster/text/text.aster",
            "aster/core/core.aster",
            "aster/io/io.aster",
            "aster/collections/collections.aster",
        ];
        for relative in modules {
            let full = root.join(relative);
            fs::create_dir_all(full.parent().expect("parent")).expect("create dir");
            fs::write(&full, "// placeholder").expect("write stdlib file");
        }
    }

    #[test]
    fn valid_stdlib_path_passes_validation() {
        let root = temp_dir("valid");
        write_valid_stdlib(&root);
        let result = validate_stdlib_path(&root);
        fs::remove_dir_all(&root).ok();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn nonexistent_path_fails_validation() {
        let path = temp_dir("nonexistent");
        fs::remove_dir_all(&path).ok(); // make sure it doesn't exist
        let result = validate_stdlib_path(&path);
        assert!(result.is_err(), "expected error for missing path");
        let msg = result.unwrap_err();
        assert!(msg.contains("not accessible"), "unexpected message: {msg}");
    }

    #[test]
    fn file_instead_of_directory_fails_validation() {
        let dir = temp_dir("file-path");
        let file = dir.join("stdlib");
        fs::write(&file, "not a directory").expect("write file");
        let result = validate_stdlib_path(&file);
        fs::remove_dir_all(&dir).ok();
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(
            msg.contains("file, not a directory"),
            "unexpected message: {msg}"
        );
    }

    #[test]
    fn empty_directory_fails_validation() {
        let root = temp_dir("empty");
        let result = validate_stdlib_path(&root);
        fs::remove_dir_all(&root).ok();
        assert!(result.is_err(), "empty directory must fail validation");
    }

    #[test]
    fn incomplete_stdlib_fails_validation() {
        let root = temp_dir("incomplete");
        // Only write math — missing core, io, text, collections.
        let math = root.join("aster/math.aster");
        fs::create_dir_all(math.parent().expect("parent")).expect("mkdir");
        fs::write(&math, "// math").expect("write");
        let result = validate_stdlib_path(&root);
        fs::remove_dir_all(&root).ok();
        assert!(result.is_err(), "incomplete stdlib must fail");
    }

    #[test]
    fn path_with_spaces_passes_validation() {
        let root = temp_dir("path with spaces");
        write_valid_stdlib(&root);
        let result = validate_stdlib_path(&root);
        fs::remove_dir_all(&root).ok();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn unicode_path_passes_validation() {
        let root = temp_dir("stdlib-üñïcödé");
        write_valid_stdlib(&root);
        let result = validate_stdlib_path(&root);
        fs::remove_dir_all(&root).ok();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn embedded_fallback_is_always_valid() {
        use aster_compiler::StandardLibrary;
        // The embedded stdlib is always present; from_path is not needed for it.
        // This just asserts the embedded constructor succeeds.
        let _stdlib = StandardLibrary::embedded();
    }
}
