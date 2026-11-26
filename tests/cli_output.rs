use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const BUDGET_SPEC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/budget.spec.json");

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("tblgen-output-{}-{}", std::process::id(), tag));
        fs::create_dir(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_tblgen"))
            .current_dir(&self.0)
            .args(args)
            .output()
            .unwrap()
    }

    #[cfg(target_os = "linux")]
    fn run_with_file_limit(&self, blocks: &str, args: &[&str]) -> Output {
        Command::new("/bin/sh")
            .current_dir(&self.0)
            // Inherit the ignored signal so the kernel returns EFBIG to write(2).
            .args([
                "-c",
                "trap '' XFSZ; ulimit -f \"$1\" || exit 99; shift; exec \"$@\"",
                "tblgen-test",
                blocks,
            ])
            .arg(env!("CARGO_BIN_EXE_tblgen"))
            .args(args)
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_written<'a>(output: &'a Output, paths: &[&str]) -> &'a str {
    let stderr = std::str::from_utf8(&output.stderr).unwrap();
    let actual: Vec<_> = stderr
        .lines()
        .filter_map(|line| line.strip_prefix("tblgen: wrote "))
        .collect();
    assert_eq!(actual, paths, "{}", stderr);
    stderr
}

#[test]
fn single_file_output_preserves_stdout_bytes_and_existing_targets() {
    let sandbox = Sandbox::new("single");
    for (cmd, input, path) in [
        ("gen", BUDGET_SPEC, "out.tbl"),
        ("decompile", "out.tbl", "out.spec.json"),
    ] {
        let stdout = sandbox.run(&[cmd, input]);
        assert!(stdout.status.success(), "{:?}", stdout);
        assert!(stdout.stdout.ends_with(b"\n"));

        let missing_parent = sandbox.run(&[cmd, input, "-o", "missing/out"]);
        assert_eq!(missing_parent.status.code(), Some(2));
        assert!(!sandbox.0.join("missing").exists());

        let output = sandbox.run(&[cmd, input, "-o", path]);
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stdout.is_empty());
        assert_written(&output, &[path]);
        assert_eq!(fs::read(sandbox.0.join(path)).unwrap(), stdout.stdout);

        let retry = sandbox.run(&[cmd, input, "-o", path]);
        assert_eq!(retry.status.code(), Some(2));
        assert!(assert_written(&retry, &[]).contains("already exists"));
        assert_eq!(fs::read(sandbox.0.join(path)).unwrap(), stdout.stdout);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn single_file_write_failure_allows_retry() {
    let sandbox = Sandbox::new("single-write-failure");
    let args = ["gen", BUDGET_SPEC, "-o", "out.tbl"];
    let failure = sandbox.run_with_file_limit("0", &args);
    assert_eq!(failure.status.code(), Some(2), "{:?}", failure);
    assert!(assert_written(&failure, &[]).contains("out.tbl:"));
    assert!(!sandbox.0.join("out.tbl").exists());
    let retry = sandbox.run(&args);
    assert!(retry.status.success(), "{:?}", retry);
    assert_written(&retry, &["out.tbl"]);
}

#[cfg(target_os = "linux")]
#[test]
fn new_rolls_back_later_reservation_and_partial_write_failures() {
    let sandbox = Sandbox::new("new-failures");
    let paths = ["out/model.tbl", "out/script.action.ts", "out/spec.json"];
    let note = "x".repeat(65_536);
    let too_long = format!("out/{}", "x".repeat(4096));
    let mut args = [
        "new",
        "script",
        "--table",
        "demo",
        "--extract",
        "false",
        "--fields",
        "f:C",
        "-o",
        paths[0],
        "--note",
        &note,
        "--spec-out",
        paths[2],
        "--script-out",
        &too_long,
    ];
    let reserve_failure = sandbox.run(&args);
    assert_eq!(
        reserve_failure.status.code(),
        Some(2),
        "{:?}",
        reserve_failure
    );
    assert!(assert_written(&reserve_failure, &[]).contains(&too_long));
    assert_eq!(fs::read_dir(sandbox.0.join("out")).unwrap().count(), 0);

    *args.last_mut().unwrap() = paths[1];
    let write_failure = sandbox.run_with_file_limit("32", &args);
    assert_eq!(write_failure.status.code(), Some(2), "{:?}", write_failure);
    assert!(assert_written(&write_failure, &[]).contains("script.action.ts:"));
    assert_eq!(fs::read_dir(sandbox.0.join("out")).unwrap().count(), 0);

    let retry = sandbox.run(&args);
    assert!(retry.status.success(), "{:?}", retry);
    assert!(retry.stdout.is_empty());
    let full_paths: Vec<_> = paths.iter().map(|path| sandbox.0.join(path)).collect();
    let reported: Vec<_> = full_paths
        .iter()
        .map(|path| path.to_str().unwrap())
        .collect();
    assert_written(&retry, &reported);
    let generated = sandbox.run(&["gen", paths[2]]);
    assert!(generated.status.success(), "{:?}", generated);
    assert_eq!(fs::read(&full_paths[0]).unwrap(), generated.stdout);
    let expected_script = tblgen::new::script_source(
        false,
        "demo",
        "demo",
        &note,
        &tblgen::new::parse_fields("f:C", false).unwrap(),
    );
    assert_eq!(
        fs::read(&full_paths[1]).unwrap(),
        expected_script.as_bytes()
    );
}
