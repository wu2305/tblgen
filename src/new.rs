//! `new`: build a starter spec for a script model or a declarative
//! (ModelTable → [Join] → Select → Output) model.
//!
//! Model properties start from the designer's new-file defaults
//! (`dwbuilder.ts#newFile`); the run mode (`extractDataEnabled`) is always an
//! explicit choice. The spec goes through the regular `gen` pipeline.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::model::{field_eq, SAVE_VERSION};

pub const FIELD_TYPES: [&str; 5] = ["C", "N", "I", "D", "P"];

#[derive(Debug, Clone)]
pub enum FieldReq {
    /// `name[=dbfield]` only: take the definition from an upstream model;
    /// `dbfield` overrides just the physical column on THIS model's output
    Inherit {
        name: String,
        dbfield: Option<String>,
    },
    Explicit(Value),
}

impl FieldReq {
    pub fn name(&self) -> &str {
        match self {
            FieldReq::Inherit { name, .. } => name,
            FieldReq::Explicit(v) => v["name"].as_str().unwrap_or(""),
        }
    }
}

/// `dbfield` must be an ASCII identifier (checked before upper-casing).
fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Parse the field shorthand `name[=dbfield]:TYPE[:length[.decimal]][:m]`,
/// comma separated. `name` is the display name; `dbfield` (the physical
/// column, upper-cased) defaults to the name and must be an ASCII identifier. TYPE is C/N/I/D/P; length defaults to C=100, N=18, else 0; an N
/// measure without decimals gets 2; a trailing `m` marks a measure.
/// With `allow_inherit`, a bare `name` or `name=dbfield` takes its
/// definition from upstream (dbfield overrides only the output column).
pub fn parse_fields(s: &str, allow_inherit: bool) -> Result<Vec<FieldReq>, String> {
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let seg: Vec<&str> = part.split(':').map(str::trim).collect();
        if seg.len() == 1 && allow_inherit {
            let (name, dbfield) = match seg[0].split_once('=') {
                Some((n, d)) => {
                    let d = d.trim();
                    if !ident(d) {
                        return Err(format!(
                            "bad field {:?}: dbfield {:?} must be an ASCII identifier",
                            part, d
                        ));
                    }
                    (n.trim().to_string(), Some(d.to_uppercase()))
                }
                None => (seg[0].to_string(), None),
            };
            if name.is_empty() {
                return Err(format!("bad field {:?}: empty field name", part));
            }
            out.push(FieldReq::Inherit { name, dbfield });
            continue;
        }
        let ty = seg.get(1).map(|t| t.to_uppercase()).unwrap_or_default();
        if seg.len() < 2 || !FIELD_TYPES.contains(&ty.as_str()) || seg[0].is_empty() {
            return Err(format!("bad field {:?}: expected name[=dbfield]:TYPE[:length[.decimal]][:m]", part));
        }
        // `显示名=dbfield`: the name users see, and the physical column
        let (name, dbfield) = match seg[0].split_once('=') {
            Some((n, d)) => (n.trim().to_string(), d.trim().to_uppercase()),
            None => (seg[0].to_string(), seg[0].to_uppercase()),
        };
        if name.is_empty() || !ident(&dbfield) {
            return Err(format!(
                "bad field {:?}: dbfield {:?} must be an ASCII identifier; write 显示名=dbfield, e.g. 统计年月=stat_month:C:7",
                part, dbfield
            ));
        }
        let mut length: i64 = match ty.as_str() {
            "C" => 100,
            "N" => 18,
            _ => 0,
        };
        let mut decimal: Option<i64> = None;
        let mut is_dim = true;
        for extra in &seg[2..] {
            if *extra == "m" {
                is_dim = false;
            } else if let Some((l, d)) = extra.split_once('.') {
                length = l.parse().map_err(|_| format!("bad length in {:?}", part))?;
                decimal = Some(d.parse().map_err(|_| format!("bad decimal in {:?}", part))?);
            } else {
                length = extra.parse().map_err(|_| format!("bad length in {:?}", part))?;
            }
        }
        if ty == "N" && decimal.is_none() && !is_dim {
            decimal = Some(2);
        }
        let mut f = json!({
            "name": name, "dbfield": dbfield, "dataType": ty,
            "length": length, "isDimension": is_dim,
        });
        if let Some(d) = decimal {
            f["decimal"] = json!(d);
        }
        out.push(FieldReq::Explicit(f));
    }
    if out.is_empty() {
        return Err("at least one field is required".into());
    }
    Ok(out)
}

