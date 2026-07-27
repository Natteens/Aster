use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const MANIFEST: &str = "[application]\nentry = \"app.Program.Main\"\n";
const MAIN_SOURCE: &str = r#"namespace app;

using aster.io;

public class Program
{
    public static int Main()
    {
        WriteLine("Hello from ASTER!");
        return 0;
    }
}
"#;
const MAX_PROJECT_NAME_CHARS: usize = 64;
const RESERVED_WINDOWS_NAMES: [&str; 23] = [
    "CON", "PRN", "AUX", "NUL", "CLOCK$", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
    "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) fn create(parent: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    validate_parent(parent)?;

    let destination = parent.join(name);
    if destination == parent || destination.parent() != Some(parent) {
        return Err("project destination must be a direct child of the current directory".into());
    }
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            return Err(if metadata.is_dir() {
                if fs::read_dir(&destination)
                    .map_err(|error| format!("could not inspect project destination: {error}"))?
                    .next()
                    .is_some()
                {
                    "Cannot create project: destination directory is not empty.".into()
                } else {
                    "Cannot create project: destination directory already exists.".into()
                }
            } else {
                "Cannot create project: destination already exists and is not a directory.".into()
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect project destination before creation: {error}"
            ));
        }
    }

    let staging = create_staging_directory(parent, name)?;
    let mut guard = StagingGuard::new(staging.clone());
    (|| {
        let app = staging.join("app");
        fs::create_dir(&app)
            .map_err(|error| format!("could not create project source directory: {error}"))?;
        write_new_file(&staging.join("Aster.toml"), MANIFEST)?;
        write_new_file(&app.join("main.aster"), MAIN_SOURCE)?;
        validate_staging(&staging)?;

        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                return Err("Cannot create project: destination appeared during creation.".into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect project destination before publication: {error}"
                ));
            }
        }
        fs::rename(&staging, &destination)
            .map_err(|error| format!("could not publish the new project: {error}"))?;
        guard.disarm();
        Ok(destination)
    })()
}

pub(crate) fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("project name cannot be empty".into());
    }
    if name == "." || name == ".." {
        return Err("project name cannot be `.` or `..`".into());
    }
    if name.chars().count() > MAX_PROJECT_NAME_CHARS {
        return Err(format!(
            "project name is too long (maximum {MAX_PROJECT_NAME_CHARS} characters)"
        ));
    }
    if name.starts_with('.') || name.trim() != name {
        return Err("project name cannot be hidden or start/end with whitespace".into());
    }
    if name.starts_with("\\\\")
        || name.starts_with("//")
        || (name.as_bytes().get(1) == Some(&b':') && name.as_bytes()[0].is_ascii_alphabetic())
        || Path::new(name).is_absolute()
    {
        return Err("project name must not be an absolute path".into());
    }
    if name
        .chars()
        .any(|character| character.is_control() || "<>:\"/\\|?*".contains(character))
    {
        return Err("project name contains an invalid path character".into());
    }
    if name.ends_with('.') {
        return Err("project name cannot end with a period".into());
    }
    let portable_base = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if RESERVED_WINDOWS_NAMES.contains(&portable_base.as_str()) {
        return Err(format!(
            "project name `{name}` is reserved by the operating system"
        ));
    }
    Ok(())
}

fn validate_parent(parent: &Path) -> Result<(), String> {
    if parent.as_os_str().is_empty() || parent.parent().is_none() {
        return Err("the project parent directory cannot be a filesystem root".into());
    }
    let metadata = fs::symlink_metadata(parent)
        .map_err(|error| format!("could not access current directory: {error}"))?;
    if !metadata.is_dir() {
        return Err("the project parent is not a directory".into());
    }
    if is_link_or_reparse(&metadata) {
        return Err("the project parent cannot be a symlink or reparse point".into());
    }
    Ok(())
}

fn create_staging_directory(parent: &Path, name: &str) -> Result<PathBuf, String> {
    for _ in 0..32 {
        let counter = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let candidate = parent.join(format!(
            ".{name}.aster-new-{}-{nanos:x}-{counter:x}",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!(
                    "could not create project staging directory: {error}"
                ));
            }
        }
    }
    Err("could not create a unique project staging directory".into())
}

