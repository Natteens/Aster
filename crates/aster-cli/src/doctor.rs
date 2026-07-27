use std::{
    collections::HashMap,
    env, fs,
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    new_project,
    stdlib_discovery::{
        DiscoveredStandardLibrary, StandardLibraryDiscoveryError, StandardLibraryOrigin,
        discover_detailed,
    },
};
use aster_codegen_cranelift::ExecutionValue;
use aster_compiler::{ProjectCompilation, StandardLibrary};

const PROBE_SOURCE: &str = r"namespace app;

public class Program
{
    public static int Main()
    {
        return 42;
    }
}
";

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DoctorStatus {
    Ok,
    Info,
    Warning,
    Error,
}

impl DoctorStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Info => "INFO",
            Self::Warning => "WARN",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DoctorCheck {
    label: &'static str,
    status: DoctorStatus,
    detail: String,
}

impl DoctorCheck {
    fn new(label: &'static str, status: DoctorStatus, detail: impl Into<String>) -> Self {
        Self {
            label,
            status,
            detail: detail.into(),
        }
    }
}

pub(crate) fn run() -> bool {
    let checks = collect_checks();
    println!("ASTER Doctor\n");
    for check in &checks {
        println!(
            "[{}] {}: {}",
            check.status.label(),
            check.label,
            check.detail
        );
    }

    let has_errors = checks
        .iter()
        .any(|check| check.status == DoctorStatus::Error);
    let has_warnings = checks
        .iter()
        .any(|check| check.status == DoctorStatus::Warning);
    println!();
    if has_errors {
        println!("ASTER Doctor found problems.");
    } else if has_warnings {
        println!("ASTER Doctor completed with warnings.");
    } else {
        println!("No problems found.");
    }
    !has_errors
}

fn collect_checks() -> Vec<DoctorCheck> {
    let version = env!("CARGO_PKG_VERSION");
    let mut checks = Vec::with_capacity(9);
    checks.push(DoctorCheck::new("Version", DoctorStatus::Ok, version));

    let target = public_target();
    checks.push(match target {
        Some(target) => DoctorCheck::new("Platform", DoctorStatus::Ok, target),
        None => DoctorCheck::new(
            "Platform",
            DoctorStatus::Error,
            "Unsupported platform or architecture",
        ),
    });

    let executable = check_executable();
    checks.push(match &executable {
        Ok(path) => DoctorCheck::new("Executable", DoctorStatus::Ok, path.display().to_string()),
        Err(reason) => DoctorCheck::new("Executable", DoctorStatus::Error, reason),
    });

    let standard_library = discover_detailed();
    checks.push(standard_library_origin_check(&standard_library));
    checks.push(standard_library_validation_check(&standard_library));

    checks.push(match (&executable, target) {
        (Ok(executable), Some(target)) => check_managed_install(executable, version, target),
        _ => DoctorCheck::new(
            "Managed installation",
            DoctorStatus::Info,
            "not checked because executable or platform information is unavailable",
        ),
    });

    checks.push(match &executable {
        Ok(executable) => check_path(executable),
        Err(_) => DoctorCheck::new(
            "PATH",
            DoctorStatus::Warning,
            "not checked because the executable path is unavailable",
        ),
    });

    if let Ok(discovered) = &standard_library {
        checks.push(check_compilation_probe(&discovered.standard_library));
        checks.push(check_current_project(&discovered.standard_library));
    } else {
        checks.push(DoctorCheck::new(
            "Compilation probe",
            DoctorStatus::Error,
            "skipped because the standard library is unavailable",
        ));
        checks.push(DoctorCheck::new(
            "Current project",
            DoctorStatus::Error,
            "not checked because the standard library is unavailable",
        ));
    }
    checks
}

fn public_target() -> Option<&'static str> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("windows-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux-x64")
    } else {
        None
    }
}

