//! End-to-end integration tests that drive the real `smod` binary.
//!
//! Unlike the unit tests (which call business-logic functions directly with a
//! `MockRegistryClient`), these spawn the compiled `smod` executable and assert
//! on its stdout/stderr and exit code, exercising the full stack — argument
//! parsing, command dispatch, the embedded registry, the real `packages/*.zip`
//! archives, and on-disk `smod.toml`/`smod.lock` — exactly as a user would.
//!
//! Every test runs inside its own [`TempDir`], so they neither touch the
//! developer's working tree nor interfere with one another. Installs work from
//! a temp directory because the installer resolves the embedded registry's
//! relative archive paths against the crate manifest dir, not the current
//! directory.

use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

/// Run `smod <args...>` inside `dir` and capture its output.
///
/// `--no-color` is passed globally so assertions match plain text regardless of
/// whether the test's stdout happens to look like a terminal.
fn smod(dir: &Path, args: &[&str]) -> Output {
    let mut full = vec!["--no-color"];
    full.extend_from_slice(args);
    Command::new(env!("CARGO_BIN_EXE_smod"))
        .args(&full)
        .current_dir(dir)
        .output()
        .expect("failed to spawn smod binary")
}

/// Convenience: stdout as a `String`.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Convenience: stderr as a `String`.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Create a fresh project via `smod new <name>` inside a temp dir and return
/// `(tempdir, project_root)`.
fn new_project(name: &str) -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let out = smod(tmp.path(), &["new", name]);
    assert!(out.status.success(), "`smod new` failed: {}", stderr(&out));
    let root = tmp.path().join(name);
    (tmp, root)
}

#[test]
fn new_creates_a_full_project_layout() {
    let (_tmp, root) = new_project("my-module");
    assert!(root.join("smod.toml").is_file());
    assert!(root.join("src").join("lib.rs").is_file());
    assert!(root.join("README.md").is_file());

    // The generated manifest carries the requested name.
    let manifest = std::fs::read_to_string(root.join("smod.toml")).unwrap();
    assert!(manifest.contains("name = \"my-module\""));
}

#[test]
fn new_rejects_an_invalid_name() {
    let tmp = TempDir::new().unwrap();
    let out = smod(tmp.path(), &["new", "../escape"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("invalid package name"),
        "stderr was: {}",
        stderr(&out)
    );
}

#[test]
fn install_then_list_reports_the_dependency() {
    let (_tmp, root) = new_project("app");

    let install = smod(&root, &["install", "payment-stream"]);
    assert!(
        install.status.success(),
        "install failed: {}",
        stderr(&install)
    );
    let out = stdout(&install);
    // The staged progress feedback is present.
    assert!(out.contains("Resolving package"), "output: {out}");
    assert!(out.contains("Verifying checksum"), "output: {out}");
    assert!(out.contains("Installed"), "output: {out}");

    // The module was extracted, and the lockfile records it.
    assert!(root.join("smod_modules").join("payment-stream").is_dir());
    assert!(root.join("smod.lock").is_file());

    let list = smod(&root, &["list"]);
    assert!(list.status.success());
    assert!(stdout(&list).contains("payment-stream"));
}

#[test]
fn list_json_is_machine_readable() {
    let (_tmp, root) = new_project("app");
    smod(&root, &["install", "payment-stream"]);

    let out = smod(&root, &["list", "--json"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.trim_start().starts_with('['), "not JSON: {text}");
    assert!(text.contains("\"name\": \"payment-stream\""), "was: {text}");
    assert!(text.contains("\"status\": \"installed\""), "was: {text}");
}

#[test]
fn search_finds_and_formats_packages() {
    let tmp = TempDir::new().unwrap();

    let human = smod(tmp.path(), &["search", "payment"]);
    assert!(human.status.success());
    let text = stdout(&human);
    assert!(text.contains("Found packages:"), "was: {text}");
    assert!(text.contains("NAME"), "header missing: {text}");
    assert!(text.contains("payment-stream"), "was: {text}");

    let json = smod(tmp.path(), &["search", "payment", "--json"]);
    assert!(json.status.success());
    assert!(stdout(&json).trim_start().starts_with('['));
}

#[test]
fn info_shows_metadata_including_dependencies() {
    let tmp = TempDir::new().unwrap();
    let out = smod(tmp.path(), &["info", "payment-stream"]);
    assert!(out.status.success());
    let text = stdout(&out);
    assert!(text.contains("program id"), "was: {text}");
    assert!(text.contains("dependencies"), "was: {text}");
    assert!(text.contains("token-vault"), "was: {text}");

    // Unknown package fails with a non-zero exit code.
    let missing = smod(tmp.path(), &["info", "does-not-exist"]);
    assert!(!missing.status.success());
}

#[test]
fn verify_and_doctor_pass_for_a_healthy_project() {
    let (_tmp, root) = new_project("app");
    smod(&root, &["install", "payment-stream"]);

    let verify = smod(&root, &["verify"]);
    assert!(
        verify.status.success(),
        "verify failed: {}",
        stderr(&verify)
    );
    assert!(stdout(&verify).contains("verified"));

    let doctor = smod(&root, &["doctor"]);
    assert!(
        doctor.status.success(),
        "doctor failed: {}",
        stderr(&doctor)
    );
    assert!(stdout(&doctor).contains("Everything looks good"));
}

#[test]
fn update_reports_up_to_date_for_the_latest_version() {
    let (_tmp, root) = new_project("app");
    smod(&root, &["install", "payment-stream"]);

    // The embedded registry has no newer version than what was just installed.
    let out = smod(&root, &["update", "payment-stream"]);
    assert!(out.status.success(), "update failed: {}", stderr(&out));
    assert!(stdout(&out).contains("up to date"));
}

#[test]
fn remove_deletes_module_and_manifest_entry() {
    let (_tmp, root) = new_project("app");
    smod(&root, &["install", "payment-stream"]);
    assert!(root.join("smod_modules").join("payment-stream").is_dir());

    let out = smod(&root, &["remove", "payment-stream"]);
    assert!(out.status.success(), "remove failed: {}", stderr(&out));
    assert!(stdout(&out).contains("Removed"));

    // The module directory is gone and the dependency is dropped from the manifest.
    assert!(!root.join("smod_modules").join("payment-stream").exists());
    let manifest = std::fs::read_to_string(root.join("smod.toml")).unwrap();
    assert!(!manifest.contains("payment-stream"));
}

#[test]
fn full_lifecycle_end_to_end() {
    // new -> install -> list -> verify -> doctor -> update -> remove, all in one
    // project, asserting the exit code at each step.
    let (_tmp, root) = new_project("lifecycle");

    for args in [
        &["install", "payment-stream"][..],
        &["list"][..],
        &["verify"][..],
        &["doctor"][..],
        &["update"][..],
        &["remove", "payment-stream"][..],
    ] {
        let out = smod(&root, args);
        assert!(
            out.status.success(),
            "step `smod {}` failed: {}",
            args.join(" "),
            stderr(&out)
        );
    }

    // After removal nothing is installed, and the project is still healthy.
    let doctor = smod(&root, &["doctor"]);
    assert!(
        doctor.status.success(),
        "final doctor failed: {}",
        stderr(&doctor)
    );
}
