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
use std::path::Path;
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
    tblgen check <file.tbl|spec.json>...
    tblgen schema

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
    if !diags.is_empty() {
        for d in &diags {
            eprintln!("{}", d.to_json());
        }
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
    for f in files {
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
