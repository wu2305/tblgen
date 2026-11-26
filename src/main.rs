//! tblgen CLI — offline `.tbl` <-> spec-format v1 converter.
//!
//! Commands:
//!   gen <spec.json> [-o out.tbl]     compile spec -> .tbl (stdout or file)
//!   decompile <file.tbl> [-o out.json]  .tbl -> spec (stdout or file)
//!   check <file.tbl|spec.json>...    validate wire .tbl files / specs
//!   schema                           print JSON Schema 2020-12 + examples
//!
//! Exit codes: 0 ok, 1 invalid data, 2 usage / I/O error.
//! JSON results on stdout; human diagnostics on stderr.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use serde_json::{json, Value};

use tblgen::check::{check_file, CheckReport};
use tblgen::decompile::decompile;
use tblgen::gen::generate_with_diagnostics;
use tblgen::schema::schema_document;
use tblgen::Diag;

mod output;

use output::{json_text, write_exclusive_all, write_out};

const USAGE: &str = "tblgen — offline .tbl <-> spec-format v1 converter

USAGE:
    tblgen gen <spec.json> [-o out.tbl]
    tblgen decompile <file.tbl> [-o out.json]
    tblgen check <file|dir>... [--exclude <path>]... [--jobs N]
    tblgen schema
    tblgen new script --table <name> --fields <list> --extract <true|false> -o <out.tbl>
                      [--schema S] [--datasource D] [--version V] [--script-name N]
                      [--script-out P | --no-script] [--template placeholder|transform]
                      [--cn C] [--note N] [--spec-out P]
    tblgen new dsl    --table <name> --input <ref> [--input <ref>] --extract <true|false>
                      -o <out.tbl> [--fields <names>] [--join-on K] [--join-type T]
                      [--schema S] [--datasource D] [--version V] [--spec-out P]

check: a directory expands to every .tbl and .action.ts below it. Inside a
project (a directory with data/), model references and scripts are resolved.
--exclude skips everything under a path (e.g. platform scripts you do not own).
--jobs N checks files with N worker threads (default: up to 4, i.e.
available parallelism capped at 4; 1 disables); an explicit N above 4 is
honored. Output order, diagnostics and exit codes are unchanged.
new: --extract true = ETL into --table; false = query logic. Field list is
name[=dbfield]:TYPE[:length[.decimal]][:m] (TYPE C/N/I/D/P, m = measure),
e.g. 统计年月=stat_month:C:7; dbfield defaults to the upper-cased name and
must be ASCII. dsl takes field names (name=dbfield overrides just the output
column). --input is the reference as written in the model (../ods/x.tbl,
$DATA:/ods/x.tbl). Script name defaults to md5(--table); the script goes to
<project>/data/script/. --cn/--note only fill the script header comment (a
model's display name lives in the folder's .meta, not in the .tbl).
Existing files are never overwritten. `tblgen new --help` shows this text.

EXIT CODES: 0 ok | 1 invalid data | 2 usage / I/O error";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("{}", USAGE);
        return ExitCode::from(2);
    }
    match args[0].as_str() {
        "gen" => cmd_gen(&args[1..]),
        "decompile" => cmd_decompile(&args[1..]),
        "check" => cmd_check(&args[1..]),
        "schema" => cmd_schema(&args[1..]),
        "new" => cmd_new(&args[1..]),
        // help goes to stderr only — stdout is reserved for JSON results
        "-h" | "--help" | "help" => {
            eprintln!("{}", USAGE);
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("tblgen: unknown command {:?}\n\n{}", other, USAGE);
            ExitCode::from(2)
        }
    }
}

fn parse_io_args(args: &[String]) -> Result<(Vec<String>, Option<String>), String> {
    let mut files = Vec::new();
    let mut out: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-o" => {
                i += 1;
                if i >= args.len() {
                    return Err("-o requires a path argument".into());
                }
                if out.is_some() {
                    return Err("duplicate -o".into());
                }
                out = Some(args[i].clone());
            }
            s if s.starts_with('-') => return Err(format!("unknown flag {:?}", s)),
            s => files.push(s.to_string()),
        }
        i += 1;
    }
    Ok((files, out))
}

