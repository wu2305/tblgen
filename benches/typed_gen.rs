//! Typed generated-form stage benchmark.
//!
//! Times `generate_with_diagnostics_profile` on parsed `format: 1` / `nodes`
//! specs. JSON is read and parsed once per case before warmup; every timed
//! iteration reuses the same `Value`.

use std::env;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::io::{self, Write};
use std::time::Instant;

use serde_json::{json, Map, Value};
use tblgen::gen::{generate_with_diagnostics, generate_with_diagnostics_profile};
use tblgen::Diag;

const MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

const STAGE_KEYS: [&str; 10] = [
    "validate_ns",
    "spec_validation_ns",
    "node_entry_validation_ns",
    "prepare_ns",
    "topo_sort_ns",
    "graph_semantics_ns",
    "field_compute_ns",
    "wire_emit_ns",
    "wire_check_ns",
    "diag_remap_ns",
];

#[derive(Clone)]
struct Case {
    name: &'static str,
    path: String,
    repeats: u32,
    warmup: u32,
}

fn default_cases() -> Vec<Case> {
    let fixture =
        |name: &str| format!("{MANIFEST_DIR}/benches/fixtures/typed-gen/{name}.spec.json");
    vec![
        Case {
            name: "small",
            path: fixture("small"),
            repeats: 300,
            warmup: 5,
        },
        Case {
            name: "experiment",
            path: format!("{MANIFEST_DIR}/tests/fixtures/experiment.spec.json"),
            repeats: 80,
            warmup: 3,
        },
        Case {
            name: "wide",
            path: fixture("wide"),
            repeats: 40,
            warmup: 3,
        },
        Case {
            name: "deep",
            path: fixture("deep"),
            repeats: 40,
            warmup: 3,
        },
        Case {
            name: "fan-in",
            path: fixture("fan-in"),
            repeats: 40,
            warmup: 3,
        },
        Case {
            name: "fan-out",
            path: fixture("fan-out"),
            repeats: 40,
            warmup: 3,
        },
        Case {
            name: "union-map",
            path: fixture("union-map"),
            repeats: 30,
            warmup: 3,
        },
        Case {
            name: "error-dense",
            path: fixture("error-dense"),
            repeats: 80,
            warmup: 3,
        },
        Case {
            name: "select-heavy",
            path: fixture("select-heavy"),
            repeats: 60,
            warmup: 3,
        },
        Case {
            name: "select-rawstep",
            path: fixture("select-rawstep"),
            repeats: 300,
            warmup: 5,
        },
    ]
}

fn read_json(path: &str) -> Result<(Value, String), Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    let value = serde_json::from_str(text.trim_start_matches('\u{feff}'))?;
    Ok((value, text))
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

fn fingerprint(bytes: &[u8]) -> String {
    format!("fnv1a64:{:016x}", fnv1a64(bytes))
}

#[derive(Clone, Copy)]
struct Sample {
    wall_ns: u64,
    stages: [u64; 10],
}

impl Sample {
    fn get(&self, key: &str) -> u64 {
        if key == "wall_ns" {
            return self.wall_ns;
        }
        let idx = STAGE_KEYS.iter().position(|k| *k == key).unwrap();
        self.stages[idx]
    }
}

fn sample_json(sample: &Sample) -> Value {
    let mut obj = Map::new();
    obj.insert("wall_ns".into(), json!(sample.wall_ns));
    for (i, key) in STAGE_KEYS.iter().enumerate() {
        obj.insert((*key).into(), json!(sample.stages[i]));
    }
    Value::Object(obj)
}

fn median(mut values: Vec<u64>) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        values[mid] as f64
    } else {
        (values[mid - 1] as f64 + values[mid] as f64) / 2.0
    }
}

fn summarize(samples: &[Sample]) -> Value {
    let mut keys = vec!["wall_ns".to_string()];
    keys.extend(STAGE_KEYS.iter().map(|s| s.to_string()));
    let mut obj = Map::new();
    for key in keys {
        let values: Vec<u64> = samples.iter().map(|s| s.get(&key)).collect();
        let sum: u128 = values.iter().map(|v| u128::from(*v)).sum();
        obj.insert(
            key,
            json!({
                "median_ns": median(values.clone()),
                "mean_ns": sum as f64 / values.len() as f64,
                "min_ns": values.iter().copied().min().unwrap_or(0),
                "max_ns": values.iter().copied().max().unwrap_or(0),
            }),
        );
    }
    Value::Object(obj)
}