fn check_executable() -> Result<PathBuf, String> {
    let executable = env::current_exe().map_err(|error| format!("path is unavailable: {error}"))?;
    let metadata = fs::metadata(&executable)
        .map_err(|error| format!("could not access `{}`: {error}", executable.display()))?;
    if !metadata.is_file() {
        return Err(format!("`{}` is not a file", executable.display()));
    }
    let expected_name = if cfg!(windows) { "aster.exe" } else { "aster" };
    if executable.file_name().and_then(|name| name.to_str()) != Some(expected_name) {
        return Err(format!(
            "unexpected executable name `{}`",
            executable.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    let parent = executable
        .parent()
        .ok_or_else(|| "executable has no parent directory".to_owned())?;
    fs::read_dir(parent)
        .map_err(|error| format!("executable directory is not accessible: {error}"))?;
    Ok(executable)
}

fn standard_library_origin_check(
    result: &Result<DiscoveredStandardLibrary, StandardLibraryDiscoveryError>,
) -> DoctorCheck {
    match result {
        Ok(discovered) => match &discovered.origin {
            StandardLibraryOrigin::Environment(path) => DoctorCheck::new(
                "Standard library origin",
                DoctorStatus::Ok,
                format!("Environment ({})", path.display()),
            ),
            StandardLibraryOrigin::ExecutableRelative(path) => DoctorCheck::new(
                "Standard library origin",
                DoctorStatus::Ok,
                format!("Executable-relative ({})", path.display()),
            ),
            StandardLibraryOrigin::Embedded => {
                DoctorCheck::new("Standard library origin", DoctorStatus::Ok, "Embedded")
            }
        },
        Err(StandardLibraryDiscoveryError::Environment { path, .. }) => DoctorCheck::new(
            "Standard library origin",
            DoctorStatus::Ok,
            format!("Environment ({})", path.display()),
        ),
        Err(StandardLibraryDiscoveryError::ExecutableRelative { path, .. }) => DoctorCheck::new(
            "Standard library origin",
            DoctorStatus::Ok,
            format!("Executable-relative ({})", path.display()),
        ),
    }
}

fn standard_library_validation_check(
    result: &Result<DiscoveredStandardLibrary, StandardLibraryDiscoveryError>,
) -> DoctorCheck {
    match result {
        Ok(_) => DoctorCheck::new(
            "Standard library structure",
            DoctorStatus::Ok,
            "required modules are readable",
        ),
        Err(StandardLibraryDiscoveryError::Environment { reason, .. }) => DoctorCheck::new(
            "Standard library structure",
            DoctorStatus::Error,
            format!(
                "ASTER_STDLIB points to an invalid standard library: {}",
                short(reason)
            ),
        ),
        Err(StandardLibraryDiscoveryError::ExecutableRelative { reason, .. }) => DoctorCheck::new(
            "Standard library structure",
            DoctorStatus::Error,
            format!(
                "The installed standard library is incomplete: {}",
                short(reason)
            ),
        ),
    }
}

fn check_managed_install(executable: &Path, version: &str, target: &str) -> DoctorCheck {
    let Some(bin_directory) = executable.parent() else {
        return managed_error("executable has no bin directory");
    };
    if bin_directory.file_name().and_then(|name| name.to_str()) != Some("bin") {
        return DoctorCheck::new(
            "Managed installation",
            DoctorStatus::Info,
            "marker was not found",
        );
    }
    let Some(root) = bin_directory.parent() else {
        return managed_error("installation root is unavailable");
    };
    let state_path = root.join("install-state.json");
    if !state_path.is_file() {
        return DoctorCheck::new(
            "Managed installation",
            DoctorStatus::Info,
            "marker was not found",
        );
    }
    match validate_managed_install(root, executable, version, target) {
        Ok(()) => DoctorCheck::new(
            "Managed installation",
            DoctorStatus::Ok,
            format!("{version} ({})", root.display()),
        ),
        Err(reason) => managed_error(reason),
    }
}

fn validate_managed_install(
    root: &Path,
    executable: &Path,
    version: &str,
    target: &str,
) -> Result<(), String> {
    let state = read_json(&root.join("install-state.json"), "install-state.json")?;
    let manifest = read_json(&root.join("install-manifest.json"), "install-manifest.json")?;
    validate_common_json(&state, version, target, "install-state.json")?;
    validate_common_json(&manifest, version, target, "install-manifest.json")?;
    let entrypoint = json_string(&manifest, "entrypoint", "install-manifest.json")?;
    let stdlib = json_string(&manifest, "stdlib", "install-manifest.json")?;
    let license = json_string(&manifest, "license", "install-manifest.json")?;
    let expected_entrypoint = if cfg!(windows) {
        "bin/aster.exe"
    } else {
        "bin/aster"
    };
    if entrypoint != expected_entrypoint || stdlib != "stdlib" || license != "LICENSE" {
        return Err("install-manifest.json contains incompatible paths".into());
    }
    if !same_path(&root.join(entrypoint), executable) {
        return Err("install-manifest.json entrypoint does not match this executable".into());
    }
    StandardLibrary::from_path(&root.join(stdlib))
        .map_err(|error| format!("managed standard library is invalid: {}", short(&error)))?;
    if !root.join(license).is_file() {
        return Err("managed installation is missing LICENSE".into());
    }
    Ok(())
}

fn read_json(path: &Path, label: &str) -> Result<HashMap<String, JsonValue>, String> {
    let source =
        fs::read_to_string(path).map_err(|error| format!("could not read {label}: {error}"))?;
    JsonParser::new(&source)
        .parse_object()
        .map_err(|error| format!("{label} is invalid JSON: {error}"))
}

fn validate_common_json(
    value: &HashMap<String, JsonValue>,
    version: &str,
    target: &str,
    label: &str,
) -> Result<(), String> {
    if value.get("schema").and_then(JsonValue::as_u64) != Some(1)
        || json_string(value, "product", label)? != "aster"
        || json_string(value, "target", label)? != target
    {
        return Err(format!(
            "{label} has incompatible product, schema, or target"
        ));
    }
    let reported_version = json_string(value, "version", label)?;
    if reported_version.is_empty() {
        return Err(format!("{label} has an empty version"));
    }
    if reported_version != version {
        return Err("Installed components report different versions".into());
    }
    Ok(())
}

fn json_string<'a>(
    value: &'a HashMap<String, JsonValue>,
    key: &str,
    label: &str,
) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(JsonValue::as_str)
        .ok_or_else(|| format!("{label} does not define string `{key}`"))
}

#[derive(Debug, PartialEq, Eq)]
enum JsonValue {
    String(String),
    Number(u64),
    Other,
}

impl JsonValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Number(_) | Self::Other => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::String(_) | Self::Other => None,
        }
    }
}

