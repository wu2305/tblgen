//! Segmented CLI timing harness for `tblgen check`.
//!
//! `report` reproduces the CLI's stdout/stderr/exit contract. `time` writes the
//! same report to stdout and one JSON timing record to stderr.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use serde_json::{json, Value};
use tblgen::check::{check_references, CheckReport};
use tblgen::project;
use tblgen::Diag;

struct CountingAlloc;
static ALLOCATED_BYTES: AtomicUsize = AtomicUsize::new(0);
static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            let size = layout.size();
            ALLOCATED_BYTES.fetch_add(size, Ordering::Relaxed);
            let live = LIVE_BYTES.fetch_add(size, Ordering::Relaxed) + size;
            PEAK_BYTES.fetch_max(live, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[cfg(target_os = "linux")]
fn cpu_ns() -> u64 {
    use std::os::raw::{c_int, c_long};

    #[repr(C)]
    struct Timeval {
        tv_sec: c_long,
        tv_usec: c_long,
    }
    #[repr(C)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        rest: [c_long; 14],
    }
    extern "C" {
        fn getrusage(who: c_int, usage: *mut Rusage) -> c_int;
    }

    let mut usage = Rusage {
        utime: Timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        stime: Timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        rest: [0; 14],
    };
    unsafe {
        getrusage(0, &mut usage);
    }
    let secs = (usage.utime.tv_sec + usage.stime.tv_sec) as u128;
    let usecs = (usage.utime.tv_usec + usage.stime.tv_usec) as u128;
    (secs * 1_000_000_000 + usecs * 1_000) as u64
}

#[cfg(not(target_os = "linux"))]
fn cpu_ns() -> u64 {
    0
}

#[derive(Clone, Copy, Default)]
struct Meter {
    wall_ns: u64,
    cpu_ns: u64,
    alloc_bytes: u64,
}

fn measure<T>(f: impl FnOnce() -> T) -> (T, Meter) {
    let wall_start = Instant::now();
    let cpu_start = cpu_ns();
    let alloc_start = ALLOCATED_BYTES.load(Ordering::Relaxed);
    let value = f();
    let wall_ns = wall_start.elapsed().as_nanos() as u64;
    let cpu_ns = cpu_ns().saturating_sub(cpu_start);
    let alloc_bytes = ALLOCATED_BYTES
        .load(Ordering::Relaxed)
        .saturating_sub(alloc_start);
    (
        value,
        Meter {
            wall_ns,
            cpu_ns,
            alloc_bytes: alloc_bytes as u64,
        },
    )
}

const FILE_PHASE_COUNT: usize = 9;
const FILE_PHASES: [&str; FILE_PHASE_COUNT] = [
    "read_ns",
    "json_parse_ns",
    "route_ns",
    "locate_ns",
    "pure_check_ns",
    "spec_check_ns",
    "script_check_ns",
    "references_ns",
    "split_ns",
];
const READ: usize = 0;
const JSON_PARSE: usize = 1;
const ROUTE: usize = 2;
const LOCATE: usize = 3;
const PURE_CHECK: usize = 4;
const SPEC_CHECK: usize = 5;
const SCRIPT_CHECK: usize = 6;
const REFERENCES: usize = 7;
const SPLIT: usize = 8;

#[derive(Clone, Copy, Default)]
struct Acc {
    wall_ns: u64,
    cpu_ns: u64,
    alloc_bytes: u64,
}

impl Acc {
    fn add(&mut self, meter: Meter) {
        self.wall_ns += meter.wall_ns;
        self.cpu_ns += meter.cpu_ns;
        self.alloc_bytes += meter.alloc_bytes;
    }
}

#[derive(Default)]
struct FileMeasurement {
    kind: &'static str,
    scope: String,
    errors: usize,
    warnings: usize,
    read_bytes: usize,
    meters: [Meter; FILE_PHASE_COUNT],
}

impl FileMeasurement {
    fn new(kind: &'static str) -> Self {
        FileMeasurement {
            kind,
            scope: String::new(),
            errors: 0,
            warnings: 0,
            read_bytes: 0,
            meters: [Meter::default(); FILE_PHASE_COUNT],
        }
    }
}

struct KindAcc {
    files: usize,
    errors: usize,
    warnings: usize,
    read_bytes: usize,
    phases: BTreeMap<&'static str, Acc>,
}