fn profile_sample(profile: tblgen::gen::GeneratedCompileProfile, wall_ns: u64) -> Sample {
    Sample {
        wall_ns,
        stages: [
            profile.validate_ns as u64,
            profile.spec_validation_ns as u64,
            profile.node_entry_validation_ns as u64,
            profile.prepare_ns as u64,
            profile.topo_sort_ns as u64,
            profile.graph_semantics_ns as u64,
            profile.field_compute_ns as u64,
            profile.wire_emit_ns as u64,
            profile.wire_check_ns as u64,
            profile.diag_remap_ns as u64,
        ],
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut repeats_override: Option<u32> = None;
    let mut warmup_override: Option<u32> = None;
    let mut out_path: Option<String> = None;
    let mut case_filter: Option<String> = None;
    let mut args = env::args().skip(1).filter(|a| a != "--bench");
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--repeats" => {
                repeats_override = Some(args.next().ok_or("--repeats requires N")?.parse()?)
            }
            "--warmup" => {
                warmup_override = Some(args.next().ok_or("--warmup requires N")?.parse()?)
            }
            "--out" => out_path = Some(args.next().ok_or("--out requires PATH")?),
            "--cases" => case_filter = Some(args.next().ok_or("--cases requires LIST")?),
            other => return Err(format!("unknown argument {other:?}").into()),
        }
    }

    let mut cases = default_cases();
    if let Some(filter) = case_filter {
        let wanted: Vec<&str> = filter
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        cases.retain(|c| wanted.contains(&c.name));
        if cases.is_empty() {
            return Err(format!("no cases matched {filter:?}").into());
        }
    }
    if let Some(n) = repeats_override {
        for c in &mut cases {
            c.repeats = n;
        }
    }
    if let Some(n) = warmup_override {
        for c in &mut cases {
            c.warmup = n;
        }
    }

    let mut report = Map::new();
    report.insert("bench".into(), json!("typed_gen"));
    report.insert(
        "protocol".into(),
        json!({
            "input": "parsed once per case; timed iterations reuse the same Value",
            "timing": "per-iteration generate_with_diagnostics_profile; wall_ns wraps the call, stage ns are cumulative inside compile_generated",
            "stage_note": "prepare_ns includes topo_sort_ns; validate_ns includes the validation sub-stages; build stages are field_compute_ns then wire_emit_ns",
            "fingerprint": "fnv1a64 over UTF-8 input bytes; not a cryptographic digest",
        }),
    );
    let mut case_reports = Vec::new();

    for case in cases {
        let (spec, input_text) = read_json(&case.path)?;

        // Equivalence: the profile entry point must return the same wire value
        // and diagnostics as the public generator before any timing sample.
        let normal = generate_with_diagnostics(&spec);
        let profiled = generate_with_diagnostics_profile(&spec);
        let normal_wire = serde_json::to_string(&normal.0)?;
        let profile_wire = serde_json::to_string(&profiled.0)?;
        let normal_diags =
            serde_json::to_string(&normal.1.iter().map(Diag::to_json).collect::<Vec<_>>())?;
        let profile_diags =
            serde_json::to_string(&profiled.1.iter().map(Diag::to_json).collect::<Vec<_>>())?;
        if normal_wire != profile_wire || normal_diags != profile_diags {
            return Err(format!("profile path diverged for {}", case.name).into());
        }
        let wire_fingerprint = fingerprint(profile_wire.as_bytes());
        let diag_fingerprint = fingerprint(profile_diags.as_bytes());
        let diag_count = profiled.1.len();
        let counts = profiled.2;
        drop(normal);
        drop(profiled);
        drop(normal_wire);
        drop(profile_wire);
        drop(normal_diags);
        drop(profile_diags);

        for _ in 0..case.warmup {
            let result = generate_with_diagnostics_profile(black_box(&spec));
            black_box(result);
        }

        let mut samples = Vec::with_capacity(case.repeats as usize);
        for _ in 0..case.repeats {
            let start = Instant::now();
            let (wire, diags, profile) = generate_with_diagnostics_profile(black_box(&spec));
            let wall_ns = start.elapsed().as_nanos() as u64;
            black_box((&wire, &diags));
            drop((wire, diags));
            samples.push(profile_sample(profile, wall_ns));
        }

        case_reports.push(json!({
            "name": case.name,
            "path": case.path,
            "input_bytes": input_text.len(),
            "input_fingerprint": fingerprint(input_text.as_bytes()),
            "repeats": case.repeats,
            "warmup": case.warmup,
            "equivalence": {
                "profile_matches_normal": true,
                "wire_fingerprint": wire_fingerprint,
                "diag_count": diag_count,
                "diag_fingerprint": diag_fingerprint,
            },
            "shape": {
                "nodes": counts.nodes,
                "fields": counts.fields,
                "steps": counts.steps,
                "input_edges": counts.input_edges,
                "wire_diagnostics": counts.wire_diagnostics,
                "mapped_diagnostics": counts.mapped_diagnostics,
            },
            "summary": summarize(&samples),
            "samples": samples.iter().map(sample_json).collect::<Vec<_>>(),
        }));
    }

    report.insert("cases".into(), Value::Array(case_reports));
    let mut out = io::BufWriter::new(io::stdout().lock());
    if let Some(path) = out_path {
        fs::write(
            &path,
            serde_json::to_string_pretty(&Value::Object(report))? + "\n",
        )?;
        eprintln!("typed_gen: wrote {path}");
    } else {
        serde_json::to_writer_pretty(&mut out, &Value::Object(report))?;
        writeln!(out)?;
    }
    out.flush()?;
    Ok(())
}