struct JsonParser {
    characters: Vec<char>,
    position: usize,
}

impl JsonParser {
    fn new(source: &str) -> Self {
        Self {
            characters: source.chars().collect(),
            position: 0,
        }
    }

    fn parse_object(mut self) -> Result<HashMap<String, JsonValue>, String> {
        self.whitespace();
        self.expect('{')?;
        let mut values = HashMap::new();
        loop {
            self.whitespace();
            if self.consume('}') {
                break;
            }
            let key = self.string()?;
            self.whitespace();
            self.expect(':')?;
            self.whitespace();
            let value = self.value()?;
            if values.insert(key, value).is_some() {
                return Err("duplicate object key".into());
            }
            self.whitespace();
            if self.consume('}') {
                break;
            }
            self.expect(',')?;
        }
        self.whitespace();
        if self.position != self.characters.len() {
            return Err("trailing content".into());
        }
        Ok(values)
    }

    fn value(&mut self) -> Result<JsonValue, String> {
        if self.peek() == Some('"') {
            return self.string().map(JsonValue::String);
        }
        if self
            .peek()
            .is_some_and(|character| character.is_ascii_digit())
        {
            let start = self.position;
            while self
                .peek()
                .is_some_and(|character| character.is_ascii_digit())
            {
                self.position += 1;
            }
            let number = self.characters[start..self.position]
                .iter()
                .collect::<String>()
                .parse()
                .map_err(|_| "invalid unsigned number")?;
            return Ok(JsonValue::Number(number));
        }
        for literal in ["true", "false", "null"] {
            if self.consume_text(literal) {
                return Ok(JsonValue::Other);
            }
        }
        Err("unsupported value".into())
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect('"')?;
        let mut result = String::new();
        loop {
            let character = self.next().ok_or("unterminated string")?;
            match character {
                '"' => return Ok(result),
                '\\' => {
                    let escaped = self.next().ok_or("unterminated escape")?;
                    result.push(match escaped {
                        '"' => '"',
                        '\\' => '\\',
                        '/' => '/',
                        'b' => '\u{0008}',
                        'f' => '\u{000c}',
                        'n' => '\n',
                        'r' => '\r',
                        't' => '\t',
                        _ => return Err("unsupported string escape".into()),
                    });
                }
                value if value.is_control() => {
                    return Err("control character in string".into());
                }
                value => result.push(value),
            }
        }
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(char::is_whitespace) {
            self.position += 1;
        }
    }

    fn expect(&mut self, expected: char) -> Result<(), String> {
        if self.consume(expected) {
            Ok(())
        } else {
            Err(format!("expected `{expected}`"))
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.position += 1;
            true
        } else {
            false
        }
    }