fn read_json(path: &str) -> Result<Value, (String, i32)> {
    let text = fs::read_to_string(path).map_err(|e| (format!("{}: {}", path, e), 2))?;
    // tolerate an optional UTF-8 BOM on JSON reads
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    serde_json::from_str(text).map_err(|e| (format!("{}: invalid JSON: {}", path, e), 1))
}

fn cmd_gen(args: &[String]) -> ExitCode {
    let (files, out) = match parse_io_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tblgen gen: {}\n\n{}", e, USAGE);
            return ExitCode::from(2);
        }
    };
    if files.len() != 1 {
        eprintln!("tblgen gen: expected exactly 1 spec file\n\n{}", USAGE);
        return ExitCode::from(2);
    }
    let spec = match read_json(&files[0]) {
        Ok(v) => v,
        Err((msg, code)) => {
            eprintln!("tblgen: {}", msg);
            return ExitCode::from(code as u8);
        }
    };
    let (tbl, diags) = generate_with_diagnostics(&spec);
    // every diagnostic goes to stderr as JSON; only errors stop generation
    for d in &diags {
        eprintln!("{}", d.to_json());
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return ExitCode::from(1);
    }
    match tbl {
        Some(tbl) => match write_out(out.as_deref(), &tbl) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("tblgen: {}", msg);
                ExitCode::from(2)
            }
        },
        None => ExitCode::from(1),
    }
}

fn cmd_decompile(args: &[String]) -> ExitCode {
    let (files, out) = match parse_io_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tblgen decompile: {}\n\n{}", e, USAGE);
            return ExitCode::from(2);
        }
    };
    if files.len() != 1 {
        eprintln!(
            "tblgen decompile: expected exactly 1 .tbl file\n\n{}",
            USAGE
        );
        return ExitCode::from(2);
    }
    let tbl = match read_json(&files[0]) {
        Ok(v) => v,
        Err((msg, code)) => {
            eprintln!("tblgen: {}", msg);
            return ExitCode::from(code as u8);
        }
    };
    match decompile(&tbl) {
        Ok(spec) => match write_out(out.as_deref(), &spec) {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("tblgen: {}", msg);
                ExitCode::from(2)
            }
        },
        Err(d) => {
            eprintln!("{}", d.to_json());
            ExitCode::from(1)
        }
    }
}

enum CheckTarget {
    File(String),
    DiscoveryError { path: PathBuf, error: io::Error },
}

fn is_excluded(path: &Path, exclude: &[PathBuf]) -> bool {
    if exclude.is_empty() {
        return false;
    }
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    exclude.iter().any(|x| abs.starts_with(x))
}

fn directory_entries(
    dir: &Path,
    entries: impl Iterator<Item = io::Result<fs::DirEntry>>,
    out: &mut Vec<CheckTarget>,
) -> Vec<(PathBuf, io::Result<fs::FileType>)> {
    let mut named = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => named.push((entry.path(), entry.file_type())),
            Err(error) => errors.push(error),
        }
    }
    // An iterator error carries no entry name. Report it against this
    // directory, before its named entries, in a stable order.
    errors.sort_by_cached_key(|error| error.to_string());
    out.extend(errors.into_iter().map(|error| CheckTarget::DiscoveryError {
        path: dir.to_path_buf(),
        error,
    }));
    named.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    named
}