#[derive(Default)]
struct BatchStats {
    phases: BTreeMap<&'static str, Acc>,
    by_kind: BTreeMap<&'static str, KindAcc>,
    files: Vec<FileRecord>,
    output_bytes: usize,
    alloc_total_bytes: usize,
    alloc_live_bytes_end: usize,
    alloc_peak_bytes: usize,
}

#[derive(Clone)]
struct FileRecord {
    file: String,
    kind: &'static str,
    scope: String,
    errors: usize,
    warnings: usize,
    read_bytes: usize,
    wall_ns: u64,
    cpu_ns: u64,
    alloc_bytes: u64,
    phase_wall_ns: [u64; FILE_PHASE_COUNT],
}

impl BatchStats {
    fn new() -> Self {
        let mut phases = BTreeMap::new();
        phases.insert("enumerate_ns", Acc::default());
        for phase in FILE_PHASES {
            phases.insert(phase, Acc::default());
        }
        for phase in ["report_build_ns", "report_serialize_ns", "report_write_ns"] {
            phases.insert(phase, Acc::default());
        }
        BatchStats {
            phases,
            by_kind: BTreeMap::new(),
            files: Vec::new(),
            output_bytes: 0,
            alloc_total_bytes: 0,
            alloc_live_bytes_end: 0,
            alloc_peak_bytes: 0,
        }
    }

    fn add_phase(&mut self, name: &'static str, meter: Meter) {
        if let Some(acc) = self.phases.get_mut(name) {
            acc.add(meter);
        }
    }

    fn add_file(&mut self, file: String, meas: FileMeasurement) {
        let mut wall_ns = 0;
        let mut cpu_ns = 0;
        let mut alloc_bytes = 0;
        let mut phase_wall_ns = [0u64; FILE_PHASE_COUNT];
        for (i, meter) in meas.meters.iter().enumerate() {
            wall_ns += meter.wall_ns;
            cpu_ns += meter.cpu_ns;
            alloc_bytes += meter.alloc_bytes;
            phase_wall_ns[i] = meter.wall_ns;
            self.add_phase(FILE_PHASES[i], *meter);
        }

        let kind_acc = self.by_kind.entry(meas.kind).or_insert_with(|| KindAcc {
            files: 0,
            errors: 0,
            warnings: 0,
            read_bytes: 0,
            phases: FILE_PHASES.iter().map(|p| (*p, Acc::default())).collect(),
        });
        kind_acc.files += 1;
        kind_acc.errors += meas.errors;
        kind_acc.warnings += meas.warnings;
        kind_acc.read_bytes += meas.read_bytes;
        for (i, meter) in meas.meters.iter().enumerate() {
            if let Some(acc) = kind_acc.phases.get_mut(FILE_PHASES[i]) {
                acc.add(*meter);
            }
        }
        self.files.push(FileRecord {
            file,
            kind: meas.kind,
            scope: meas.scope.clone(),
            errors: meas.errors,
            warnings: meas.warnings,
            read_bytes: meas.read_bytes,
            wall_ns,
            cpu_ns,
            alloc_bytes,
            phase_wall_ns,
        });
    }

    fn finish_alloc(&mut self) {
        self.alloc_total_bytes = ALLOCATED_BYTES.load(Ordering::Relaxed);
        self.alloc_live_bytes_end = LIVE_BYTES.load(Ordering::Relaxed);
        self.alloc_peak_bytes = PEAK_BYTES.load(Ordering::Relaxed);
    }

