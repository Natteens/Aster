use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
type ManagedMutation = (&'static str, fn(&Path));

#[test]
fn healthy_checkout_doctor_is_ordered_dynamic_and_side_effect_free() {
    let directory = temporary_directory("checkout healthy");
    let output = doctor(binary(), &directory, None, true);
    assert!(output.status.success(), "{}", stderr(&output));
    let text = stdout(&output);
    assert_order(
        &text,
        &[
            "Version:",
            "Platform:",
            "Executable:",
            "Standard library origin:",
            "Standard library structure:",
            "Managed installation:",
            "PATH:",
            "Compilation probe:",
            "Current project:",
        ],
    );
    assert!(text.contains(&format!("[OK] Version: {}", env!("CARGO_PKG_VERSION"))));
    assert!(
        text.contains("[OK] Platform: windows-x64") || text.contains("[OK] Platform: linux-x64")
    );
    assert!(text.contains("[OK] Standard library origin: Embedded"));
    assert!(text.contains("[OK] Compilation probe:"));
    assert!(text.contains("[INFO] Managed installation: marker was not found"));
    assert!(text.contains("[INFO] Current project: Current directory is not an ASTER project"));
    assert!(!text.contains("42\n"), "probe output leaked: {text}");
    assert!(fs::read_dir(&directory).expect("read cwd").next().is_none());
    fs::remove_dir_all(directory).expect("remove checkout directory");
}

#[test]
fn environment_stdlib_reports_its_origin_and_invalid_override_never_falls_back() {
    let directory = temporary_directory("environment");
    let stdlib = temporary_directory("environment stdlib ü");
    copy_real_stdlib(&stdlib);
    let valid = doctor(binary(), &directory, Some(&stdlib), true);
    assert!(valid.status.success(), "{}", stderr(&valid));
    assert!(stdout(&valid).contains("[OK] Standard library origin: Environment"));

    let missing = stdlib.join("missing");
    let invalid = doctor(binary(), &directory, Some(&missing), true);
    assert!(!invalid.status.success());
    let text = stdout(&invalid);
    assert!(text.contains("[ERROR] Standard library structure: ASTER_STDLIB"));
    assert!(text.contains("[ERROR] Compilation probe: skipped"));
    assert!(!text.contains("origin: Embedded"));

    fs::remove_dir_all(directory).expect("remove environment cwd");
    fs::remove_dir_all(stdlib).expect("remove environment stdlib");
}

#[test]
fn executable_relative_bundle_and_managed_install_are_validated() {
    let layout = managed_layout("instalação ASTER com espaços ü");
    let cwd = temporary_directory("managed cwd");
    let executable = layout.join("bin").join(binary_name());

    let bundle = doctor(&executable, &cwd, None, true);
    assert!(bundle.status.success(), "{}", stderr(&bundle));
    assert!(stdout(&bundle).contains("[OK] Standard library origin: Executable-relative"));
    assert!(stdout(&bundle).contains("[INFO] Managed installation: marker was not found"));

    write_state(&layout, env!("CARGO_PKG_VERSION"), target());
    let managed = doctor(&executable, &cwd, None, true);
    assert!(managed.status.success(), "{}", stderr(&managed));
    let text = stdout(&managed);
    assert!(text.contains(&format!(
        "[OK] Managed installation: {}",
        env!("CARGO_PKG_VERSION")
    )));
    assert!(text.contains("[OK] PATH: contains the ASTER bin directory"));

    fs::remove_dir_all(layout).expect("remove managed layout");
    fs::remove_dir_all(cwd).expect("remove managed cwd");
}

#[test]
fn incomplete_executable_relative_stdlib_is_an_error_without_embedded_fallback() {
    let layout = managed_layout("incomplete-relative");
    fs::remove_file(layout.join("stdlib/aster/math.aster")).expect("remove required module");
    let cwd = temporary_directory("incomplete cwd");
    let output = doctor(&layout.join("bin").join(binary_name()), &cwd, None, true);
    assert!(!output.status.success());
    let text = stdout(&output);
    assert!(text.contains(
        "[ERROR] Standard library structure: The installed standard library is incomplete"
    ));
    assert!(!text.contains("origin: Embedded"));
    fs::remove_dir_all(layout).expect("remove incomplete layout");
    fs::remove_dir_all(cwd).expect("remove incomplete cwd");
}

#[test]
fn managed_install_rejects_invalid_state_manifest_versions_target_entrypoint_and_stdlib() {
    let cases: &[ManagedMutation] = &[
        ("invalid-state", |root| {
            fs::write(root.join("install-state.json"), "{ invalid").expect("write invalid state");
        }),
        ("invalid-manifest", |root| {
            fs::write(root.join("install-manifest.json"), "{ invalid")
                .expect("write invalid manifest");
        }),
        ("version", |root| write_state(root, "0.0.0", target())),
        ("target", |root| {
            write_state(root, env!("CARGO_PKG_VERSION"), "other-target");
        }),
        ("entrypoint", |root| {
            write_manifest(root, env!("CARGO_PKG_VERSION"), target(), "bin/not-aster");
        }),
        ("stdlib", |root| {
            fs::remove_file(root.join("stdlib/aster/math.aster")).expect("remove stdlib module");
        }),
    ];
    let cwd = temporary_directory("managed invalid cwd");
    for (label, mutate) in cases {
        let layout = managed_layout(label);
        write_state(&layout, env!("CARGO_PKG_VERSION"), target());
        mutate(&layout);
        let output = doctor(&layout.join("bin").join(binary_name()), &cwd, None, true);
        assert!(!output.status.success(), "{label} unexpectedly succeeded");
        assert!(
            stdout(&output).contains("[ERROR] Managed installation:"),
            "{label}: {}",
            stdout(&output)
        );
        fs::remove_dir_all(layout).expect("remove invalid managed layout");
    }
    fs::remove_dir_all(cwd).expect("remove invalid managed cwd");
}

#[test]
fn path_check_is_warning_only_and_similar_entry_does_not_count() {
    let directory = temporary_directory("path");
    let output = doctor(binary(), &directory, None, false);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("[WARN] PATH: ASTER is not available"));
    assert!(text.contains("completed with warnings"));
    fs::remove_dir_all(directory).expect("remove path directory");
}

