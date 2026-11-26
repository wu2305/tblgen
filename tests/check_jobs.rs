//! `tblgen check --jobs N`: parallel batch checking must be observably
//! identical to the serial path — same per-file order in the JSON report,
//! same stderr lines (in file order), same exit codes — across valid files,
//! diagnostics, I/O failures, directories, project references and mixed
//! relative/absolute spellings.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn fixture(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tblgen-jobs-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Sandbox { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.join(rel)
    }

    fn write(&self, rel: &str, contents: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, contents).unwrap();
        p
    }

    fn copy_fixture(&self, fixture_rel: &str, rel: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::copy(fixture(fixture_rel), &p).unwrap();
        p
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run(bin: &str, args: &[String]) -> Output {
    // run from the sandbox parent so relative-path spellings can be exercised
    Command::new(bin).args(args).output().unwrap()
}

fn run_in(bin: &str, cwd: &Path, args: &[String]) -> Output {
    Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn check_args(paths: &[String], jobs: Option<&str>) -> Vec<String> {
    let mut v = vec!["check".to_string()];
    if let Some(j) = jobs {
        v.push(format!("--jobs={}", j));
    }
    v.extend(paths.iter().cloned());
    v
}

fn assert_same(a: &Output, b: &Output, what: &str) {
    assert_eq!(
        a.status.code(),
        b.status.code(),
        "{}: exit codes differ\nstdout A:\n{}\nstderr A:\n{}\nstdout B:\n{}\nstderr B:\n{}",
        what,
        String::from_utf8_lossy(&a.stdout),
        String::from_utf8_lossy(&a.stderr),
        String::from_utf8_lossy(&b.stdout),
        String::from_utf8_lossy(&b.stderr),
    );
    assert_eq!(a.stdout, b.stdout, "{}: stdout differs", what);
    assert_eq!(a.stderr, b.stderr, "{}: stderr differs", what);
}

/// Build a directory tree exercising every check_file route: clean dataflow,
/// dataflow with diagnostics, spec doc, invalid JSON, non-flow JSON,
/// .action.ts script, plus a two-file project with a model reference.
fn build_tree(sb: &Sandbox) -> Vec<String> {
    sb.copy_fixture(
        "tests/fixtures/dataflow_sql.tbl",
        "proj/data/tables/ods/src.tbl",
    );
    sb.copy_fixture(
        "tests/fixtures/dataflow_pres.tbl",
        "proj/data/tables/ods/dst.tbl",
    );
    sb.copy_fixture(
        "tests/fixtures/dataflow_pres.tbl",
        "proj/data/tables/dataflow_xxb_year.tbl",
    );
    sb.copy_fixture("examples/budget.spec.json", "specs/budget.spec.json");
    sb.write("bad/json.tbl", "{not json");
    sb.write("plain/data.json", "{\"hello\": 1}");
    sb.write("plain/nonflow.tbl", "[1,2,3]");
    sb.write(
        "plain/script.action.ts",
        "export const x = context.params.y;\n",
    );
    sb.write(
        "proj/data/tables/ads/ref.tbl",
        r#"{
        "dataFlow": {"nodes": {
            "m": {"id": "m", "type": "ModelTable", "alias": "m",
                  "moduleTablePath": "../ods/src.tbl", "fields": []},
            "s": {"id": "s", "type": "Script", "alias": "s", "scriptName": "myscript",
                  "fields": [{"name": "f1", "dbfield": "F1"}], "inputNodes": ["m"]},
            "o": {"id": "o", "type": "Output", "alias": "o", "inputNodes": ["s"], "fields": []}
        }}
    }"#,
    );
    sb.write(
        "proj/data/script/myscript.action.ts",
        "// ok\nexport function run() {}\n",
    );
    vec![
        "proj/data/tables/ods/src.tbl",
        "proj/data/tables/ods/dst.tbl",
        "proj/data/tables/ads/ref.tbl",
        "proj/data/tables/dataflow_xxb_year.tbl",
        "specs/budget.spec.json",
        "bad/json.tbl",
        "plain/data.json",
        "plain/nonflow.tbl",
        "plain/script.action.ts",
    ]
    .into_iter()
    .map(|r| sb.path(r).display().to_string())
    .collect()
}

#[test]
fn check_jobs_matches_serial_on_mixed_tree() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let sb = Sandbox::new("mixed");
    let files = build_tree(&sb);

    let serial = run(bin, &check_args(&files, Some("1")));
    for jobs in ["2", "4", "8"] {
        let par = run(bin, &check_args(&files, Some(jobs)));
        assert_same(&serial, &par, &format!("jobs={}", jobs));
    }
    // default (auto parallelism) agrees too
    let auto = run(bin, &check_args(&files, None));
    assert_same(&serial, &auto, "default jobs");

    // the serial run must exercise both clean and failing files
    let report: serde_json::Value = serde_json::from_slice(&serial.stdout).unwrap();
    let arr = report["files"].as_array().unwrap();
    assert_eq!(arr.len(), files.len());
    assert!(serial.status.code() != Some(0), "mixed tree should fail");
    assert!(
        serial.stderr.len() > 0,
        "io/invalid-json files log to stderr"
    );
}