fn entry_is_dir(path: &Path, file_type: fs::FileType) -> io::Result<bool> {
    if !file_type.is_symlink() {
        return Ok(file_type.is_dir());
    }
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        // Keep dangling links eligible for the same suffix check as files.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Expand directories in sorted depth-first order, retaining discovery errors.
/// Hidden directories and node_modules are skipped when found during traversal.
fn expand_dirs(args: &[String], exclude: &[PathBuf]) -> Vec<CheckTarget> {
    fn walk(dir: &Path, exclude: &[PathBuf], out: &mut Vec<CheckTarget>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                out.push(CheckTarget::DiscoveryError {
                    path: dir.to_path_buf(),
                    error,
                });
                return;
            }
        };
        for (path, file_type) in directory_entries(dir, entries, out) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            match file_type.and_then(|ty| entry_is_dir(&path, ty)) {
                Ok(true) => {
                    if !name.starts_with('.')
                        && name != "node_modules"
                        && !is_excluded(&path, exclude)
                    {
                        walk(&path, exclude, out);
                    }
                }
                Ok(false) => {
                    if (name.ends_with(".tbl") || name.ends_with(".action.ts"))
                        && !is_excluded(&path, exclude)
                    {
                        out.push(CheckTarget::File(path.display().to_string()));
                    }
                }
                Err(error) => {
                    if !is_excluded(&path, exclude) {
                        out.push(CheckTarget::DiscoveryError { path, error });
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    for a in args {
        let p = Path::new(a);
        if is_excluded(p, exclude) {
            continue;
        }
        if p.is_dir() {
            walk(p, exclude, &mut out);
        } else {
            out.push(CheckTarget::File(a.clone()));
        }
    }
    out
}

/// A check or discovery outcome plus whether the failure was an I/O error
/// (exit-code 2), kept in input order for replay.
enum FileOutcome {
    Ok(CheckReport),
    Err { report: CheckReport, io_error: bool },
}

impl FileOutcome {
    fn check_error(file: String, message: String, io_error: bool) -> Self {
        Self::Err {
            report: CheckReport {
                file,
                scope: "unknown".into(),
                errors: vec![Diag::spec("", "io", message)],
                warnings: vec![],
            },
            io_error,
        }
    }
}

/// Validate every expanded file and retain discovery errors. Checks are
/// independent, so `jobs > 1` uses a bounded pool. Outcomes are replayed in
/// input order, keeping stdout, stderr and exit codes identical to serial.
fn run_checks(targets: &[CheckTarget], jobs: usize) -> Vec<FileOutcome> {
    let n = targets.len();
    let workers = jobs.max(1).min(n.max(1));
    let mut outcomes: Vec<Option<FileOutcome>> = (0..n).map(|_| None).collect();
    if workers <= 1 {
        for (i, target) in targets.iter().enumerate() {
            outcomes[i] = Some(check_one(target));
        }
    } else {
        let next = AtomicUsize::new(0);
        let (tx, rx) = mpsc::channel::<(usize, FileOutcome)>();
        std::thread::scope(|s| {
            for _ in 0..workers {
                let tx = tx.clone();
                let next = &next;
                s.spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    // a send failure means the receiver is gone (panic path);
                    // dropping the outcome is fine, the scope re-panics
                    let _ = tx.send((i, check_one(&targets[i])));
                });
            }
            drop(tx);
            for (i, outcome) in rx {
                outcomes[i] = Some(outcome);
            }
        });
    }
    outcomes.into_iter().map(|o| o.unwrap()).collect()
}

fn check_one(target: &CheckTarget) -> FileOutcome {
    match target {
        CheckTarget::File(file) => match check_file(Path::new(file)) {
            Ok(report) => FileOutcome::Ok(report),
            Err((message, code)) => FileOutcome::check_error(file.clone(), message, code == 2),
        },
        CheckTarget::DiscoveryError { path, error } => {
            let file = path.display().to_string();
            let message = format!("{}: {}", file, error);
            FileOutcome::check_error(file, message, true)
        }
    }
}

#[cfg(test)]
mod check_discovery_tests {
    use super::*;

    #[test]
    fn directory_iteration_keeps_named_entries_and_reports_unnamed_errors() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let entry = fs::read_dir(&dir).unwrap().next().unwrap().unwrap();
        let entry_path = entry.path();
        let entries = [
            Err(io::Error::other("entries unavailable")),
            Ok(entry),
            Err(io::Error::other("directory entry unavailable")),
        ];
        let mut targets = Vec::new();
        let named = directory_entries(&dir, entries.into_iter(), &mut targets);

        assert_eq!(named.len(), 1);
        assert_eq!(named[0].0, entry_path);
        assert!(named[0].1.is_ok());
        assert_eq!(targets.len(), 2);
        for (target, message) in targets
            .iter()
            .zip(["directory entry unavailable", "entries unavailable"])
        {
            let FileOutcome::Err {
                report,
                io_error: true,
            } = check_one(target)
            else {
                panic!("expected a directory I/O outcome");
            };
            assert_eq!(report.file, dir.display().to_string());
            assert_eq!(report.errors[0].rule, "io");
            assert_eq!(
                report.errors[0].message,
                format!("{}: {}", dir.display(), message)
            );
        }
    }
}