    fn consume_text(&mut self, expected: &str) -> bool {
        let expected = expected.chars().collect::<Vec<_>>();
        if self.characters[self.position..].starts_with(&expected) {
            self.position += expected.len();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.characters.get(self.position).copied()
    }

    fn next(&mut self) -> Option<char> {
        let value = self.peek()?;
        self.position += 1;
        Some(value)
    }
}

fn managed_error(reason: impl Into<String>) -> DoctorCheck {
    DoctorCheck::new("Managed installation", DoctorStatus::Error, reason)
}

fn check_path(executable: &Path) -> DoctorCheck {
    let Some(bin_directory) = executable.parent() else {
        return DoctorCheck::new("PATH", DoctorStatus::Warning, "executable has no directory");
    };
    let present = env::var_os("PATH")
        .into_iter()
        .flat_map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .any(|entry| same_path_lexical(&entry, bin_directory));
    if present {
        DoctorCheck::new("PATH", DoctorStatus::Ok, "contains the ASTER bin directory")
    } else {
        DoctorCheck::new(
            "PATH",
            DoctorStatus::Warning,
            "ASTER is not available through the current PATH",
        )
    }
}

fn check_compilation_probe(stdlib: &StandardLibrary) -> DoctorCheck {
    match compilation_probe(stdlib) {
        Ok(()) => DoctorCheck::new(
            "Compilation probe",
            DoctorStatus::Ok,
            "parsing, HIR, MIR, codegen, and execution succeeded",
        ),
        Err(reason) => DoctorCheck::new("Compilation probe", DoctorStatus::Error, short(&reason)),
    }
}

fn compilation_probe(stdlib: &StandardLibrary) -> Result<(), String> {
    let temporary = TemporaryDirectory::create("probe")?;
    let project = new_project::create_with_source(temporary.path(), "DoctorProbe", PROBE_SOURCE)?;
    let root_source = project.join("app/main.aster");
    let compilation = compile_project(&root_source, stdlib)?;
    let entry = aster_compiler::select_application_entry(&compilation, &root_source)
        .map_err(|diagnostics| short_application_diagnostics(&diagnostics))?;
    let value = aster_codegen_cranelift::execute_symbol(&compilation.compilation.mir, entry.symbol)
        .map_err(|error| error.to_string())?;
    if value != ExecutionValue::Int(42) {
        return Err(format!("probe returned unexpected value `{value}`"));
    }
    Ok(())
}

fn check_current_project(stdlib: &StandardLibrary) -> DoctorCheck {
    let current = match env::current_dir() {
        Ok(current) => current,
        Err(error) => {
            return DoctorCheck::new(
                "Current project",
                DoctorStatus::Error,
                format!("current directory is unavailable: {error}"),
            );
        }
    };
    if !current.join("Aster.toml").is_file() {
        return DoctorCheck::new(
            "Current project",
            DoctorStatus::Info,
            "Current directory is not an ASTER project",
        );
    }
    let root_source = current.join("app/main.aster");
    let result = compile_project(&root_source, stdlib).and_then(|project| {
        aster_compiler::select_application_entry(&project, &root_source)
            .map(|_| ())
            .map_err(|diagnostics| short_application_diagnostics(&diagnostics))
    });
    match result {
        Ok(()) => DoctorCheck::new(
            "Current project",
            DoctorStatus::Ok,
            current.display().to_string(),
        ),
        Err(reason) => DoctorCheck::new("Current project", DoctorStatus::Error, short(&reason)),
    }
}

fn compile_project(path: &Path, stdlib: &StandardLibrary) -> Result<ProjectCompilation, String> {
    let project = aster_compiler::compile_project_with_stdlib(path, stdlib.clone()).map_err(
        |diagnostics| {
            diagnostics.first().map_or_else(
                || "compilation failed".to_owned(),
                |value| short(&value.render()),
            )
        },
    )?;
    aster_codegen_cranelift::validate(&project.compilation.mir)
        .map_err(|error| error.to_string())?;
    Ok(project)
}

fn short_application_diagnostics(diagnostics: &[aster_compiler::ApplicationDiagnostic]) -> String {
    diagnostics.first().map_or_else(
        || "application entry validation failed".to_owned(),
        |value| short(&value.render()),
    )
}

fn short(message: &str) -> String {
    message.lines().next().unwrap_or(message).trim().to_owned()
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => same_path_lexical(&left, &right),
        _ => same_path_lexical(left, right),
    }
}

fn same_path_lexical(left: &Path, right: &Path) -> bool {
    let normalize = |path: &Path| {
        let text = path
            .components()
            .filter(|component| !matches!(component, Component::CurDir))
            .collect::<PathBuf>()
            .to_string_lossy()
            .trim_matches('"')
            .trim_end_matches(['/', '\\'])
            .to_owned();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    normalize(left) == normalize(right)
}

struct TemporaryDirectory {
    path: PathBuf,
}

impl TemporaryDirectory {
    fn create(label: &str) -> Result<Self, String> {
        for _ in 0..32 {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = env::temp_dir().join(format!(
                "aster-doctor-{label}-{}-{nanos:x}-{id:x}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(format!("could not create temporary directory: {error}"));
                }
            }
        }
        Err("could not create a unique temporary directory".into())
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{public_target, same_path_lexical};

    #[test]
    fn public_target_matches_the_supported_host() {
        assert!(matches!(public_target(), Some("windows-x64" | "linux-x64")));
    }

    #[test]
    fn path_comparison_is_platform_appropriate_and_lexical() {
        assert!(same_path_lexical(
            Path::new("C:/Aster/bin/"),
            Path::new("C:/Aster/bin")
        ));
        if cfg!(windows) {
            assert!(same_path_lexical(
                Path::new("C:/ASTER/BIN"),
                Path::new("c:/aster/bin")
            ));
        } else {
            assert!(!same_path_lexical(
                Path::new("/ASTER/bin"),
                Path::new("/aster/bin")
            ));
        }
    }
}