    fn to_json(&self) -> Value {
        let phases = self
            .phases
            .iter()
            .fold(serde_json::Map::new(), |mut map, (name, acc)| {
                map.insert(
                    (*name).to_string(),
                    json!({
                        "wall_ns": acc.wall_ns,
                        "cpu_ns": acc.cpu_ns,
                        "alloc_bytes": acc.alloc_bytes,
                    }),
                );
                map
            });
        let by_kind = self
            .by_kind
            .iter()
            .fold(serde_json::Map::new(), |mut map, (kind, acc)| {
                let phases = acc.phases.iter().fold(
                    serde_json::Map::new(),
                    |mut phase_map, (name, phase)| {
                        phase_map.insert(
                            (*name).to_string(),
                            json!({
                                "wall_ns": phase.wall_ns,
                                "cpu_ns": phase.cpu_ns,
                                "alloc_bytes": phase.alloc_bytes,
                            }),
                        );
                        phase_map
                    },
                );
                map.insert(
                    (*kind).to_string(),
                    json!({
                        "files": acc.files,
                        "errors": acc.errors,
                        "warnings": acc.warnings,
                        "read_bytes": acc.read_bytes,
                        "phases": Value::Object(phases),
                    }),
                );
                map
            });
        let mut top = self.files.clone();
        top.sort_by(|a, b| b.wall_ns.cmp(&a.wall_ns).then_with(|| a.file.cmp(&b.file)));
        let top_files = top
            .into_iter()
            .take(20)
            .map(|f| {
                let phases = FILE_PHASES
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| f.phase_wall_ns[*i] > 0)
                    .map(|(i, name)| (*name, f.phase_wall_ns[i]))
                    .collect::<BTreeMap<_, _>>();
                json!({
                    "file": f.file,
                    "kind": f.kind,
                    "scope": f.scope,
                    "errors": f.errors,
                    "warnings": f.warnings,
                    "read_bytes": f.read_bytes,
                    "wall_ns": f.wall_ns,
                    "cpu_ns": f.cpu_ns,
                    "alloc_bytes": f.alloc_bytes,
                    "phase_wall_ns": phases,
                })
            })
            .collect::<Vec<_>>();
        let files = self.files.len();
        let errors = self.by_kind.values().map(|k| k.errors).sum::<usize>();
        let warnings = self.by_kind.values().map(|k| k.warnings).sum::<usize>();
        let read_bytes = self.by_kind.values().map(|k| k.read_bytes).sum::<usize>();
        json!({
            "totals": {
                "files": files,
                "errors": errors,
                "warnings": warnings,
                "read_bytes": read_bytes,
                "output_bytes": self.output_bytes,
                "alloc_total_bytes": self.alloc_total_bytes,
                "alloc_live_bytes_end": self.alloc_live_bytes_end,
                "alloc_peak_bytes": self.alloc_peak_bytes,
            },
            "phases": Value::Object(phases),
            "by_kind": Value::Object(by_kind),
            "top_files": top_files,
        })
    }
}

fn expand_dirs(args: &[String]) -> Vec<String> {
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
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
    out.retain(|f| {
        let _abs = fs::canonicalize(f).unwrap_or_else(|_| PathBuf::from(f));
        true
    });
    out
}