fn parse_jobs_value(v: &str) -> Result<usize, String> {
    let n = v
        .parse::<usize>()
        .map_err(|_| format!("--jobs: invalid value {:?}", v))?;
    if n == 0 {
        return Err("--jobs: must be >= 1".into());
    }
    Ok(n)
}

/// `--jobs N`: returns the remaining args and worker count (default:
/// available parallelism capped at 4; the caller caps it by target count).
/// An explicit N is honored as given, including values above 4.
fn parse_jobs(args: &[String]) -> Result<(Vec<String>, usize), String> {
    let mut jobs: Option<usize> = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--jobs" {
            let v = args.get(i + 1).ok_or("--jobs requires a value")?;
            if jobs.is_some() {
                return Err("duplicate --jobs".into());
            }
            jobs = Some(parse_jobs_value(v)?);
            i += 2;
        } else if let Some(v) = a.strip_prefix("--jobs=") {
            if jobs.is_some() {
                return Err("duplicate --jobs".into());
            }
            jobs = Some(parse_jobs_value(v)?);
            i += 1;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    let jobs = jobs.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get().min(4))
            .unwrap_or(1)
    });
    Ok((rest, jobs))
}

fn cmd_check(args: &[String]) -> ExitCode {
    // --jobs N (checked first so it is accepted anywhere in the arg list)
    let (args, jobs) = match parse_jobs(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tblgen check: {}\n\n{}", e, USAGE);
            return ExitCode::from(2);
        }
    };
    // --exclude <path> (repeatable) skips files under that path
    let mut exclude: Vec<PathBuf> = Vec::new();
    let mut kept: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--exclude" {
            let Some(x) = args.get(i + 1) else {
                eprintln!("tblgen check: --exclude requires a path\n\n{}", USAGE);
                return ExitCode::from(2);
            };
            match fs::canonicalize(x) {
                Ok(p) => exclude.push(p),
                Err(e) => {
                    eprintln!("tblgen check: --exclude {}: {}", x, e);
                    return ExitCode::from(2);
                }
            }
            i += 2;
        } else {
            kept.push(args[i].clone());
            i += 1;
        }
    }
    let args = &kept[..];
    let (files, _) = match parse_io_args(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("tblgen check: {}\n\n{}", e, USAGE);
            return ExitCode::from(2);
        }
    };
    if files.is_empty() {
        eprintln!("tblgen check: expected at least 1 file\n\n{}", USAGE);
        return ExitCode::from(2);
    }
    let mut reports: Vec<CheckReport> = Vec::new();
    let mut had_io_error = false;
    // Replay file and discovery diagnostics in input order.
    for outcome in run_checks(&expand_dirs(&files, &exclude), jobs) {
        match outcome {
            FileOutcome::Ok(r) => reports.push(r),
            FileOutcome::Err { report, io_error } => {
                eprintln!("tblgen: {}", report.errors[0].message);
                had_io_error |= io_error;
                reports.push(report);
            }
        }
    }
    // stdout: machine-readable summary
    let out = json!({
        "files": reports.iter().map(|r| json!({
            "file": r.file,
            "scope": r.scope,
            "errors": r.errors.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
            "warnings": r.warnings.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()
    });
    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    if had_io_error {
        return ExitCode::from(2);
    }
    if reports.iter().any(|r| !r.errors.is_empty()) {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_schema(args: &[String]) -> ExitCode {
    if !args.is_empty() {
        eprintln!(
            "tblgen schema: unexpected arguments {:?}\n\n{}",
            args, USAGE
        );
        return ExitCode::from(2);
    }
    let doc = schema_document();
    match serde_json::to_string_pretty(&doc) {
        Ok(s) => {
            println!("{}", s);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tblgen: schema serialize: {}", e);
            ExitCode::from(2)
        }
    }
}

/// `new script|dsl`: build a starter spec, generate it, write the .tbl (and
/// for script models the script skeleton). Nothing is written if any target
/// exists or the spec has errors.
fn cmd_new(args: &[String]) -> ExitCode {
    match run_new(args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("tblgen new: {}\n\n{}", msg, USAGE);
            ExitCode::from(2)
        }
    }
}

fn run_new(args: &[String]) -> Result<ExitCode, String> {
    use std::collections::HashMap;
    use tblgen::new::*;
    use tblgen::project;

    if args.iter().any(|a| a == "--help" || a == "-h") {
        eprintln!("{}", USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let kind = args.first().map(String::as_str).ok_or("expected script or dsl")?;
    if kind != "script" && kind != "dsl" {
        return Err(format!("unknown model kind {:?}; expected script or dsl", kind));
    }
    const VALUED: [&str; 16] = [
        "--table", "--schema", "--datasource", "--fields", "--extract", "-o", "--version",
        "--script-name", "--script-out", "--template", "--cn", "--note", "--spec-out",
        "--input", "--join-on", "--join-type",
    ];
    let mut opts: HashMap<&str, Vec<String>> = HashMap::new();
    let mut no_script = false;
    let mut i = 1;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--no-script" {
            no_script = true;
        } else if let Some(k) = VALUED.iter().find(|k| **k == a) {
            let v = args.get(i + 1).ok_or(format!("{} requires a value", a))?;
            let entry = opts.entry(k).or_default();
            if !entry.is_empty() && *k != "--input" {
                return Err(format!("duplicate {}", a));
            }
            entry.push(v.clone());
            i += 1;
        } else {
            return Err(format!("unknown argument {:?}", a));
        }
        i += 1;
    }
    // reject options that the other model kind would silently ignore
    let script_only = ["--script-name", "--script-out", "--template", "--cn", "--note"];
    let dsl_only = ["--input", "--join-on", "--join-type"];
    let foreign: &[&str] = if kind == "script" { &dsl_only } else { &script_only };
    if let Some(k) = foreign.iter().find(|k| opts.contains_key(*k)) {
        return Err(format!("{} does not apply to new {}", k, kind));
    }
    if kind == "dsl" && no_script {
        return Err("--no-script does not apply to new dsl".into());
    }
    let one = |k: &str| opts.get(k).and_then(|v| v.first()).cloned();
    let need = |k: &str| one(k).ok_or(format!("{} is required", k));

    let out = PathBuf::from(need("-o")?);
    let extract = match need("--extract")?.as_str() {
        "true" => true,
        "false" => false,
        v => return Err(format!("--extract must be true (ETL, writes dbTableName) or false (query logic), got {:?}", v)),
    };
    let common = Common {
        table: need("--table")?,
        schema: one("--schema"),
        datasource: one("--datasource"),
        extract,
        version: one("--version"),
    };
    // normalize the primary target before ANY filesystem mutation:
    // symlink/case aliases in existing parents are resolved so duplicate
    // and existence checks below compare canonical paths
    let out =
        project::normalized_new_path(&out).map_err(|e| format!("{}: {}", out.display(), e))?;
    // locate WITHOUT creating anything: an existing project resolves
    // normally; a not-yet-created <P>/data/... path resolves prospectively
    let located = project::locate_new(&out);

    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    let spec = if kind == "script" {
        let fields = parse_fields(&need("--fields")?, false)?;
        let script_name = one("--script-name").unwrap_or_else(|| md5_hex(common.table.as_bytes()));
        if !no_script {
            let target = match (one("--script-out"), &located) {
                (Some(p), _) => PathBuf::from(p),
                (None, Some(at)) => project::script_candidates(
                    &at.projects_root,
                    project::project_name(&at.platform_path),
                    &script_name,
                )[1]
                .clone(),
                (None, None) => {
                    return Err("the output is not inside a project (no data/ directory above it); \
                                pass --script-out or --no-script".into())
                }
            };
            let template = one("--template").unwrap_or_else(|| "placeholder".into());
            if template != "placeholder" && template != "transform" {
                return Err(format!("--template must be placeholder or transform, got {:?}", template));
            }
            let cn = one("--cn").unwrap_or_else(|| common.table.clone());
            let note = one("--note").unwrap_or_else(|| "占位，待数据源或口径确认".into());
            writes.push((target, script_source(template == "transform", &cn, &common.table, &note, &fields)));
        }
        script_spec(&common, &fields, &script_name)
    } else {
        let refs = opts.get("--input").cloned().unwrap_or_default();
        let mut inputs = Vec::new();
        for r in &refs {
            let disk = match &located {
                Some(at) => {
                    let p = project::resolve_reference(&at.platform_path, r)
                        .ok_or(format!("cannot resolve --input {:?}", r))?;
                    project::to_disk(&at.projects_root, &p)
                }
                None if r.starts_with('$') => {
                    return Err(format!("--input {:?} needs the output inside a project layout", r))
                }
                None => out.parent().unwrap_or(Path::new(".")).join(r),
            };
            inputs.push(read_input(r, &disk)?);
        }
        let fields = one("--fields").map(|f| parse_fields(&f, true)).transpose()?;
        let join_type = one("--join-type").unwrap_or_else(|| "LeftJoin".into());
        dsl_spec(&common, &inputs, fields.as_deref(), one("--join-on").as_deref(), &join_type)?
    };

    let (tbl, diags) = generate_with_diagnostics(&spec);
    for d in &diags {
        eprintln!("{}", d.to_json());
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return Ok(ExitCode::from(1));
    }
    let Some(tbl) = tbl else {
        return Ok(ExitCode::from(1));
    };
    writes.insert(0, (out, json_text(&tbl)?));
    if let Some(p) = one("--spec-out") {
        writes.push((PathBuf::from(p), json_text(&spec)?));
    }
    // normalize every target, then preflight ALL of them before any write:
    // duplicate normalized targets and existing files (including dangling
    // symlinks, which `exists()` misses) reject the whole command
    for (p, _) in writes.iter_mut() {
        *p = project::normalized_new_path(p).map_err(|e| format!("{}: {}", p.display(), e))?;
    }
    {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for (p, _) in &writes {
            if !seen.insert(p.clone()) {
                eprintln!("tblgen: duplicate output target {}", p.display());
                return Ok(ExitCode::from(2));
            }
        }
    }
    for (p, _) in &writes {
        if fs::symlink_metadata(p).is_ok() {
            eprintln!("tblgen: {}: already exists (use a fresh path)", p.display());
            return Ok(ExitCode::from(2));
        }
    }
    for (p, _) in &writes {
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
        }
    }
    write_exclusive_all(&writes)?;
    Ok(ExitCode::SUCCESS)
}