pub struct Common {
    pub table: String,
    pub schema: Option<String>,
    pub datasource: Option<String>,
    pub extract: bool,
    pub version: Option<String>,
}

fn properties(c: &Common) -> Value {
    let mut p = json!({
        // dwbuilder.ts#newFile defaults
        "datasource": c.datasource.clone().unwrap_or_else(|| "defdw".into()),
        "rollbackEnabled": false,
        "rebuildIndexAndKey": true,
        "modifyDbTableEnabled": ["field", "index", "constraint"],
        "dbTableName": c.table,
        "extractDataEnabled": c.extract,
    });
    if let Some(s) = &c.schema {
        p["dbSchema"] = json!(s);
    }
    p
}

fn wrap(c: &Common, nodes: Vec<Value>) -> Value {
    // version always emitted: explicit --version wins, else the platform
    // save version the generator would default to anyway
    json!({
        "format": 1,
        "version": c.version.clone().unwrap_or_else(|| SAVE_VERSION.to_string()),
        "properties": properties(c),
        "nodes": nodes,
    })
}

/// Script model: a Script node (fields declared here, data from the script)
/// feeding the default Output.
pub fn script_spec(c: &Common, fields: &[FieldReq], script_name: &str) -> Value {
    let fields: Vec<Value> = fields
        .iter()
        .filter_map(|f| match f {
            FieldReq::Explicit(v) => Some(v.clone()),
            FieldReq::Inherit { .. } => None,
        })
        .collect();
    wrap(
        c,
        vec![
            json!({"key": "script", "type": "Script", "alias": "脚本",
                   "config": {"scriptName": script_name}, "fields": fields}),
            json!({"key": "out", "type": "Output", "alias": "模型输出", "inputs": ["script"]}),
        ],
    )
}

pub struct Input {
    /// reference as written in the model, e.g. `../ods/x.tbl` or `$DATA:/ods/x.tbl`
    pub reference: String,
    /// node alias (upstream file stem)
    pub alias: String,
    /// upstream `dimensions` + `measures`
    pub fields: Vec<Value>,
}