fn build_report_value(reports: &[CheckReport]) -> Value {
    json!({
        "files": reports.iter().map(|r| json!({
            "file": r.file,
            "scope": r.scope,
            "errors": r.errors.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
            "warnings": r.warnings.iter().map(|d| d.to_json()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>()
    })
}

fn check_file_timed(path: &Path) -> (Result<CheckReport, (String, i32)>, FileMeasurement) {
    let file = path.display().to_string();
    let (read_result, read_meter) = measure(|| fs::read_to_string(path));
    let mut meas = FileMeasurement::new("io_error");
    meas.meters[READ] = read_meter;
    let text = match read_result {
        Ok(text) => text,
        Err(e) => {
            meas.errors = 1;
            return (Err((format!("{}: {}", file, e), 2)), meas);
        }
    };
    meas.read_bytes = text.len();

    if file.ends_with(".action.ts") {
        meas.kind = "script";
        let (at, locate_meter) = measure(|| {
            project::locate(path)
                .map(|l| l.platform_path)
                .unwrap_or_else(|| file.clone())
        });
        meas.meters[LOCATE] = locate_meter;
        let (errors, script_meter) = measure(|| tblgen::script::check_script(&at, &text));
        meas.meters[SCRIPT_CHECK] = script_meter;
        meas.errors = errors.len();
        meas.scope = "script".into();
        return (
            Ok(CheckReport {
                file,
                scope: "script".into(),
                errors,
                warnings: Vec::new(),
            }),
            meas,
        );
    }

    let text_ref = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let (parse_result, parse_meter) = measure(|| serde_json::from_str::<Value>(text_ref));
    meas.meters[JSON_PARSE] = parse_meter;
    let v: Value = match parse_result {
        Ok(v) => v,
        Err(e) => {
            meas.kind = "invalid_json";
            meas.errors = 1;
            return (Err((format!("{}: invalid JSON: {}", file, e), 1)), meas);
        }
    };

    if !v.is_object() {
        meas.kind = "invalid";
        meas.errors = 1;
        meas.scope = "invalid".into();
        let mut errors = Vec::new();
        errors.push(Diag::wire(
            String::new(),
            "structure.root",
            "",
            "document root must be a JSON object",
        ));
        return (
            Ok(CheckReport {
                file,
                scope: "invalid".into(),
                errors,
                warnings: Vec::new(),
            }),
            meas,
        );
    }

    let ((kind, is_spec), route_meter) = measure(|| {
        let is_spec = v.get("format").and_then(Value::as_u64) == Some(1)
            && (v.get("nodes").is_some()
                || v.get("archive").is_some()
                || v.get("rawTable").is_some());
        let declares_flow = v
            .pointer("/properties/modelDataType")
            .and_then(Value::as_str)
            == Some("DataFlow");
        let kind = if is_spec {
            "spec"
        } else if v.get("dataFlow").is_some() || declares_flow {
            "dataflow"
        } else {
            "skipped"
        };
        (kind, is_spec)
    });
    meas.meters[ROUTE] = route_meter;
    meas.kind = kind;
    meas.scope = kind.into();

    let diags: Vec<Diag> = if is_spec {
        let (d, meter) = measure(|| tblgen::gen::validate_spec(&v));
        meas.meters[SPEC_CHECK] = meter;
        d
    } else if kind == "dataflow" {
        let (mut d, meter) = measure(|| tblgen::check::check(&v));
        meas.meters[PURE_CHECK] = meter;
        let (located, locate_meter) = measure(|| project::locate(path));
        meas.meters[LOCATE] = locate_meter;
        if let Some(at) = located {
            let (refs, ref_meter) = measure(|| check_references(&v, &at));
            meas.meters[REFERENCES] = ref_meter;
            d.extend(refs);
        }
        d
    } else {
        Vec::new()
    };

    let ((errors, warnings), split_meter) = measure(|| {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        for d in diags {
            if d.is_warning() {
                warnings.push(d);
            } else {
                errors.push(d);
            }
        }
        (errors, warnings)
    });
    meas.meters[SPLIT] = split_meter;
    meas.errors = errors.len();
    meas.warnings = warnings.len();
    (
        Ok(CheckReport {
            file,
            scope: meas.scope.clone(),
            errors,
            warnings,
        }),
        meas,
    )
}

fn run_batch(args: &[String]) -> (ExitCode, BatchStats) {
    let mut stats = BatchStats::new();
    let (files, enumerate_meter) = measure(|| expand_dirs(args));
    stats.add_phase("enumerate_ns", enumerate_meter);

    let mut reports: Vec<CheckReport> = Vec::new();
    let mut had_io_error = false;
    for f in files {
        let (result, meas) = check_file_timed(Path::new(&f));
        stats.add_file(f.clone(), meas);
        match result {
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

    let (out, report_build_meter) = measure(|| build_report_value(&reports));
    stats.add_phase("report_build_ns", report_build_meter);
    let (text, report_serialize_meter) =
        measure(|| serde_json::to_string_pretty(&out).unwrap_or_default());
    stats.add_phase("report_serialize_ns", report_serialize_meter);
    let output_bytes = text.len() + 1;
    let ((), report_write_meter) = measure(|| {
        let mut stdout = io::stdout().lock();
        let _ = stdout.write_all(text.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    });
    stats.add_phase("report_write_ns", report_write_meter);
    stats.output_bytes = output_bytes;
    stats.finish_alloc();

    if had_io_error {
        (ExitCode::from(2), stats)
    } else if reports.iter().any(|r| !r.errors.is_empty()) {
        (ExitCode::from(1), stats)
    } else {
        (ExitCode::SUCCESS, stats)
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(mode) = args.first().map(String::as_str) else {
        eprintln!("usage: cli_timing [report|time] <file|dir>...");
        return ExitCode::from(2);
    };
    if mode != "report" && mode != "time" {
        eprintln!("usage: cli_timing [report|time] <file|dir>...");
        return ExitCode::from(2);
    }
    let paths = &args[1..];
    if paths.is_empty() {
        eprintln!("usage: cli_timing [report|time] <file|dir>...");
        return ExitCode::from(2);
    }
    let (code, stats) = run_batch(paths);
    if mode == "time" {
        let stats_json = serde_json::to_string(&stats.to_json()).unwrap_or_default();
        eprintln!("TBLGEN_CLI_TIMING_JSON={}", stats_json);
    }
    code
}