#[test]
fn current_project_is_checked_without_execution_or_parent_search() {
    let parent = temporary_directory("projects");
    let new_output = command(binary(), &parent)
        .args(["new", "Healthy"])
        .env_remove("ASTER_STDLIB")
        .output()
        .expect("create project");
    assert!(new_output.status.success(), "{}", stderr(&new_output));
    let project = parent.join("Healthy");
    let healthy = doctor(binary(), &project, None, true);
    assert!(healthy.status.success(), "{}", stderr(&healthy));
    assert!(stdout(&healthy).contains("[OK] Current project:"));
    assert!(!stdout(&healthy).contains("Hello from ASTER!"));

    fs::write(project.join("Aster.toml"), "{ invalid").expect("damage manifest");
    let before = fs::read(project.join("Aster.toml")).expect("snapshot manifest");
    let invalid = doctor(binary(), &project, None, true);
    assert!(!invalid.status.success());
    assert!(stdout(&invalid).contains("[ERROR] Current project:"));
    assert_eq!(
        fs::read(project.join("Aster.toml")).expect("read manifest after doctor"),
        before
    );

    let child = parent.join("child");
    fs::create_dir(&child).expect("create child");
    fs::write(
        parent.join("Aster.toml"),
        "[package]\nname = \"parent\"\n\n[application]\nentry = \"app.Program.Main\"\n",
    )
    .expect("write parent manifest");
    let no_parent_search = doctor(binary(), &child, None, true);
    assert!(no_parent_search.status.success());
    assert!(stdout(&no_parent_search).contains("Current directory is not an ASTER project"));
    fs::remove_dir_all(parent).expect("remove projects directory");
}

#[test]
fn doctor_rejects_arguments_and_is_advertised_by_help() {
    let directory = temporary_directory("cli");
    let extra = command(binary(), &directory)
        .args(["doctor", "extra"])
        .output()
        .expect("run doctor with extra argument");
    assert!(!extra.status.success());
    assert!(stderr(&extra).contains("unexpected argument `extra`"));

    let help = command(binary(), &directory)
        .arg("--help")
        .output()
        .expect("run help");
    assert!(help.status.success());
    assert!(stdout(&help).contains("doctor"));
    assert!(stdout(&help).contains("Diagnose the ASTER installation and environment"));
    fs::remove_dir_all(directory).expect("remove CLI directory");
}

