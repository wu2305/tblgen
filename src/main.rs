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
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::{json, Value};

use tblgen::check::{check_file, CheckReport};
use tblgen::decompile::decompile;
use tblgen::gen::{generate, validate_spec};
use tblgen::schema::schema_document;
use tblgen::Diag;

const USAGE: &str = "tblgen — offline .tbl <-> spec-format v1 converter

USAGE:
    tblgen gen <spec.json> [-o out.tbl]
    tblgen decompile <file.tbl> [-o out.json]
    tblgen check <file|dir>...
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
new: --extract true = ETL into --table; false = query logic. Field list is
name:TYPE[:length[.decimal]][:m] (TYPE C/N/I/D/P, m = measure); dsl takes
names only. --input is the reference as written in the model (../ods/x.tbl,
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

fn write_out(path: Option<&str>, value: &Value) -> Result<(), (String, i32)> {
    let text = serde_json::to_string_pretty(value).map_err(|e| (format!("serialize: {}", e), 2))?;
    match path {
        Some(p) => {
            // create_new: fail if the target already exists (no overwrite)
            match fs::OpenOptions::new().write(true).create_new(true).open(p) {
                Ok(mut f) => {
                    f.write_all(text.as_bytes())
                        .and_then(|_| f.write_all(b"\n"))
                        .map_err(|e| (format!("{}: {}", p, e), 2))?;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    return Err((format!("{}: already exists (use a fresh path)", p), 2));
                }
                Err(e) => return Err((format!("{}: {}", p, e), 2)),
            }
            eprintln!("tblgen: wrote {}", p);
        }
        None => {
            println!("{}", text);
        }
    }
    Ok(())
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
    let diags = validate_spec(&spec);
    // every diagnostic goes to stderr as JSON; only errors stop generation
    for d in &diags {
        eprintln!("{}", d.to_json());
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return ExitCode::from(1);
    }
    match generate(&spec) {
        Ok(tbl) => match write_out(out.as_deref(), &tbl) {
            Ok(()) => ExitCode::SUCCESS,
            Err((msg, code)) => {
                eprintln!("tblgen: {}", msg);
                ExitCode::from(code as u8)
            }
        },
        Err(d) => {
            eprintln!("{}", d.to_json());
            ExitCode::from(1)
        }
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
            Err((msg, code)) => {
                eprintln!("tblgen: {}", msg);
                ExitCode::from(code as u8)
            }
        },
        Err(d) => {
            eprintln!("{}", d.to_json());
            ExitCode::from(1)
        }
    }
}

/// Directories expand to every `.tbl` and `.action.ts` below them, sorted;
/// hidden directories and node_modules are skipped.
fn expand_dirs(args: &[String]) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            if p.is_dir() {
                if !name.starts_with('.') && name != "node_modules" {
                    walk(&p, out);
                }
            } else if name.ends_with(".tbl") || name.ends_with(".action.ts") {
                out.push(p.display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    for a in args {
        let p = Path::new(a);
        if p.is_dir() {
            walk(p, &mut out);
        } else {
            out.push(a.clone());
        }
    }
    out
}

fn cmd_check(args: &[String]) -> ExitCode {
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
    for f in expand_dirs(&files) {
        match check_file(Path::new(&f)) {
            Ok(r) => reports.push(r),
            Err((msg, code)) => {
                eprintln!("tblgen: {}", msg);
                if code == 2 {
                    had_io_error = true;
                }
                reports.push(CheckReport {
                    file: f.clone(),
                    scope: "unknown".into(),
                    errors: vec![Diag::spec("", "io", msg)],
                    warnings: vec![],
                });
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
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
    }
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

    let diags = validate_spec(&spec);
    for d in &diags {
        eprintln!("{}", d.to_json());
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return Ok(ExitCode::from(1));
    }
    let tbl = generate(&spec).map_err(|d| d.to_json().to_string())?;
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default() + "\n";
    writes.insert(0, (out, pretty(&tbl)));
    if let Some(p) = one("--spec-out") {
        writes.push((PathBuf::from(p), pretty(&spec)));
    }
    if let Some((p, _)) = writes.iter().find(|(p, _)| p.exists()) {
        eprintln!("tblgen: {}: already exists (use a fresh path)", p.display());
        return Ok(ExitCode::from(2));
    }
    for (p, text) in &writes {
        if let Some(parent) = p.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
        }
        fs::write(p, text).map_err(|e| format!("{}: {}", p.display(), e))?;
        eprintln!("tblgen: wrote {}", p.display());
    }
    Ok(ExitCode::SUCCESS)
}