#[test]
fn check_jobs_directory_and_exclude() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let sb = Sandbox::new("dir");
    build_tree(&sb);
    let root = sb.path("proj").display().to_string();

    let serial = run(bin, &check_args(&[root.clone()], Some("1")));
    for jobs in ["2", "4"] {
        let par = run(bin, &check_args(&[root.clone()], Some(jobs)));
        assert_same(&serial, &par, &format!("dir jobs={}", jobs));
    }

    // --exclude a subdirectory: same filtered set under any worker count
    let excl = sb.path("proj/data/tables/ads").display().to_string();
    let args = |jobs: &str| {
        vec![
            "check".to_string(),
            format!("--jobs={}", jobs),
            "--exclude".to_string(),
            excl.clone(),
            root.clone(),
        ]
    };
    let s1 = run(bin, &args("1"));
    for jobs in ["3", "5"] {
        assert_same(
            &s1,
            &run(bin, &args(jobs)),
            &format!("exclude jobs={}", jobs),
        );
    }
}

#[test]
fn check_jobs_missing_and_mixed_paths() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let sb = Sandbox::new("missing");
    let good = sb.copy_fixture("tests/fixtures/dataflow_sql.tbl", "a.tbl");
    let missing = sb.path("does/not/exist.tbl").display().to_string();
    let good_s = good.display().to_string();

    // missing file -> exit 2 with io diag; mixed with a good file keeps order
    let serial = run(
        bin,
        &check_args(&[good_s.clone(), missing.clone()], Some("1")),
    );
    assert_eq!(serial.status.code(), Some(2));
    for jobs in ["2", "4"] {
        assert_same(
            &serial,
            &run(
                bin,
                &check_args(&[good_s.clone(), missing.clone()], Some(jobs)),
            ),
            &format!("missing jobs={}", jobs),
        );
    }

    // same file spelled twice stays twice, in argument order
    let twice = vec![good_s.clone(), good_s.clone()];
    let s1 = run(bin, &check_args(&twice, Some("1")));
    let s4 = run(bin, &check_args(&twice, Some("4")));
    assert_same(&s1, &s4, "duplicate arg");
    let report: serde_json::Value = serde_json::from_slice(&s1.stdout).unwrap();
    assert_eq!(report["files"].as_array().unwrap().len(), 2);

    // relative + absolute + .. spellings of the same file
    let rel = format!(
        "{}/{}",
        sb.dir.file_name().unwrap().to_string_lossy(),
        good.file_name().unwrap().to_string_lossy()
    );
    let dotdot = format!(
        "{}/./{}",
        sb.dir.display(),
        good.file_name().unwrap().to_string_lossy()
    );
    let mixed = vec![rel, good_s.clone(), dotdot];
    let cwd = sb.dir.parent().unwrap().to_path_buf();
    let s1 = run_in(bin, &cwd, &check_args(&mixed, Some("1")));
    let s4 = run_in(bin, &cwd, &check_args(&mixed, Some("4")));
    assert_same(&s1, &s4, "mixed spellings");
}

#[test]
fn check_jobs_rejects_bad_values() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let sb = Sandbox::new("badjobs");
    let f = sb.copy_fixture("tests/fixtures/dataflow_sql.tbl", "a.tbl");
    let f = f.display().to_string();
    for bad in [
        vec!["--jobs=0"],
        vec!["--jobs=-1"],
        vec!["--jobs=abc"],
        vec!["--jobs"],
        vec!["--jobs=1", "--jobs=2"],
    ] {
        let mut args = vec!["check".to_string()];
        args.extend(bad.iter().map(|s| s.to_string()));
        args.push(f.clone());
        let out = Command::new(bin).args(&args).output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(2),
            "args {:?} should exit 2, got {:?}",
            args,
            out.status.code()
        );
    }
    // bare `--jobs` consumes the next arg as its value -> invalid usize
    let out = Command::new(bin)
        .args(["check", "--jobs", &f])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn check_jobs_unreadable_file() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let sb = Sandbox::new("unreadable");
    let f = sb.copy_fixture("tests/fixtures/dataflow_sql.tbl", "locked/a.tbl");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o000)).unwrap();
    }
    let fs_str = f.display().to_string();
    let serial = run(bin, &check_args(&[fs_str.clone()], Some("1")));
    let par = run(bin, &check_args(&[fs_str.clone()], Some("4")));
    assert_same(&serial, &par, "unreadable");
    #[cfg(unix)]
    {
        let euid = Command::new("id").arg("-u").output().unwrap().stdout;
        let root = String::from_utf8_lossy(&euid).trim() == "0";
        if root {
            // root reads anything; the file was still checked, not errored
            let report: serde_json::Value = serde_json::from_slice(&serial.stdout).unwrap();
            assert_eq!(report["files"][0]["scope"], serde_json::json!("dataflow"));
        } else {
            assert_eq!(serial.status.code(), Some(2));
        }
    }
}