fn write_new_file(path: &Path, contents: &str) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("could not create `{}`: {error}", path.display()))?;
    file.write_all(contents.as_bytes())
        .map_err(|error| format!("could not write `{}`: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("could not finish `{}`: {error}", path.display()))
}

fn validate_staging(staging: &Path) -> Result<(), String> {
    let manifest = fs::read_to_string(staging.join("Aster.toml"))
        .map_err(|error| format!("could not validate generated manifest: {error}"))?;
    let source = fs::read_to_string(staging.join("app/main.aster"))
        .map_err(|error| format!("could not validate generated source: {error}"))?;
    if manifest != MANIFEST || source != MAIN_SOURCE {
        return Err("generated project content did not pass validation".into());
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

struct StagingGuard {
    path: PathBuf,
    active: bool,
}

impl StagingGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for StagingGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{MAIN_SOURCE, MANIFEST, create, validate_name};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn names_are_portable_and_unicode_directory_names_are_supported() {
        for valid in ["HelloAster", "hello-aster", "Projeto Áster", "项目"] {
            assert!(validate_name(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            ".",
            "..",
            ".hidden",
            " leading",
            "trailing ",
            "C:\\Aster",
            "\\\\server\\share",
            "../escape",
            "a/b",
            "a\\b",
            "CON",
            "nul.txt",
            "bad:name",
            "bad\u{7f}name",
        ] {
            assert!(validate_name(invalid).is_err(), "{invalid:?}");
        }
        assert!(validate_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn creation_is_deterministic_and_never_overwrites_an_existing_destination() {
        let first_parent = temporary_directory("deterministic one");
        let second_parent = temporary_directory("deterministic dois ü");
        let first = create(&first_parent, "HelloAster").expect("create first project");
        let second = create(&second_parent, "HelloAster").expect("create second project");

        assert_eq!(
            fs::read(first.join("Aster.toml")).expect("read first manifest"),
            fs::read(second.join("Aster.toml")).expect("read second manifest")
        );
        assert_eq!(
            fs::read(first.join("app/main.aster")).expect("read first source"),
            fs::read(second.join("app/main.aster")).expect("read second source")
        );
        assert_eq!(
            fs::read_to_string(first.join("Aster.toml")).expect("read manifest"),
            MANIFEST
        );
        assert_eq!(
            fs::read_to_string(first.join("app/main.aster")).expect("read source"),
            MAIN_SOURCE
        );

        let before = snapshot(&first);
        assert!(create(&first_parent, "HelloAster").is_err());
        assert_eq!(snapshot(&first), before);

        fs::remove_dir_all(first_parent).expect("remove first parent");
        fs::remove_dir_all(second_parent).expect("remove second parent");
    }

    #[test]
    fn existing_empty_directory_file_and_hidden_content_are_preserved() {
        let parent = temporary_directory("existing");
        let empty = parent.join("Empty");
        fs::create_dir(&empty).expect("create empty destination");
        assert!(create(&parent, "Empty").is_err());
        assert!(empty.is_dir());

        let file = parent.join("File");
        fs::write(&file, "owned").expect("create destination file");
        assert!(create(&parent, "File").is_err());
        assert_eq!(
            fs::read_to_string(&file).expect("read destination"),
            "owned"
        );

        let non_empty = parent.join("Owned");
        fs::create_dir(&non_empty).expect("create destination");
        fs::write(non_empty.join(".git"), "owned").expect("create hidden content");
        assert!(create(&parent, "Owned").is_err());
        assert_eq!(
            fs::read_to_string(non_empty.join(".git")).expect("read hidden content"),
            "owned"
        );
        assert!(fs::read_dir(&parent).expect("read parent").all(|entry| {
            !entry
                .expect("read entry")
                .file_name()
                .to_string_lossy()
                .contains(".aster-new-")
        }));
        fs::remove_dir_all(parent).expect("remove parent");
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aster-new-unit-{label}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        path
    }

    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = vec![
            (
                PathBuf::from("Aster.toml"),
                fs::read(root.join("Aster.toml")).expect("read manifest"),
            ),
            (
                PathBuf::from("app/main.aster"),
                fs::read(root.join("app/main.aster")).expect("read source"),
            ),
        ];
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    }
}