/// Read an upstream model's fields for a ModelTable input. The top-level
/// `dimensions`/`measures` partitions are authoritative; `children` of
/// folder/hierarchy entries hold the real leaf definitions (pure folders
/// and reference-only children carry no `dataType` and are skipped). When
/// the default Output's `fields` list is a complete duplicate-free
/// permutation of the collected names, leaf order follows it (the saved
/// display order); otherwise partition order is kept. Numeric-string
/// `length`/`decimal` values are normalized to numbers.
pub fn read_input(reference: &str, disk: &Path) -> Result<Input, String> {
    let text = std::fs::read_to_string(disk).map_err(|e| format!("{}: {}", disk.display(), e))?;
    let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{}: invalid JSON: {}", disk.display(), e))?;
    let keep = ["name", "dbfield", "dataType", "length", "decimal", "isDimension", "desc"];
    let mut fields = Vec::new();
    for k in ["dimensions", "measures"] {
        for f in v.get(k).and_then(Value::as_array).into_iter().flatten() {
            collect_leaf(f, k == "dimensions", &keep, &mut fields);
        }
    }
    // reorder by the saved Output field order when it is a clean
    // permutation — a stale cache must never add/drop fields
    if let Some(order) = output_field_order(&v) {
        if let Some(pos) = permutation_positions(&fields, &order) {
            fields.sort_by_key(|f| {
                pos.get(f["name"].as_str().unwrap_or("").to_uppercase().as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }
    }
    let alias = disk
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "input".into());
    Ok(Input { reference: reference.to_string(), alias, fields })
}

/// Emit the kept definition keys of a leaf field; `length`/`decimal`
/// arrive as numeric strings on real saved files and are normalized.
fn field_def(f: &Value, is_dim: bool, keep: &[&str]) -> Option<Value> {
    // a real definition has both name and dataType; reference-only
    // children (name only) and pure folders are skipped
    if f.get("name").and_then(Value::as_str).is_none() || f.get("dataType").is_none() {
        return None;
    }
    let mut m = Map::new();
    for key in keep {
        if let Some(x) = f.get(key) {
            m.insert(key.to_string(), x.clone());
        }
    }
    for key in ["length", "decimal"] {
        if let Some(s) = m.get(key).and_then(Value::as_str) {
            if let Ok(n) = s.trim().parse::<i64>() {
                m.insert(key.to_string(), json!(n));
            }
        }
    }
    m.entry("isDimension".to_string()).or_insert(json!(is_dim));
    Some(Value::Object(m))
}

/// Collect leaf field definitions, descending into folder/hierarchy
/// `children`. A container itself is NEVER a field — even when it carries
/// stray name/dataType keys, only its children are read.
fn collect_leaf(f: &Value, is_dim: bool, keep: &[&str], out: &mut Vec<Value>) {
    let container = f.get("isFolder").and_then(Value::as_bool) == Some(true)
        || f.get("isHierarchy").and_then(Value::as_bool) == Some(true);
    if container {
        for c in f
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            collect_leaf(c, is_dim, keep, out);
        }
        return;
    }
    if let Some(d) = field_def(f, is_dim, keep) {
        out.push(d);
    }
}

/// Saved display order of the default Output's fields (`dataFlow.nodes.
/// .default.fields`, falling back to a root `fields` array).
fn output_field_order(v: &Value) -> Option<Vec<String>> {
    let arr = v
        .pointer("/dataFlow/nodes/default/fields")
        .or_else(|| v.get("fields"))?
        .as_array()?;
    Some(
        arr.iter()
            .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
            .collect(),
    )
}

/// When `order` is a complete duplicate-free permutation of the collected
/// field names (folded), return each field's position in `order`.
fn permutation_positions(
    fields: &[Value],
    order: &[String],
) -> Option<std::collections::HashMap<String, usize>> {
    if order.len() != fields.len() {
        return None;
    }
    let mut pos = std::collections::HashMap::new();
    for (i, n) in order.iter().enumerate() {
        if pos.insert(n.to_uppercase(), i).is_some() {
            return None; // duplicate in the saved order
        }
    }
    // the collected names must be duplicate-free too — a folded-name
    // collision makes any positional mapping ambiguous
    let mut seen = std::collections::HashSet::new();
    for f in fields {
        let n = f["name"].as_str().unwrap_or("").to_uppercase();
        if !seen.insert(n.clone()) || !pos.contains_key(&n) {
            return None;
        }
    }
    Some(pos)
}

/// Declarative model: ModelTable input(s) → (Join) → Select → Output. Fields
/// are named only (`name` or `name=dbfield`); each comes from the first
/// input that has it, with its upstream definition. Join fields carry only
/// `name` (+ `inputNode` when several inputs match); a `dbfield` override
/// materializes as explicit Output fields with the upstream definition.
pub fn dsl_spec(
    c: &Common,
    inputs: &[Input],
    fields: Option<&[FieldReq]>,
    join_on: Option<&str>,
    join_type: &str,
) -> Result<Value, String> {
    if inputs.is_empty() || inputs.len() > 2 {
        return Err("--input must be given once or twice".into());
    }
    let all: Vec<FieldReq>;
    let fields = match fields {
        Some(f) => f,
        None if inputs.len() == 1 => {
            all = inputs[0]
                .fields
                .iter()
                .map(|f| FieldReq::Inherit {
                    name: f["name"].as_str().unwrap_or("").to_string(),
                    dbfield: None,
                })
                .collect();
            &all
        }
        None => return Err("--fields is required with two inputs".into()),
    };
    // (input, upstream def, dbfield override) per selected field, in order
    let mut selected: Vec<(&Input, &Value, Option<String>)> = Vec::new();
    for req in fields {
        let first = inputs.iter().find_map(|i| {
            i.fields
                .iter()
                .find(|f| f["name"].as_str().is_some_and(|n| field_eq(n, req.name())))
                .map(|f| (i, f))
        });
        let Some((input, upstream)) = first else {
            return Err(format!("field {:?} is not in any input model", req.name()));
        };
        let dbfield = match req {
            FieldReq::Inherit { dbfield, .. } => dbfield.clone(),
            FieldReq::Explicit(_) => {
                return Err(format!(
                    "field {:?}: declarative models take field types from the input; give names only (name=dbfield overrides the output column)",
                    req.name()
                ));
            }
        };
        selected.push((input, upstream, dbfield));
    }
    let keep: Vec<String> = selected
        .iter()
        .map(|(_, f, _)| f["name"].as_str().unwrap_or("").to_string())
        .collect();
    let mut nodes: Vec<Value> = inputs
        .iter()
        .map(|i| {
            json!({"key": i.alias, "type": "ModelTable", "alias": i.alias,
                   "config": {"moduleTablePath": i.reference, "devQueryFixedRows": true,
                              "devFixedRowCount": 100000},
                   "fields": i.fields})
        })
        .collect();
    // Output fields exist only when a dbfield override was requested:
    // upstream definitions in the chosen order, override applied there
    let output_fields = selected.iter().any(|(_, _, d)| d.is_some()).then(|| {
        selected
            .iter()
            .map(|(_, f, d)| {
                let mut m = f.as_object().cloned().unwrap_or_default();
                if let Some(dv) = d {
                    m.insert("dbfield".into(), json!(dv));
                }
                Value::Object(m)
            })
            .collect::<Vec<_>>()
    });
    let mut out_node = json!({"key": "out", "type": "Output", "alias": "模型输出"});
    if let Some(of) = output_fields {
        out_node["fields"] = json!(of);
    }
    let head = if inputs.len() == 2 {
        let on = join_on.ok_or("two inputs need --join-on, e.g. key or left.KEY=right.KEY")?;
        let (l, r) = (&inputs[0].alias, &inputs[1].alias);
        let (le, re) = match on.split_once('=') {
            Some((a, b)) => (a.trim().to_string(), b.trim().to_string()),
            None => (format!("{}.{}", l, on.trim()), format!("{}.{}", r, on.trim())),
        };
        let qualify = |e: String, default_alias: &str| match e.rsplit_once('.') {
            Some((a, f)) => (a.to_string(), f.to_string()),
            None => (default_alias.to_string(), e),
        };
        let (la, lf) = qualify(le, l);
        let (ra, rf) = qualify(re, r);
        // name-only Join field refs; inputNode only when several inputs
        // carry the same field name (Unicode-aware fold)
        let jfields: Vec<Value> = selected
            .iter()
            .map(|(input, f, _)| {
                let uname = f["name"].as_str().unwrap_or("");
                let ambiguous = inputs
                    .iter()
                    .filter(|i| {
                        i.fields
                            .iter()
                            .any(|g| g["name"].as_str().is_some_and(|n| field_eq(n, uname)))
                    })
                    .count()
                    > 1;
                let mut jf = json!({"name": uname});
                if ambiguous {
                    jf["inputNode"] = json!(input.alias);
                }
                jf
            })
            .collect();
        nodes.push(json!({
            "key": "join", "type": "Join", "alias": "关联", "inputs": [l, r],
            "config": {"joinConditions": [{
                "joinType": join_type, "leftTable": la, "rightTable": ra,
                "clauses": [{"leftExp": format!("[{}].[{}]", la, lf), "operator": "=",
                             "rightExp": format!("[{}].[{}]", ra, rf)}]
            }]},
            "fields": jfields,
        }));
        nodes.push(json!({"key": "select", "type": "Select", "alias": "列加工",
                          "inputs": ["join"]}));
        "select"
    } else {
        // a Select's fields follow its input; unselected fields are dropped
        // with a RemoveField step (omitted entirely when nothing is dropped)
        let drop: Vec<&str> = inputs[0]
            .fields
            .iter()
            .filter_map(|f| f["name"].as_str())
            .filter(|n| !keep.iter().any(|k| field_eq(k, n)))
            .collect();
        let mut sel = json!({"key": "select", "type": "Select", "alias": "列加工",
                             "inputs": [inputs[0].alias]});
        if !drop.is_empty() {
            sel["steps"] = json!([{ "type": "RemoveField", "fields": drop }]);
        }
        nodes.push(sel);
        "select"
    };
    out_node["inputs"] = json!([head]);
    nodes.push(out_node);
    Ok(wrap(c, nodes))
}

/// Field list as TypeScript `DbFieldInfo` literals for script skeletons.
pub fn fields_ts(fields: &[FieldReq]) -> String {
    fields
        .iter()
        .filter_map(|f| match f {
            FieldReq::Explicit(v) => Some(v),
            FieldReq::Inherit { .. } => None,
        })
        .map(|f| {
            let dec = f.get("decimal").map(|d| format!(", decimal: {}", d)).unwrap_or_default();
            format!(
                "  {{ name: \"{}\", dataType: FieldDataType.{}, length: {}{}, dbfield: \"{}\", isDimension: {} }},",
                f["name"].as_str().unwrap_or(""),
                f["dataType"].as_str().unwrap_or(""),
                f["length"],
                dec,
                f["dbfield"].as_str().unwrap_or(""),
                f["isDimension"]
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

const PLACEHOLDER_TMPL: &str = r#"/**
 * {cn}（占位）
 *
 * 物理表：{physical}
 * 状态：{note}
 * 约定：数据源确认后按既有脚本规范补齐抽取（配置写顶部常量、不读 context.params、
 *       设计器预览按 context.isPreview 截断）；正式运行在未接入状态下显式报错。
 */

let fieldsInfo: DbFieldInfo[] = [
{fields}
];

/**
 * 返回脚本节点字段结构。
 */
function onProcessFields(context: IDataFlowScriptNodeContext): DbFieldInfo[] {
  return fieldsInfo;
}

/**
 * 占位加工：预览返回字段结构与零行，正式运行显式报错等待接入。
 */
function onProcessData(context: IDataFlowScriptNodeContext): DbTableInfo | any[][] {
  if (context.isPreview) {
    return null;
  }
  throw new Error("占位模型未接入：{physical} 数据来源确认后补齐加工");
}
"#;

const TRANSFORM_TMPL: &str = r#"/**
 * {cn}
 *
 * 物理表：{physical}
 * 输入：上游模型连到本脚本节点后流式读取；未连线时预览返回字段结构、正式运行显式报错。
 * 约定：配置写顶部常量、不读 context.params、设计器预览按 PREVIEW_MAX_ROWS 截断写出。
 */

let fieldsInfo: DbFieldInfo[] = [
{fields}
];

// 设计器预览时最多写出的行数，正式运行不截断
const PREVIEW_MAX_ROWS = 200;

function onProcessFields(context: IDataFlowScriptNodeContext): DbFieldInfo[] {
  return fieldsInfo;
}

function fieldIndex(input: FlowScriptNodeInputData): { [dbfield: string]: number } {
  let fields = input.getFields();
  let idx: { [dbfield: string]: number } = {};
  for (let i = 0; i < fields.length; i++) {
    idx[fields[i].dbfield] = i;
  }
  return idx;
}

function cell(row: any[], idx: { [dbfield: string]: number }, dbfield: string): any {
  let i = idx[dbfield];
  return i === undefined || row === null || row === undefined ? null : row[i];
}

function str(value: any): string {
  return value === null || value === undefined ? "" : String(value);
}

/**
 * 转换加工：逐行读上游输入，按字段顺序写出。在此补齐映射与计算逻辑。
 */
function onProcessData(context: IDataFlowScriptNodeContext): DbTableInfo | any[][] {
  let output = context.getOutput();
  if (context.getInputCount() === 0) {
    if (context.isPreview) {
      return null;
    }
    throw new Error("{physical} 未连线：请将上游模型连到本脚本节点");
  }
  let input = context.getInput(0);
  let idx = fieldIndex(input);
  let maxRows = context.isPreview ? PREVIEW_MAX_ROWS : -1;
  let written = 0;
  while (input.hasNext()) {
    let row = input.nextRow();
    // TODO：按目标字段逐个取值计算后写出，示例：
    // output.writeRow([str(cell(row, idx, "SRC_FIELD"))]);
    written++;
    if (maxRows > 0 && written >= maxRows) {
      console.info("预览模式：已写出 " + written + " 行，停止继续写出");
      break;
    }
  }
  console.info("{physical} 写出 " + written + " 行");
  return null;
}
"#;

/// Script skeleton. `transform` reads one upstream input; otherwise a
/// placeholder that previews the field structure and fails at run time.
pub fn script_source(transform: bool, cn: &str, physical: &str, note: &str, fields: &[FieldReq]) -> String {
    let t = if transform { TRANSFORM_TMPL } else { PLACEHOLDER_TMPL };
    t.replace("{cn}", cn)
        .replace("{physical}", physical)
        .replace("{note}", note)
        .replace("{fields}", &fields_ts(fields))
}

/// Lowercase hex MD5 (RFC 1321), used for deterministic 32-hex script names.
pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());
    let (mut a0, mut b0, mut c0, mut d0) = (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk.chunks(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0].iter().flat_map(|w| w.to_le_bytes()).map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_reference() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"ods_demo"), "83273f6cc051e0131565a2508ce53779");
        assert_eq!(md5_hex("中文".as_bytes()), "a7bac2239fcdcb3a067903d8077c4a07");
    }

    #[test]
    fn field_shorthand() {
        let f = parse_fields("m:C:7, amt:N:18.4:m, n:N:m, d:D", false).unwrap();
        let v: Vec<Value> = f.into_iter().map(|x| match x { FieldReq::Explicit(v) => v, _ => panic!() }).collect();
        assert_eq!(v[0]["length"], json!(7));
        assert_eq!((v[1]["decimal"].clone(), v[1]["isDimension"].clone()), (json!(4), json!(false)));
        assert_eq!((v[2]["length"].clone(), v[2]["decimal"].clone()), (json!(18), json!(2)));
        assert_eq!(v[3]["length"], json!(0));
        assert!(parse_fields("x", false).is_err());
        let v = match &parse_fields("统计年月=stat_month:C:7", false).unwrap()[0] {
            FieldReq::Explicit(v) => v.clone(),
            _ => panic!(),
        };
        assert_eq!((v["name"].clone(), v["dbfield"].clone()), (json!("统计年月"), json!("STAT_MONTH")));
        assert!(parse_fields("统计年月:C:7", false).is_err(), "Chinese name needs an explicit dbfield");
        assert!(parse_fields("a=1x:C", false).is_err());
        assert!(matches!(
            parse_fields("x", true).unwrap()[0],
            FieldReq::Inherit { .. }
        ));
    }

    #[test]
    fn inherit_name_dbfield() {
        // bare name and name=dbfield inherit the upstream definition; the
        // dbfield override is validated as an ASCII ident BEFORE upper-casing
        let f = parse_fields("车辆VIN=vin, plain, n==d", true);
        assert!(f.is_err(), "double '=' must be rejected: {:?}", f);
        let f = parse_fields("车辆VIN=vin, plain", true).unwrap();
        match &f[0] {
            FieldReq::Inherit { name, dbfield } => {
                assert_eq!(name, "车辆VIN");
                assert_eq!(dbfield.as_deref(), Some("VIN"));
            }
            _ => panic!("expected Inherit"),
        }
        match &f[1] {
            FieldReq::Inherit { name, dbfield } => {
                assert_eq!(name, "plain");
                assert_eq!(*dbfield, None);
            }
            _ => panic!("expected Inherit"),
        }
        // typed syntax is unchanged: a TYPE segment still parses Explicit
        let f2 = parse_fields("x:C", true).unwrap();
        assert!(matches!(f2[0], FieldReq::Explicit(_)));
        // non-ASCII dbfield override rejected before upper-casing
        assert!(parse_fields("x=列:C", true).is_err() || parse_fields("x=列", true).is_err());
    }

    fn input(alias: &str, fields: Vec<Value>) -> Input {
        Input {
            reference: format!("../{}/x.tbl", alias),
            alias: alias.into(),
            fields,
        }
    }

    #[test]
    fn dsl_spec_dbfield_override_and_join_ambiguity() {
        let c = Common {
            table: "t".into(),
            schema: None,
            datasource: None,
            extract: false,
            version: None,
        };
        // upstream keeps its own dbfield; the override lands only on the
        // model's Output fields
        let vin = json!({"name":"车辆VIN","dbfield":"VIN_RAW","dataType":"C",
                         "length":100,"isDimension":true});
        let other = json!({"name":"other","dbfield":"OTHER","dataType":"C",
                           "length":100,"isDimension":true});
        let spec = dsl_spec(
            &c,
            &[input("up", vec![vin.clone(), other.clone()])],
            Some(&parse_fields("车辆VIN=vin", true).unwrap()),
            None,
            "LeftJoin",
        )
        .unwrap();
        assert_eq!(spec["version"], json!(SAVE_VERSION), "default version");
        let nodes = spec["nodes"].as_array().unwrap();
        assert_eq!(
            nodes[0]["fields"][0]["dbfield"],
            json!("VIN_RAW"),
            "upstream ModelTable definition must stay untouched"
        );
        let out = nodes.iter().find(|n| n["key"] == "out").unwrap();
        assert_eq!(
            out["fields"][0]["dbfield"],
            json!("VIN"),
            "override materializes on the Output field"
        );
        assert_eq!(out["fields"][0]["name"], json!("车辆VIN"));
        // unselected upstream field is dropped via RemoveField, no steps:[]
        let sel = nodes.iter().find(|n| n["type"] == "Select").unwrap();
        assert!(sel["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["type"] == "RemoveField"));

        // two inputs sharing a field name -> Join field carries inputNode;
        // a unique name does not
        let shared = json!({"name":"k","dbfield":"K","dataType":"C",
                            "length":20,"isDimension":true});
        let only_b = json!({"name":"w","dbfield":"W","dataType":"N",
                            "length":18,"decimal":2,"isDimension":false});
        let spec2 = dsl_spec(
            &c,
            &[
                input("l", vec![shared.clone()]),
                input("r", vec![shared.clone(), only_b.clone()]),
            ],
            Some(&parse_fields("k,w", true).unwrap()),
            Some("k"),
            "InnerJoin",
        )
        .unwrap();
        let jn = spec2["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "Join")
            .unwrap();
        let jfs = jn["fields"].as_array().unwrap();
        let kf = jfs.iter().find(|f| f["name"] == "k").unwrap();
        assert_eq!(kf["inputNode"], json!("l"), "ambiguous name gets inputNode");
        assert!(kf.get("dataType").is_none(), "Join fields are name-only");
        let wf = jfs.iter().find(|f| f["name"] == "w").unwrap();
        assert!(wf.get("inputNode").is_none(), "unique name stays bare");
        // no-op seed Select carries no empty steps array
        let sel2 = spec2["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["type"] == "Select")
            .unwrap();
        assert!(
            sel2.get("steps")
                .and_then(Value::as_array)
                .map(|s| !s.is_empty())
                .unwrap_or(true),
            "steps must be absent or non-empty: {:?}",
            sel2
        );
    }

    #[test]
    fn read_input_folders_order_and_stale_cache() {
        let dir = std::env::temp_dir().join(format!("tblgen-newtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("up.tbl");
        // interleaved measure/dimension partition order, a nested folder,
        // numeric-string length, and a stale Output order that is NOT a
        // clean permutation (extra name) -> partition order must win
        let doc = json!({
            "dimensions":[{"name":"d1","dataType":"C","isDimension":true},
                {"name":"grp","isFolder":true,"children":[
                    {"name":"d2","dataType":"C","isDimension":true},
                    {"name":"refonly"},
                    {"name":"d3","dataType":"N","isDimension":true,"length":"12"}]}],
            "measures":[{"name":"m1","dataType":"N","isDimension":false,"decimal":"4"}],
            "dataFlow":{"nodes":{"default":{"fields":[
                {"name":"m1"},{"name":"d1"},{"name":"ghost"},{"name":"d2"}]}}}
        });
        std::fs::write(&p, serde_json::to_string(&doc).unwrap()).unwrap();
        let input = read_input("up.tbl", &p).unwrap();
        let names: Vec<&str> = input
            .fields
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        // stale cache (ghost + missing d3/m1 order) is not a permutation:
        // authoritative partition order is kept, folder children inlined
        assert_eq!(names, vec!["d1", "d2", "d3", "m1"]);
        assert_eq!(
            input.fields[2]["length"],
            json!(12),
            "numeric string normalized"
        );
        assert_eq!(input.fields[3]["decimal"], json!(4));
        assert!(input.fields.iter().all(|f| f.get("isFolder").is_none()));

        // clean permutation -> saved Output order wins
        let doc2 = doc.clone();
        let mut doc2 = doc2;
        doc2["dataFlow"]["nodes"]["default"]["fields"] = json!(["m1", "d3", "d2", "d1"]
            .iter()
            .map(|n| json!({"name":n}))
            .collect::<Vec<_>>());
        std::fs::write(&p, serde_json::to_string(&doc2).unwrap()).unwrap();
        let input2 = read_input("up.tbl", &p).unwrap();
        let names2: Vec<&str> = input2
            .fields
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names2,
            vec!["m1", "d3", "d2", "d1"],
            "clean permutation reorders"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_input_folder_with_stray_type_is_not_a_field() {
        // a folder/hierarchy container is NEVER itself a field — even when
        // it carries stray name/dataType keys alongside isFolder.
        let dir = std::env::temp_dir().join(format!("tblgen-folder-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("up.tbl");
        let doc = json!({
            "dimensions":[
                {"name":"grp","isFolder":true,"dataType":"C","isDimension":true,
                 "children":[{"name":"d1","dataType":"C","isDimension":true}]},
                {"name":"grp2","isHierarchy":true,"name2":"x",
                 "children":[{"name":"d2","dataType":"N","isDimension":true}]}],
            "measures":[]
        });
        std::fs::write(&p, serde_json::to_string(&doc).unwrap()).unwrap();
        let input = read_input("up.tbl", &p).unwrap();
        let names: Vec<&str> = input
            .fields
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["d1", "d2"],
            "containers must not leak as fields"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn permutation_positions_rejects_duplicates_both_ways() {
        let f = |n: &str| json!({"name": n});
        // duplicate folded names in the COLLECTED fields -> no permutation
        let dup_fields = vec![f("A"), f("a")];
        let order: Vec<String> = vec!["A".into(), "B".into()];
        assert!(permutation_positions(&dup_fields, &order).is_none());
        // duplicate folded names in the SAVED order -> no permutation
        let fields = vec![f("A"), f("B")];
        let dup_order: Vec<String> = vec!["a".into(), "A".into()];
        assert!(permutation_positions(&fields, &dup_order).is_none());
        // a clean duplicate-free permutation still resolves
        let ok_order: Vec<String> = vec!["b".into(), "A".into()];
        let pos = permutation_positions(&fields, &ok_order).expect("clean permutation");
        assert_eq!(pos["A"], 1);
        assert_eq!(pos["B"], 0);
    }
}