fn doctor(
    executable: &Path,
    cwd: &Path,
    stdlib: Option<&Path>,
    include_bin_in_path: bool,
) -> Output {
    let mut command = command(executable, cwd);
    command.arg("doctor");
    match stdlib {
        Some(path) => {
            command.env("ASTER_STDLIB", path);
        }
        None => {
            command.env_remove("ASTER_STDLIB");
        }
    }
    let executable_directory = executable.parent().expect("binary directory");
    let path = if include_bin_in_path {
        let mut entries = vec![executable_directory.to_path_buf()];
        entries.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        env::join_paths(entries).expect("join PATH")
    } else {
        env::join_paths([executable_directory.with_file_name("similar-bin")]).expect("join PATH")
    };
    command.env("PATH", path);
    output_with_executable_busy_retry(&mut command).expect("run aster doctor")
}

fn output_with_executable_busy_retry(command: &mut Command) -> io::Result<Output> {
    const RETRIES: u32 = 5;
    for attempt in 0..=RETRIES {
        match command.output() {
            Err(error) if is_executable_busy(&error) && attempt < RETRIES => {
                thread::sleep(Duration::from_millis(10 * (1 << attempt)));
            }
            result => return result,
        }
    }
    unreachable!("retry loop always returns on its final attempt")
}

fn is_executable_busy(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::ExecutableFileBusy
}

fn command(executable: &Path, cwd: &Path) -> Command {
    let mut command = Command::new(executable);
    command.current_dir(cwd);
    command
}

fn managed_layout(label: &str) -> PathBuf {
    let root = temporary_directory(label);
    fs::create_dir(root.join("bin")).expect("create bin");
    fs::copy(binary(), root.join("bin").join(binary_name())).expect("copy binary");
    copy_real_stdlib(&root.join("stdlib"));
    fs::write(root.join("LICENSE"), "license\n").expect("write license");
    write_manifest(
        &root,
        env!("CARGO_PKG_VERSION"),
        target(),
        &format!("bin/{}", binary_name()),
    );
    root
}

fn write_state(root: &Path, version: &str, target: &str) {
    fs::write(
        root.join("install-state.json"),
        format!(
            "{{\n  \"schema\": 1,\n  \"product\": \"aster\",\n  \"version\": \"{version}\",\n  \"target\": \"{target}\"\n}}\n"
        ),
    )
    .expect("write state");
}

fn write_manifest(root: &Path, version: &str, target: &str, entrypoint: &str) {
    fs::write(
        root.join("install-manifest.json"),
        format!(
            "{{\n  \"schema\": 1,\n  \"product\": \"aster\",\n  \"version\": \"{version}\",\n  \"target\": \"{target}\",\n  \"entrypoint\": \"{entrypoint}\",\n  \"stdlib\": \"stdlib\",\n  \"license\": \"LICENSE\"\n}}\n"
        ),
    )
    .expect("write manifest");
}

fn copy_real_stdlib(root: &Path) {
    let workspace_stdlib = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../stdlib");
    for relative in [
        "aster/math.aster",
        "aster/text/text.aster",
        "aster/core/core.aster",
        "aster/io/io.aster",
        "aster/collections/collections.aster",
        "aster/testing/testing.aster",
        "aster/random/random.aster",
        "aster/time/time.aster",
    ] {
        let destination = root.join(relative);
        fs::create_dir_all(destination.parent().expect("module parent"))
            .expect("create module dir");
        fs::copy(workspace_stdlib.join(relative), destination).expect("copy stdlib module");
    }
}

fn binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_aster"))
}

fn binary_name() -> &'static str {
    if cfg!(windows) { "aster.exe" } else { "aster" }
}

fn target() -> &'static str {
    if cfg!(windows) {
        "windows-x64"
    } else {
        "linux-x64"
    }
}

fn temporary_directory(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = env::temp_dir().join(format!(
        "aster-doctor-integration-{label}-{}-{id}",
        std::process::id()
    ));
    fs::create_dir(&path).expect("create temporary directory");
    path
}

fn assert_order(text: &str, labels: &[&str]) {
    let mut previous = 0;
    for label in labels {
        let position = text
            .find(label)
            .unwrap_or_else(|| panic!("missing {label}: {text}"));
        assert!(position >= previous, "{label} is out of order");
        previous = position;
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("UTF-8 stdout")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("UTF-8 stderr")
}

#[test]
fn executable_busy_is_the_only_retryable_doctor_spawn_error() {
    assert!(is_executable_busy(&io::Error::from(
        io::ErrorKind::ExecutableFileBusy
    )));
    assert!(!is_executable_busy(&io::Error::from(
        io::ErrorKind::PermissionDenied
    )));
}
