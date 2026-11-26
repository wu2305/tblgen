//! Compare the public wire checker on identical parsed documents, without timing JSON I/O.
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::io::{self, Write};
use std::time::Instant;

use serde_json::{json, Value};
use tblgen::check;

fn read_json(path: &str) -> Result<Value, Box<dyn Error>> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(text.trim_start_matches('\u{feff}'))?)
}

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    Ordered,
    Reordered,
    Missing,
}

fn wide_table(width: usize, layout: Layout) -> Value {
    let fields: Vec<_> = (0..width)
        .map(|i| {
            let name = match (layout, i % 4) {
                (Layout::Ordered, _) => format!("F{i}"),
                (_, 0) => format!("F{i}"),
                (_, 1) => format!("Straße{i}"),
                (_, 2) => format!("金额{i}"),
                _ => format!("mixed_Case_identifier_{i}"),
            };
            json!({"name":name, "dataType":"C", "isDimension":layout == Layout::Ordered || i % 3 != 0})
        })
        .collect();
    let linked: Vec<_> = fields
        .iter()
        .cloned()
        .map(|mut f| {
            f["inputField"] = f["name"].clone();
            f
        })
        .collect();
    let mut tbl = json!({"version":"4.19.0","properties":{"modelDataType":"DataFlow","extractDataEnabled":false},
    "dimensions":linked,"dataFlow":{"nodes":{
        "src":{"type":"ModelTable","id":"src","alias":"src","fields":fields,"moduleTablePath":"m.tbl"},
        "sel":{"type":"Select","id":"sel","alias":"sel","inputNodes":["src"],"fields":linked},
        "default":{"type":"Output","id":"default","alias":"out","inputNodes":["sel"],"fields":linked}
    }}});
    if layout != Layout::Ordered {
        let mut dims = Vec::new();
        let mut measures = Vec::new();
        for (i, f) in linked.iter().enumerate().rev() {
            if layout == Layout::Missing && i % 7 == 0 {
                continue;
            }
            let mut entry = f.clone();
            if i % 4 == 1 {
                entry["name"] = json!(f["name"].as_str().unwrap().to_uppercase());
            }
            if f["isDimension"] == true {
                dims.push(entry);
            } else {
                measures.push(entry);
            }
        }
        tbl["dimensions"] = json!([{"children":dims}]);
        tbl["measures"] = json!([{"children":measures}]);
    }
    tbl
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args()
        .skip(1)
        .filter(|a| a != "--bench")
        .collect();
    let mode = args.first().map(String::as_str).unwrap_or("time");
    let source = args.get(1).map(String::as_str).unwrap_or("width:32");
    let repeats: u32 = args.get(2).map_or(Ok(1), |s| s.parse())?;
    if !matches!(mode, "time" | "diagnostics") || repeats == 0 || args.len() > 3 {
        return Err(
            "usage: wire_check [time|diagnostics] [FILE|width:N|reordered:N|missing:N|manifest:JSON] [repeats]".into(),
        );
    }
    let paths: Vec<String> = if let Some(path) = source.strip_prefix("manifest:") {
        serde_json::from_value(read_json(path)?)?
    } else {
        vec![source.into()]
    };
    if paths.is_empty() {
        return Err("empty input manifest".into());
    }
    let mut elapsed_ns = 0;
    let mut out = io::BufWriter::new(io::stdout().lock());
    for path in &paths {
        let doc = match path.split_once(':') {
            Some(("width", n)) => wide_table(n.parse()?, Layout::Ordered),
            Some(("reordered", n)) => wide_table(n.parse()?, Layout::Reordered),
            Some(("missing", n)) => wide_table(n.parse()?, Layout::Missing),
            _ => read_json(path)?,
        };
        if mode == "diagnostics" {
            let diags: Vec<_> = check(&doc).iter().map(|d| d.to_json()).collect();
            serde_json::to_writer(&mut out, &json!([path, diags]))?;
            writeln!(out)?;
        } else {
            // Parse one document at a time. Retaining the corpus would distort peak RSS.
            let start = Instant::now();
            for _ in 0..repeats {
                drop(black_box(check(black_box(&doc))));
            }
            elapsed_ns += start.elapsed().as_nanos();
        }
    }
    if mode == "time" {
        writeln!(
            out,
            "{}",
            json!({"documents":paths.len(),"repeats":repeats,"elapsed_ns":elapsed_ns})
        )?;
    }
    out.flush()?;
    Ok(())
}
