//! `generate` — compile a spec-format v1 document into a `.tbl` wire object.
//!
//! Pipeline: dispatch on form (generated / archive / rawTable) →
//! `validate_spec` (all diagnostics, spec RFC 6901 paths) → `build` (field
//! projection, topo ordering, verbatim merges). `generate` returns the first
//! diagnostic as `Err`; `validate_spec` is exposed for `check` on spec input.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::model::*;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compile a spec document to a `.tbl` wire `Value`.
pub fn generate(spec: &Value) -> Result<Value, Diag> {
    if !spec.is_object() {
        return Err(Diag::spec(
            "",
            "structure.spec",
            "spec must be a JSON object",
        ));
    }
    match spec.get("format") {
        Some(v) if v.as_u64() == Some(1) => {}
        _ => {
            return Err(Diag::spec(
                "/format",
                "structure.format",
                "spec.format must be 1",
            ))
        }
    }

    let has_archive = spec.get("archive").is_some();
    let has_rawtable = spec.get("rawTable").is_some();
    let has_nodes = spec.get("nodes").is_some();
    match (has_archive, has_rawtable, has_nodes) {
        (true, false, false) => build_archive(spec),
        (false, true, false) => build_rawtable(spec),
        (false, false, true) => {
            let diags = validate_spec(spec);
            if let Some(d) = diags.into_iter().find(|d| !d.is_warning()) {
                return Err(d);
            }
            build_generated(spec)
        }
        _ => Err(Diag::spec(
            "",
            "structure.form",
            "spec must contain exactly one of: nodes (generated), archive, rawTable",
        )),
    }
}

/// Full spec validation (used by `gen` and by `check` on spec input).
pub fn validate_spec(spec: &Value) -> Vec<Diag> {
    let mut d = Vec::new();
    if !spec.is_object() {
        d.push(Diag::spec(
            "",
            "structure.spec",
            "spec must be a JSON object",
        ));
        return d;
    }
    match spec.get("format") {
        Some(v) if v.as_u64() == Some(1) => {}
        _ => d.push(Diag::spec(
            "/format",
            "structure.format",
            "spec.format must be 1",
        )),
    }
    let has_archive = spec.get("archive").is_some();
    let has_rawtable = spec.get("rawTable").is_some();
    let has_nodes = spec.get("nodes").is_some();
    match (has_archive, has_rawtable, has_nodes) {
        (true, false, false) => validate_archive(spec, &mut d),
        (false, true, false) => validate_rawtable(spec, &mut d),
        (false, false, true) => validate_generated(spec, &mut d),
        _ => d.push(Diag::spec(
            "",
            "structure.form",
            "spec must contain exactly one of: nodes (generated), archive, rawTable",
        )),
    }
    d
}

// ---------------------------------------------------------------------------
// Node preparation: keys, ids, topo
// ---------------------------------------------------------------------------

struct NodeDesc<'a> {
    idx: usize,
    key: String,
    /// resolved wire map key / node id (typed nodes); for rawNode = spec key
    wire_key: String,
    /// spec node object (typed) — None for rawNode entries
    spec: Option<&'a Value>,
    /// rawNode body for raw entries
    raw_body: Option<&'a Value>,
    ty: Option<String>,
    inputs: Vec<String>,
    /// resolved wire node ids for `inputs` (key -> wire_key), filled post-resolution
    inputs_wire: Vec<String>,
    alias: Option<String>,
    /// computed output fields (wire-shaped), filled during build
    out_fields: Option<Vec<Value>>,
}

fn node_ty(spec_node: &Value) -> Option<String> {
    spec_node
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn prepare_nodes<'a>(spec: &'a Value, diags: &mut Vec<Diag>) -> Vec<NodeDesc<'a>> {
    // borrow the spec's node array (lifetime 'a) — do not clone
    let nodes: &[Value] = spec
        .get("nodes")
        .and_then(Value::as_array)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let mut descs: Vec<NodeDesc> = Vec::new();
    let mut keys: HashSet<String> = HashSet::new();
    let mut ids: HashSet<String> = HashSet::new();

    // first pass: keys
    for (i, n) in nodes.iter().enumerate() {
        let path = format!("/nodes/{}", i);
        if !n.is_object() {
            diags.push(Diag::spec(
                path,
                "structure.node",
                "node entry must be an object",
            ));
            continue;
        }
        // closed key check happens in validate_generated; here extract key
        let key = match n.get("key").and_then(Value::as_str) {
            Some(k) if !k.is_empty() => k.to_string(),
            Some(_) => {
                diags.push(Diag::spec(
                    format!("{}/key", path),
                    "structure.node.key",
                    "key must be a non-empty string",
                ));
                format!("n{:04}", i + 1)
            }
            None => format!("n{:04}", i + 1),
        };
        if !keys.insert(key.clone()) {
            diags.push(Diag::spec(
                format!("{}/key", path),
                "structure.node.key.duplicate",
                format!("duplicate node key {:?}", key),
            ));
        }
        descs.push(NodeDesc {
            idx: i,
            key: key.clone(),
            wire_key: String::new(),
            spec: if n.get("rawNode").is_some() {
                None
            } else {
                Some(n)
            },
            raw_body: n.get("rawNode"),
            ty: node_ty(n),
            inputs: Vec::new(),
            inputs_wire: Vec::new(),
            alias: n.get("alias").and_then(Value::as_str).map(str::to_string),
            out_fields: None,
        });
    }

    // second pass: inputs + resolved ids
    let key_set: HashSet<String> = descs.iter().map(|d| d.key.clone()).collect();
    // first declared Output index (typed only)
    let first_output_idx = descs
        .iter()
        .position(|d| d.spec.is_some() && d.ty.as_deref() == Some("Output"));

    for d in descs.iter_mut() {
        if let Some(sn) = d.spec {
            // inputs
            if let Some(arr) = sn.get("inputs").and_then(Value::as_array) {
                for (ii, v) in arr.iter().enumerate() {
                    match v.as_str() {
                        Some(s) => {
                            d.inputs.push(s.to_string());
                            if !key_set.contains(s) {
                                diags.push(Diag::spec(
                                    format!("/nodes/{}/inputs/{}", d.idx, ii),
                                    "graph.danglingInput",
                                    format!("inputs references unknown key {:?}", s),
                                ));
                            }
                        }
                        None => diags.push(Diag::spec(
                            format!("/nodes/{}/inputs/{}", d.idx, ii),
                            "structure.inputs",
                            "inputs entries must be strings (node keys)",
                        )),
                    }
                }
            }
            // resolved id
            let explicit_id = sn.get("id").and_then(Value::as_str).map(str::to_string);
            let resolved =
                if d.idx == first_output_idx.unwrap_or(usize::MAX) && explicit_id.is_none() {
                    "default".to_string()
                } else {
                    explicit_id.unwrap_or_else(|| d.key.clone())
                };
            if !ids.insert(resolved.clone()) {
                diags.push(Diag::spec(
                    format!("/nodes/{}/id", d.idx),
                    "structure.node.id.duplicate",
                    format!("duplicate resolved node id {:?}", resolved),
                ));
            }
            d.wire_key = resolved;
        } else {
            // rawNode: wire map key = spec key, body untouched
            d.wire_key = d.key.clone();
        }
    }

    // resolve input keys -> wire keys now that every wire_key is known
    {
        let key_to_wire: HashMap<String, String> = descs
            .iter()
            .map(|d| (d.key.clone(), d.wire_key.clone()))
            .collect();
        for d in descs.iter_mut() {
            d.inputs_wire = d
                .inputs
                .iter()
                .filter_map(|k| key_to_wire.get(k).cloned())
                .collect();
        }
    }

    // topo order (stable: declaration order among ready nodes)
    topo_sort(&mut descs, diags);
    descs
}

fn topo_sort(descs: &mut Vec<NodeDesc>, diags: &mut Vec<Diag>) {
    // Kahn with declaration-order tie-break; mutates descs into topo order.
    let n = descs.len();
    let key_to_idx: HashMap<String, usize> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key.clone(), i))
        .collect();
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (i, d) in descs.iter().enumerate() {
        for inp in &d.inputs {
            if let Some(&j) = key_to_idx.get(inp) {
                adj[j].push(i);
                indeg[i] += 1;
            }
        }
    }
    let mut ready: Vec<usize> = (0..n).filter(|&i| indeg[i] == 0).collect();
    ready.sort();
    let mut out: Vec<NodeDesc> = Vec::with_capacity(n);
    let mut emitted = vec![false; n];
    while let Some(i) = ready.first().copied() {
        ready.remove(0);
        if emitted[i] {
            continue;
        }
        emitted[i] = true;
        out.push(descs[i].clone_shallow());
        for &j in &adj[i] {
            indeg[j] -= 1;
            if indeg[j] == 0 {
                // insert keeping declaration order
                let pos = ready.iter().position(|&r| r > j).unwrap_or(ready.len());
                ready.insert(pos, j);
            }
        }
    }
    if out.len() < n {
        diags.push(Diag::spec(
            "/nodes",
            "graph.cycle",
            "node inputs contain a cycle",
        ));
        // keep remaining in declaration order so build can proceed/fail clearly
        for (i, d) in descs.iter().enumerate() {
            if !emitted[i] {
                out.push(d.clone_shallow());
            }
        }
    }
    *descs = out;
}

impl<'a> NodeDesc<'a> {
    fn clone_shallow(&self) -> NodeDesc<'a> {
        NodeDesc {
            idx: self.idx,
            key: self.key.clone(),
            wire_key: self.wire_key.clone(),
            spec: self.spec,
            raw_body: self.raw_body,
            ty: self.ty.clone(),
            inputs: self.inputs.clone(),
            inputs_wire: self.inputs_wire.clone(),
            alias: self.alias.clone(),
            out_fields: self.out_fields.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Spec validation
// ---------------------------------------------------------------------------

fn validate_generated(spec: &Value, diags: &mut Vec<Diag>) {
    // closed top-level keys for generated form
    const ALLOWED_TOP: [&str; 7] = [
        "format",
        "version",
        "properties",
        "parameters",
        "subProcesses",
        "nodes",
        "raw",
    ];
    if let Some(obj) = spec.as_object() {
        for k in obj.keys() {
            if !ALLOWED_TOP.contains(&k.as_str()) {
                diags.push(Diag::spec(
                    format!("/{}", ptr_escape(k)),
                    "structure.spec.key",
                    format!("unexpected spec key {:?}; use raw for passthrough", k),
                ));
            }
        }
    }
    if let Some(v) = spec.get("version") {
        if !v.is_string() {
            diags.push(Diag::spec(
                "/version",
                "structure.version",
                "version must be a string",
            ));
        }
    }
    validate_properties(spec, diags);
    validate_parameters(spec, diags);
    validate_subprocesses(spec, diags);
    if let Some(raw) = spec.get("raw") {
        if !raw.is_object() {
            diags.push(Diag::spec("/raw", "structure.raw", "raw must be an object"));
        }
    }

    let nodes = spec.get("nodes").and_then(Value::as_array);
    let Some(nodes) = nodes else {
        diags.push(Diag::spec(
            "/nodes",
            "structure.nodes",
            "nodes must be an array",
        ));
        return;
    };

    // per-node structural validation
    for (i, n) in nodes.iter().enumerate() {
        validate_node_entry(n, i, diags);
    }

    // graph + semantic validation needs prepared descriptors
    let mut graph_diags = Vec::new();
    let descs = prepare_nodes(spec, &mut graph_diags);
    diags.extend(graph_diags);
    validate_graph_semantics(spec, &descs, diags);
}

fn validate_node_entry(n: &Value, i: usize, diags: &mut Vec<Diag>) {
    let path = format!("/nodes/{}", i);
    let Some(obj) = n.as_object() else {
        diags.push(Diag::spec(
            path,
            "structure.node",
            "node entry must be an object",
        ));
        return;
    };
    for k in obj.keys() {
        if !SPEC_NODE_KEYS.contains(&k.as_str()) {
            diags.push(Diag::spec(
                format!("{}/{}", path, ptr_escape(k)),
                "structure.node.key",
                format!("unexpected node key {:?}; use raw/rawNode", k),
            ));
        }
    }
    if let Some(rn) = n.get("rawNode") {
        // rawNode escape: only key + rawNode allowed
        for k in obj.keys() {
            if k != "key" && k != "rawNode" {
                diags.push(Diag::spec(
                    format!("{}/{}", path, ptr_escape(k)),
                    "structure.node.rawNode",
                    "rawNode entries may only carry key + rawNode",
                ));
            }
        }
        if !rn.is_object() {
            diags.push(Diag::spec(
                format!("{}/rawNode", path),
                "structure.node.rawNode",
                "rawNode must be a wire node object",
            ));
        }
        return;
    }
    // typed node
    let ty = match n.get("type").and_then(Value::as_str) {
        Some(t) => t,
        None => {
            diags.push(Diag::spec(
                format!("{}/type", path),
                "structure.node.type",
                "typed node requires type",
            ));
            return;
        }
    };
    if !NODE_TYPES.contains(&ty) {
        diags.push(Diag::spec(
            format!("{}/type", path),
            "enum.nodeType",
            format!("unknown node type {:?}; use rawNode for non-enum types", ty),
        ));
        return;
    }
    if !node_supported(ty) {
        diags.push(Diag::spec(
            format!("{}/type", path),
            "coverage.node.unknown",
            format!("node type {} has no typed semantics; use rawNode", ty),
        ));
        return;
    }
    for k in ["key", "alias", "id"] {
        if let Some(v) = n.get(k) {
            if !v.is_string() {
                diags.push(Diag::spec(
                    format!("{}/{}", path, k),
                    "structure.node",
                    format!("{} must be a string", k),
                ));
            }
        }
    }
    if let Some(v) = n.get("inputs") {
        validate_str_array(v, &format!("{}/inputs", path), "inputs", diags);
    }
    if let Some(v) = n.get("fields") {
        validate_field_array(v, &format!("{}/fields", path), diags);
    }
    if let Some(v) = n.get("originalFields") {
        validate_field_array(v, &format!("{}/originalFields", path), diags);
    }
    if let Some(v) = n.get("config") {
        validate_config(v, ty, &format!("{}/config", path), diags);
    }
    if let Some(v) = n.get("steps") {
        if ty != "Select" {
            diags.push(Diag::spec(
                format!("{}/steps", path),
                "structure.node.steps",
                "steps are only valid on Select nodes",
            ));
        } else {
            validate_steps(v, &format!("{}/steps", path), diags);
        }
    }
    if let Some(v) = n.get("raw") {
        if !v.is_object() {
            diags.push(Diag::spec(
                format!("{}/raw", path),
                "structure.node.raw",
                "raw must be an object",
            ));
        }
    }
}

fn validate_str_array(v: &Value, path: &str, what: &str, diags: &mut Vec<Diag>) {
    match v.as_array() {
        Some(arr) => {
            for (i, e) in arr.iter().enumerate() {
                if !e.is_string() {
                    diags.push(Diag::spec(
                        format!("{}/{}", path, i),
                        "structure.array",
                        format!("{} entries must be strings", what),
                    ));
                }
            }
        }
        None => diags.push(Diag::spec(
            path,
            "structure.array",
            format!("{} must be an array", what),
        )),
    }
}

fn validate_field_array(v: &Value, path: &str, diags: &mut Vec<Diag>) {
    let Some(arr) = v.as_array() else {
        diags.push(Diag::spec(
            path,
            "structure.fields",
            "fields must be an array",
        ));
        return;
    };
    for (i, f) in arr.iter().enumerate() {
        validate_field(f, &format!("{}/{}", path, i), diags);
    }
}

fn validate_field(f: &Value, path: &str, diags: &mut Vec<Diag>) {
    let Some(obj) = f.as_object() else {
        diags.push(Diag::spec(
            path,
            "structure.field",
            "field must be an object",
        ));
        return;
    };
    for k in obj.keys() {
        if !field_key_allowed(k) {
            diags.push(Diag::spec(
                format!("{}/{}", path, ptr_escape(k)),
                "structure.field.key",
                format!("unexpected field key {:?}", k),
            ));
        }
    }
    for k in ["name", "dataType"] {
        match f.get(k).and_then(Value::as_str) {
            Some(s) if !s.is_empty() => {}
            _ => diags.push(Diag::spec(
                format!("{}/{}", path, k),
                "field.required",
                format!("field.{} must be a non-empty string", k),
            )),
        }
    }
    validate_field_scalars(f, path, diags);
}

/// Scalar typing for every allowed FIELD_KEYS member: name/dataType and the
/// other string keys must be strings; isDimension/extractable/hidden
/// booleans; length/decimal integers. Shared with the addField payload
/// (which is field-shaped).
fn validate_field_scalars(f: &Value, path: &str, diags: &mut Vec<Diag>) {
    for k in [
        "name",
        "dataType",
        "dbfield",
        "inputField",
        "inputNode",
        "originalField",
        "originalNode",
        "exp",
        "dateFormat",
        "aggType",
        "desc",
    ] {
        if let Some(v) = f.get(k) {
            if !v.is_string() {
                diags.push(Diag::spec(
                    format!("{}/{}", path, k),
                    "structure.field.type",
                    format!("field.{} must be a string", k),
                ));
            }
        }
    }
    for k in ["isDimension", "extractable", "hidden"] {
        if let Some(v) = f.get(k) {
            if !v.is_boolean() {
                diags.push(Diag::spec(
                    format!("{}/{}", path, k),
                    "structure.field.type",
                    format!("field.{} must be a boolean", k),
                ));
            }
        }
    }
    for k in ["length", "decimal"] {
        if let Some(v) = f.get(k) {
            if !(v.is_i64() || v.is_u64()) {
                diags.push(Diag::spec(
                    format!("{}/{}", path, k),
                    "structure.field.type",
                    format!("field.{} must be an integer", k),
                ));
            }
        }
    }
}

fn validate_config(v: &Value, ty: &str, path: &str, diags: &mut Vec<Diag>) {
    let Some(obj) = v.as_object() else {
        diags.push(Diag::spec(
            path,
            "structure.config",
            "config must be an object",
        ));
        return;
    };
    for (k, kv) in obj {
        let kpath = format!("{}/{}", path, ptr_escape(k));
        // scalar keys (common + per-type)
        if CONFIG_KEYS_COMMON.contains(&k.as_str()) || config_scalar_keys(ty).contains(&k.as_str())
        {
            let kind = config_scalar_kind(ty, k);
            let ok = match kind {
                ScalarKind::Str => kv.is_string(),
                ScalarKind::Num => kv.is_number(),
                ScalarKind::Bool => kv.is_boolean(),
                ScalarKind::AnyScalar => kv.is_string() || kv.is_number() || kv.is_boolean(),
            };
            if !ok {
                diags.push(Diag::spec(
                    kpath.clone(),
                    "structure.config.type",
                    format!("config.{} must be a {:?} scalar", k, kind),
                ));
            }
            if k == "unionType" {
                if let Some(su) = kv.as_str() {
                    if !UNION_TYPES.contains(&su) {
                        diags.push(Diag::spec(
                            kpath.clone(),
                            "enum.unionType",
                            format!("unknown unionType {:?}", su),
                        ));
                    }
                }
            }
            continue;
        }
        // plain string-array keys (distinctFields)
        if config_strarray_keys(ty).contains(&k.as_str()) {
            match kv.as_array() {
                Some(arr) => {
                    for (i, e) in arr.iter().enumerate() {
                        if !e.is_string() {
                            diags.push(Diag::spec(
                                format!("{}/{}", kpath, i),
                                "structure.config",
                                "string-array config entries must be strings",
                            ));
                        }
                    }
                }
                None => diags.push(Diag::spec(
                    kpath,
                    "structure.config",
                    "config value must be an array of strings",
                )),
            }
            continue;
        }
        // closed object/array payloads
        if let Some((_, kind)) = config_closed_keys(ty)
            .iter()
            .find(|(ck, _)| *ck == k.as_str())
        {
            validate_closed_config(kv, *kind, &kpath, diags);
            continue;
        }
        diags.push(Diag::spec(
            kpath,
            "structure.config.key",
            format!("config key {:?} not allowed on {} nodes; use raw", k, ty),
        ));
    }
}

fn validate_closed_config(v: &Value, kind: ClosedKind, path: &str, diags: &mut Vec<Diag>) {
    match kind {
        ClosedKind::StrArray => {
            if let Some(arr) = v.as_array() {
                for (i, e) in arr.iter().enumerate() {
                    if !e.is_string() {
                        diags.push(Diag::spec(
                            format!("{}/{}", path, i),
                            "structure.config",
                            "entries must be strings",
                        ));
                    }
                }
            } else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "expected array of strings",
                ));
            }
        }
        ClosedKind::StrMap => match v.as_object() {
            Some(obj) => {
                for (mk, mv) in obj {
                    if !mv.is_string() {
                        diags.push(Diag::spec(
                            format!("{}/{}", path, ptr_escape(mk)),
                            "structure.config",
                            "groupFieldsMap values must be strings",
                        ));
                    }
                }
            }
            None => diags.push(Diag::spec(
                path,
                "structure.config",
                "expected object of string->string",
            )),
        },
        ClosedKind::Sorts => {
            let Some(arr) = v.as_array() else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "distinctSorts must be an array",
                ));
                return;
            };
            for (i, item) in arr.iter().enumerate() {
                let ip = format!("{}/{}", path, i);
                let Some(o) = item.as_object() else {
                    diags.push(Diag::spec(
                        ip,
                        "structure.config",
                        "distinctSorts entries must be objects",
                    ));
                    continue;
                };
                for k in o.keys() {
                    if !["exp", "type"].contains(&k.as_str()) {
                        diags.push(Diag::spec(
                            format!("{}/{}", ip, ptr_escape(k)),
                            "structure.config.key",
                            "unexpected distinctSorts key",
                        ));
                    }
                }
                for k in ["exp", "type"] {
                    if o.get(k)
                        .and_then(Value::as_str)
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                    {
                        diags.push(Diag::spec(
                            format!("{}/{}", ip, k),
                            "field.required",
                            format!("distinctSorts.{} must be a non-empty string", k),
                        ));
                    }
                }
            }
        }
        ClosedKind::JoinConditions => {
            let Some(arr) = v.as_array() else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "joinConditions must be an array",
                ));
                return;
            };
            for (i, item) in arr.iter().enumerate() {
                let ip = format!("{}/{}", path, i);
                let Some(o) = item.as_object() else {
                    diags.push(Diag::spec(
                        ip,
                        "structure.config",
                        "joinConditions entries must be objects",
                    ));
                    continue;
                };
                for k in o.keys() {
                    if !["leftTable", "rightTable", "joinType", "exp", "clauses"]
                        .contains(&k.as_str())
                    {
                        diags.push(Diag::spec(
                            format!("{}/{}", ip, ptr_escape(k)),
                            "structure.config.key",
                            "unexpected joinConditions key",
                        ));
                    }
                }
                for k in ["leftTable", "rightTable"] {
                    if o.get(k)
                        .and_then(Value::as_str)
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                    {
                        diags.push(Diag::spec(
                            format!("{}/{}", ip, k),
                            "field.required",
                            format!("joinConditions.{} must be a non-empty string", k),
                        ));
                    }
                }
                for k in ["joinType", "exp"] {
                    if let Some(x) = o.get(k) {
                        if !x.is_string() {
                            diags.push(Diag::spec(
                                format!("{}/{}", ip, k),
                                "structure.config.type",
                                format!("joinConditions.{} must be a string", k),
                            ));
                        }
                    }
                }
                if let Some(cl) = o.get("clauses") {
                    let Some(carr) = cl.as_array() else {
                        diags.push(Diag::spec(
                            format!("{}/clauses", ip),
                            "structure.config",
                            "clauses must be an array",
                        ));
                        continue;
                    };
                    for (ci, c) in carr.iter().enumerate() {
                        let cp = format!("{}/clauses/{}", ip, ci);
                        let Some(co) = c.as_object() else {
                            diags.push(Diag::spec(
                                cp,
                                "structure.config",
                                "clause entries must be objects",
                            ));
                            continue;
                        };
                        for k in co.keys() {
                            if !["leftExp", "rightExp", "operator", "exp"].contains(&k.as_str()) {
                                diags.push(Diag::spec(
                                    format!("{}/{}", cp, ptr_escape(k)),
                                    "structure.config.key",
                                    "unexpected clause key",
                                ));
                            }
                        }
                        for k in ["leftExp", "rightExp"] {
                            if co
                                .get(k)
                                .and_then(Value::as_str)
                                .map(|s| s.is_empty())
                                .unwrap_or(true)
                            {
                                diags.push(Diag::spec(
                                    format!("{}/{}", cp, k),
                                    "field.required",
                                    format!("clause.{} must be a non-empty string", k),
                                ));
                            }
                        }
                    }
                }
            }
        }
        ClosedKind::UnionMapArray => {
            let Some(arr) = v.as_array() else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "unionMapArray must be an array",
                ));
                return;
            };
            for (i, item) in arr.iter().enumerate() {
                let ip = format!("{}/{}", path, i);
                let Some(o) = item.as_object() else {
                    diags.push(Diag::spec(
                        ip,
                        "structure.config",
                        "unionMapArray entries must be objects",
                    ));
                    continue;
                };
                for k in o.keys() {
                    if !["visible", "values"].contains(&k.as_str()) {
                        diags.push(Diag::spec(
                            format!("{}/{}", ip, ptr_escape(k)),
                            "structure.config.key",
                            "unexpected unionMapArray key",
                        ));
                    }
                }
                if let Some(vis) = o.get("visible") {
                    if !vis.is_boolean() {
                        diags.push(Diag::spec(
                            format!("{}/visible", ip),
                            "structure.config.type",
                            "unionMapArray.visible must be a boolean",
                        ));
                    }
                }
                match o.get("values") {
                    Some(Value::Array(vals)) => {
                        for (vi, vv) in vals.iter().enumerate() {
                            if !(vv.is_string() || vv.is_null()) {
                                diags.push(Diag::spec(
                                    format!("{}/values/{}", ip, vi),
                                    "structure.config.type",
                                    "unionMapArray.values entries must be strings or null",
                                ));
                            }
                        }
                    }
                    _ => diags.push(Diag::spec(
                        format!("{}/values", ip),
                        "field.required",
                        "unionMapArray.values must be an array",
                    )),
                }
            }
        }
        ClosedKind::ColToRowInfo => {
            let Some(o) = v.as_object() else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "colToRowInfo must be an object",
                ));
                return;
            };
            for k in o.keys() {
                if !["headers", "valuesArray"].contains(&k.as_str()) {
                    diags.push(Diag::spec(
                        format!("{}/{}", path, ptr_escape(k)),
                        "structure.config.key",
                        "unexpected colToRowInfo key",
                    ));
                }
            }
            match o.get("headers") {
                Some(Value::Array(h)) => {
                    for (i, hv) in h.iter().enumerate() {
                        if !hv.is_string() {
                            diags.push(Diag::spec(
                                format!("{}/headers/{}", path, i),
                                "structure.config",
                                "headers entries must be strings",
                            ));
                        }
                    }
                }
                _ => diags.push(Diag::spec(
                    format!("{}/headers", path),
                    "field.required",
                    "colToRowInfo.headers must be an array of strings",
                )),
            }
            match o.get("valuesArray") {
                Some(Value::Array(rows)) => {
                    for (ri, row) in rows.iter().enumerate() {
                        let rp = format!("{}/valuesArray/{}", path, ri);
                        let Some(rarr) = row.as_array() else {
                            diags.push(Diag::spec(
                                rp,
                                "structure.config",
                                "valuesArray rows must be arrays",
                            ));
                            continue;
                        };
                        for (ci, cell) in rarr.iter().enumerate() {
                            let cp = format!("{}/{}", rp, ci);
                            if cell.is_string() || cell.is_null() {
                                continue;
                            }
                            let Some(co) = cell.as_object() else {
                                diags.push(Diag::spec(
                                    cp,
                                    "structure.config.type",
                                    "valuesArray cells must be string, null, or field object",
                                ));
                                continue;
                            };
                            for k in co.keys() {
                                if ![
                                    "name",
                                    "dataType",
                                    "dbfield",
                                    "length",
                                    "decimal",
                                    "inputField",
                                ]
                                .contains(&k.as_str())
                                {
                                    diags.push(Diag::spec(
                                        format!("{}/{}", cp, ptr_escape(k)),
                                        "structure.config.key",
                                        "unexpected field-reference key",
                                    ));
                                }
                            }
                            for k in ["name", "dataType"] {
                                if co
                                    .get(k)
                                    .and_then(Value::as_str)
                                    .map(|s| s.is_empty())
                                    .unwrap_or(true)
                                {
                                    diags.push(Diag::spec(
                                        format!("{}/{}", cp, k),
                                        "field.required",
                                        format!("field reference requires non-empty {}", k),
                                    ));
                                }
                            }
                        }
                    }
                }
                _ => diags.push(Diag::spec(
                    format!("{}/valuesArray", path),
                    "field.required",
                    "colToRowInfo.valuesArray must be an array",
                )),
            }
        }
    }
}

fn validate_steps(v: &Value, path: &str, diags: &mut Vec<Diag>) {
    let Some(arr) = v.as_array() else {
        diags.push(Diag::spec(
            path,
            "structure.steps",
            "steps must be an array",
        ));
        return;
    };
    for (i, s) in arr.iter().enumerate() {
        validate_step(s, &format!("{}/{}", path, i), diags);
    }
}

fn validate_step(s: &Value, path: &str, diags: &mut Vec<Diag>) {
    let Some(obj) = s.as_object() else {
        diags.push(Diag::spec(path, "structure.step", "step must be an object"));
        return;
    };
    if let Some(rs) = obj.get("rawStep") {
        for k in obj.keys() {
            if k != "rawStep" {
                diags.push(Diag::spec(
                    format!("{}/{}", path, ptr_escape(k)),
                    "structure.step.rawStep",
                    "rawStep entries may only carry rawStep",
                ));
            }
        }
        if !rs.is_object() {
            diags.push(Diag::spec(
                format!("{}/rawStep", path),
                "structure.step.rawStep",
                "rawStep must be a wire step object",
            ));
        }
        return;
    }
    let ty = match obj.get("type").and_then(Value::as_str) {
        Some(t) => t,
        None => {
            diags.push(Diag::spec(
                format!("{}/type", path),
                "structure.step.type",
                "typed step requires type",
            ));
            return;
        }
    };
    if !STEP_TYPES.contains(&ty) {
        diags.push(Diag::spec(
            format!("{}/type", path),
            "enum.stepType",
            format!("unknown step type {:?}; use rawStep", ty),
        ));
        return;
    }
    if !step_supported(ty) {
        diags.push(Diag::spec(
            format!("{}/type", path),
            "coverage.step.unknown",
            format!("step type {} has no typed semantics; use rawStep", ty),
        ));
        return;
    }
    // closed step keys
    let allowed = step_allowed_keys(ty);
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            diags.push(Diag::spec(
                format!("{}/{}", path, ptr_escape(k)),
                "structure.step.key",
                format!("unexpected step key {:?} for {}", k, ty),
            ));
        }
    }
    if let Some(f) = obj.get("fields") {
        validate_str_array(f, &format!("{}/fields", path), "fields", diags);
    }
    if let Some(v) = obj.get("isRegExp") {
        if !v.is_boolean() {
            diags.push(Diag::spec(
                format!("{}/isRegExp", path),
                "structure.step",
                "isRegExp must be a boolean",
            ));
        }
    }
    // fields operand: non-empty strings, no duplicates
    if let Some(f) = obj.get("fields") {
        if let Some(arr) = f.as_array() {
            let mut seen = HashSet::new();
            for (i, e) in arr.iter().enumerate() {
                match e.as_str() {
                    Some(s) if !s.is_empty() => {
                        if !seen.insert(s.to_lowercase()) {
                            diags.push(Diag::spec(
                                format!("{}/fields/{}", path, i),
                                "structure.step.fields",
                                "duplicate field in step.fields",
                            ));
                        }
                    }
                    _ => diags.push(Diag::spec(
                        format!("{}/fields/{}", path, i),
                        "structure.step.fields",
                        "step.fields entries must be non-empty strings",
                    )),
                }
            }
        }
    }
    // this step type's own payload: closed keys + typed values; the
    // function appends the actual payload name exactly once.
    validate_step_payload(s, ty, path, diags);
    // prerequisites that hold even when explicit node fields are supplied
    validate_step_prereqs(s, ty, path, diags);
}

/// Validate the payload object of a typed step (`refactor`, `addField`,
/// `split`, `subString`, `replace`, `trim`, `text2id`,
/// `transformFieldInfo`, scalar `filter`).
fn validate_step_payload(obj: &Value, ty: &str, path: &str, diags: &mut Vec<Diag>) {
    // DeleteSpechars.spechars is a top-level step key read via getString
    // (DataFlowSelectNodeStepImpl.java:133) — strictly a string. Checked before
    // the payload-name early return since spechars is not a payload object.
    if ty == "DeleteSpechars" {
        if let Some(v) = obj.get("spechars") {
            if !v.is_string() {
                diags.push(Diag::spec(
                    format!("{}/spechars", path),
                    "structure.step.payload.type",
                    "DeleteSpechars.spechars must be a string",
                ));
            }
        }
    }
    let pname = match step_payload_name(ty) {
        Some(p) => p,
        None => return,
    };
    // payload base path: <step path>/<payload name> — appended exactly once
    let pbase = format!("{}/{}", path, pname);
    // Filter's payload is a scalar expression string, not an object.
    if pname == "filter" {
        match obj.get("filter") {
            Some(v) if v.as_str().map(|s| !s.is_empty()).unwrap_or(false) => {}
            _ => diags.push(Diag::spec(
                format!("{}/filter", path),
                "field.required",
                "Filter step requires a non-empty filter expression",
            )),
        }
        return;
    }
    let pv = match obj.get(pname) {
        Some(v) => v,
        None => {
            diags.push(Diag::spec(
                pbase.clone(),
                "field.required",
                format!("{} step requires a {} payload object", ty, pname),
            ));
            return;
        }
    };
    let Some(pobj) = pv.as_object() else {
        diags.push(Diag::spec(
            pbase.clone(),
            "structure.step.payload",
            format!("{}.{} must be an object", ty, pname),
        ));
        return;
    };
    let Some(keys) = step_payload_keys(pname) else {
        return;
    };
    for k in pobj.keys() {
        if !keys.contains(&k.as_str()) {
            diags.push(Diag::spec(
                format!("{}/{}", pbase, ptr_escape(k)),
                "structure.step.payload.key",
                format!("unexpected {} key {:?}", pname, k),
            ));
        }
    }
    // per-payload required keys + scalar types
    let req_str: &[&str] = match pname {
        "addField" => &["name", "dataType", "exp"],
        "transformFieldInfo" => &["exp", "dataType"],
        _ => &[],
    };
    for k in req_str {
        if pobj
            .get(*k)
            .and_then(Value::as_str)
            .map(|s| s.is_empty())
            .unwrap_or(true)
        {
            diags.push(Diag::spec(
                format!("{}/{}", pbase, k),
                "field.required",
                format!("{}.{} must be a non-empty string", pname, k),
            ));
        }
    }
    // addField is field-shaped: share the field scalar typing (string/bool/int
    // per key) plus the required role/type/name checks above.
    if pname == "addField" {
        validate_field_scalars(pv, &pbase, diags);
    }
    // addField.isDimension must be a declared boolean role
    if pname == "addField"
        && !pobj
            .get("isDimension")
            .map(Value::is_boolean)
            .unwrap_or(false)
    {
        diags.push(Diag::spec(
            format!("{}/isDimension", pbase),
            "field.required",
            "addField.isDimension must be a boolean (dimension/measure role is never guessed)",
        ));
    }
    // typed scalar checks per payload
    let str_keys: &[&str] = match pname {
        "refactor" => &["rename", "desc", "dataType"],
        "text2id" => &[
            "mapTablePath",
            "textField",
            "idField",
            "synonym",
            "sortField",
            "srcField",
        ],
        "trim" => &["trimPosition"],
        "subString" => &["type", "boundaryType", "headChar", "footChar"],
        "split" => &["splitString", "splitOff", "splitType", "resultOrderField"],
        "replace" => &["replace"],
        _ => &[],
    };
    for k in str_keys {
        if let Some(x) = pobj.get(*k) {
            if !x.is_string() {
                diags.push(Diag::spec(
                    format!("{}/{}", pbase, k),
                    "structure.step.payload.type",
                    format!("{}.{} must be a string", pname, k),
                ));
            }
        }
    }
    // integer scalars: getIntValue reads — must be JSON integers
    let int_keys: &[&str] = match pname {
        "subString" => &["count", "start", "end"],
        "split" => &["splitFieldsCount"],
        _ => &[],
    };
    for k in int_keys {
        if let Some(x) = pobj.get(*k) {
            if !(x.is_i64() || x.is_u64()) {
                diags.push(Diag::spec(
                    format!("{}/{}", pbase, k),
                    "structure.step.payload.type",
                    format!("{}.{} must be an integer", pname, k),
                ));
            }
        }
    }
    let bool_keys: &[&str] = match pname {
        "subString" => &["includeBoundary"],
        "trim" => &["trimAllBlank"],
        "split" => &["showSplitResultOrder"],
        _ => &[],
    };
    for k in bool_keys {
        if let Some(x) = pobj.get(*k) {
            if !x.is_boolean() {
                diags.push(Diag::spec(
                    format!("{}/{}", pbase, k),
                    "structure.step.payload.type",
                    format!("{}.{} must be a boolean", pname, k),
                ));
            }
        }
    }
    // enum-valued strings: Java does Enum.valueOf -> invalid values throw
    // (DataFlowSelectNodeStepImpl.java:115-116,152-153,160-161)
    let enums: &[(&str, &[&str], bool)] = match pname {
        // (key, allowed, blank_is_null)
        "trim" => &[(
            "trimPosition",
            &["Front", "End", "FrontAndEnd", "All"][..],
            false,
        )],
        "split" => &[
            ("splitOff", &["First", "Last", "All"][..], true),
            ("splitType", &["Column", "Row"][..], true),
        ],
        "subString" => &[
            ("type", &["Left", "Right", "Mid"][..], true),
            ("boundaryType", &["Index", "Char"][..], true),
        ],
        _ => &[],
    };
    for (k, allowed, blank_ok) in enums {
        if let Some(x) = pobj.get(*k).and_then(Value::as_str) {
            if x.is_empty() && *blank_ok {
                continue;
            }
            if !allowed.contains(&x) {
                diags.push(Diag::spec(
                    format!("{}/{}", pbase, k),
                    "structure.step.payload.enum",
                    format!(
                        "{}.{} value {:?} not in {:?} (Java valueOf would throw)",
                        pname, k, x, allowed
                    ),
                ));
            }
        }
    }
    // array members must be arrays of strings (Java casts List<String>)
    let strarr_keys: &[&str] = match pname {
        "split" => &["childFields"],
        "replace" => &["replaceValues"],
        "groupNumber" => &["intervalCaption"],
        _ => &[],
    };
    for k in strarr_keys {
        if let Some(x) = pobj.get(*k) {
            let ok = x
                .as_array()
                .map(|a| a.iter().all(|e| e.is_string()))
                .unwrap_or(false);
            if !ok {
                diags.push(Diag::spec(
                    format!("{}/{}", pbase, k),
                    "structure.step.payload.type",
                    format!("{}.{} must be an array of strings", pname, k),
                ));
            }
        }
    }
    // groupNumber.interval is List<Integer>
    if pname == "groupNumber" {
        if let Some(x) = pobj.get("interval") {
            let ok = x
                .as_array()
                .map(|a| a.iter().all(|e| e.is_i64() || e.is_u64()))
                .unwrap_or(false);
            if !ok {
                diags.push(Diag::spec(
                    format!("{}/interval", pbase),
                    "structure.step.payload.type",
                    "groupNumber.interval must be an array of integers",
                ));
            }
        }
    }
}

/// Step prerequisites independent of node-level field derivation:
/// Filter needs a non-empty fields operand (Java skips the step otherwise);
/// AddField's `fields` must agree with `addField.name`; Refactor rename is a
/// single string applied to exactly one selected field.
fn validate_step_prereqs(obj: &Value, ty: &str, path: &str, diags: &mut Vec<Diag>) {
    let fields: Vec<String> = obj
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match ty {
        "Filter" => {
            if fields.is_empty() {
                diags.push(Diag::spec(
                    format!("{}/fields", path),
                    "field.required",
                    "Filter step requires a non-empty fields operand (Java skips the step otherwise)",
                ));
            }
        }
        "AddField" => {
            let name = obj
                .pointer("/addField/name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if !name.is_empty() {
                if fields.is_empty() {
                    // synthesized as [addField.name] — fine
                } else if fields.len() != 1 || !fields[0].eq_ignore_ascii_case(&name) {
                    diags.push(Diag::spec(
                        format!("{}/fields", path),
                        "structure.step.fields",
                        format!(
                            "AddField.fields must be exactly [addField.name] ({:?}), got {:?}",
                            name, fields
                        ),
                    ));
                }
            }
        }
        "Refactor" => {
            let has_rename = obj
                .pointer("/refactor/rename")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if has_rename && fields.len() != 1 {
                diags.push(Diag::spec(
                    format!("{}/fields", path),
                    "field.required",
                    "Refactor rename applies to exactly one selected field",
                ));
            }
        }
        _ => {}
    }
}

fn validate_properties(spec: &Value, diags: &mut Vec<Diag>) {
    let empty = Map::new();
    let obj = match spec.get("properties") {
        None => &empty,
        Some(p) => match p.as_object() {
            Some(o) => o,
            None => {
                diags.push(Diag::spec(
                    "/properties",
                    "structure.properties",
                    "properties must be an object",
                ));
                return;
            }
        },
    };
    for (k, v) in obj {
        if k == "raw" {
            continue;
        }
        let path = format!("/properties/{}", ptr_escape(k));
        match prop_kind(k) {
            None => diags.push(Diag::spec(
                path,
                "structure.properties.key",
                format!(
                    "{:?} is not a typed model property; put it under properties.raw to copy it verbatim",
                    k
                ),
            )),
            Some(kind) if !prop_value_fits(kind, v) => diags.push(Diag::spec(
                path,
                if kind == PropKind::ModelDataType {
                    "structure.properties.modelDataType"
                } else {
                    "structure.properties"
                },
                format!("properties.{} must be {}", k, prop_kind_desc(kind)),
            )),
            Some(_) => {}
        }
    }
    let raw = match obj.get("raw") {
        None => None,
        Some(r) => match r.as_object() {
            Some(ro) => Some(ro),
            None => {
                diags.push(Diag::spec(
                    "/properties/raw",
                    "structure.properties.raw",
                    "properties.raw must be an object",
                ));
                None
            }
        },
    };
    // A typed key may also appear in raw (e.g. a copied properties block);
    // it is only an error when both places disagree.
    if let Some(ro) = raw {
        for (k, v) in ro {
            if let Some(typed) = obj.get(k).filter(|_| prop_kind(k).is_some()) {
                if typed != v {
                    diags.push(Diag::spec(
                        format!("/properties/raw/{}", ptr_escape(k)),
                        "structure.properties.raw.conflict",
                        format!(
                            "properties.raw.{k} ({}) conflicts with properties.{k} ({}); keep only properties.{k}",
                            v, typed
                        ),
                    ));
                }
            }
        }
    }
    let lookup = |k: &str| obj.get(k).or_else(|| raw.and_then(|r| r.get(k)));
    run_mode_warnings(lookup("extractDataEnabled"), lookup("dbTableName"), "/properties", diags);
}

/// Run-mode diagnostics shared by spec and wire checks. `extractDataEnabled`
/// selects ETL (output written to dbTableName) vs query logic (graph compiled
/// to SQL at query time); the loader defaults a missing key to false, and an
/// extracted model without dbTableName is treated as not extracted.
pub fn run_mode_warnings(
    extract: Option<&Value>,
    table: Option<&Value>,
    base: &str,
    diags: &mut Vec<Diag>,
) {
    let source = "DwTableLoader#loadProperties; QueryUtils#isExtractDataEnabled";
    let blank_table = table
        .and_then(Value::as_str)
        .map(|s| s.trim().is_empty())
        .unwrap_or(true);
    match extract.and_then(Value::as_bool) {
        None if extract.is_none() => diags.push(Diag::new(
            format!("{}/extractDataEnabled", base),
            "warn.properties.runMode.unset",
            source,
            "extractDataEnabled is not set, so the platform loads this model as query logic \
             (not extracted; queries compile the graph to SQL). Set true for ETL output to \
             dbTableName, or false to make query logic explicit",
        )),
        Some(true) if blank_table => diags.push(Diag::new(
            format!("{}/dbTableName", base),
            "warn.properties.runMode.noTable",
            source,
            "extractDataEnabled is true but dbTableName is blank; the platform treats the \
             model as not extracted. Set dbTableName to the output table",
        )),
        _ => {}
    }
}

fn prop_kind_desc(kind: PropKind) -> &'static str {
    match kind {
        PropKind::ModelDataType => "\"DataFlow\" for generated specs",
        PropKind::Bool => "a boolean",
        PropKind::Str => "a string",
        PropKind::StrList => "an array of strings",
        PropKind::ModifyDbTable => {
            "an array of \"field\"/\"index\"/\"constraint\" (or a legacy boolean)"
        }
    }
}

fn validate_parameters(spec: &Value, diags: &mut Vec<Diag>) {
    let Some(ps) = spec.get("parameters") else {
        return;
    };
    let Some(arr) = ps.as_array() else {
        diags.push(Diag::spec(
            "/parameters",
            "structure.parameters",
            "parameters must be an array",
        ));
        return;
    };
    for (i, p) in arr.iter().enumerate() {
        let path = format!("/parameters/{}", i);
        let Some(obj) = p.as_object() else {
            diags.push(Diag::spec(
                path,
                "structure.parameter",
                "parameter must be an object",
            ));
            continue;
        };
        for k in obj.keys() {
            if !PARAMETER_KEYS.contains(&k.as_str()) {
                diags.push(Diag::spec(
                    format!("{}/{}", path, ptr_escape(k)),
                    "structure.parameter.key",
                    format!("unexpected parameter key {:?}; use raw", k),
                ));
            }
        }
        for k in ["name", "dataType"] {
            match p.get(k).and_then(Value::as_str) {
                Some(s) if !s.is_empty() => {}
                _ => diags.push(Diag::spec(
                    format!("{}/{}", path, k),
                    "field.required",
                    format!("parameter.{} must be a non-empty string", k),
                )),
            }
        }
        if let Some(r) = obj.get("raw") {
            if !r.is_object() {
                diags.push(Diag::spec(
                    format!("{}/raw", path),
                    "structure.parameter.raw",
                    "parameter.raw must be an object",
                ));
            }
        }
    }
}

fn validate_subprocesses(spec: &Value, diags: &mut Vec<Diag>) {
    let Some(sp) = spec.get("subProcesses") else {
        return;
    };
    let Some(obj) = sp.as_object() else {
        diags.push(Diag::spec(
            "/subProcesses",
            "structure.subprocesses",
            "subProcesses must be an object",
        ));
        return;
    };
    for (id, item) in obj {
        let path = format!("/subProcesses/{}", ptr_escape(id));
        let Some(iobj) = item.as_object() else {
            diags.push(Diag::spec(
                path,
                "structure.subprocess",
                "subProcesses entries must be objects",
            ));
            continue;
        };
        for k in iobj.keys() {
            if !SUBPROCESS_KEYS.contains(&k.as_str()) {
                diags.push(Diag::spec(
                    format!("{}/{}", path, ptr_escape(k)),
                    "structure.subprocess.key",
                    format!("unexpected subProcesses key {:?}; use raw", k),
                ));
            }
        }
        match item.get("rootNodeId").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => {}
            _ => diags.push(Diag::spec(
                format!("{}/rootNodeId", path),
                "structure.subprocess",
                "subProcesses.rootNodeId must be a non-empty string",
            )),
        }
        match item.get("nodeIds") {
            Some(Value::Array(arr)) => {
                for (ni, v) in arr.iter().enumerate() {
                    if !v.is_string() {
                        diags.push(Diag::spec(
                            format!("{}/nodeIds/{}", path, ni),
                            "structure.subprocess",
                            "nodeIds entries must be strings",
                        ));
                    }
                }
            }
            _ => diags.push(Diag::spec(
                format!("{}/nodeIds", path),
                "structure.subprocess",
                "subProcesses.nodeIds must be an array",
            )),
        }
        if let Some(r) = iobj.get("raw") {
            if !r.is_object() {
                diags.push(Diag::spec(
                    format!("{}/raw", path),
                    "structure.subprocess.raw",
                    "subProcesses.raw must be an object",
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Graph + semantic validation (spec paths)
// ---------------------------------------------------------------------------

fn validate_graph_semantics(spec: &Value, descs: &[NodeDesc], diags: &mut Vec<Diag>) {
    let key_to_idx: HashMap<&str, usize> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key.as_str(), i))
        .collect();
    // name-resolution memo shared across per-node checks
    let key_to_idx_s: HashMap<String, usize> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key.clone(), i))
        .collect();
    let mut name_memo: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut name_visiting: HashSet<String> = HashSet::new();
    let param_names_s: HashSet<String> = spec
        .get("parameters")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str))
                .map(|n| n.to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    for d in descs {
        let Some(sn) = d.spec else { continue }; // rawNode: no typed semantics
        let ty = d.ty.as_deref().unwrap_or("");
        let n = d.inputs.len();
        // arity (mirrors check rules; spec paths)
        let arity_path = format!("/nodes/{}/inputs", d.idx);
        match ty {
            "Select" | "Output" | "DbOutput" => {
                if n != 1 {
                    diags.push(Diag::spec(
                        arity_path,
                        "err.dataflow.node.input",
                        format!("{} requires exactly 1 input, found {}", ty, n),
                    ));
                }
            }
            "Join" => {
                if n < 2 {
                    diags.push(Diag::spec(
                        arity_path,
                        "err.dataflow.node.input",
                        format!("Join requires at least 2 inputs, found {}", n),
                    ));
                }
            }
            "Union" => {
                if n == 0 {
                    diags.push(Diag::spec(
                        arity_path,
                        "err.dataflow.node.input",
                        "Union requires at least 1 input",
                    ));
                }
            }
            "Agg" | "Distinct" | "RowToCol" | "ColToRow" | "Snapshot" | "Update" => {
                if n == 0 {
                    diags.push(Diag::spec(
                        arity_path,
                        "err.dataflow.node.input",
                        format!("{} requires at least 1 input", ty),
                    ));
                }
            }
            "FileInput" | "ModelTable" => {
                if n > 0 {
                    diags.push(Diag::spec(
                        arity_path,
                        "err.dataflow.node.input",
                        format!("{} takes no graph inputs", ty),
                    ));
                }
            }
            "DbTable" => {
                for inp in &d.inputs {
                    if let Some(&j) = key_to_idx.get(inp.as_str()) {
                        let it = descs[j].ty.as_deref().unwrap_or("");
                        if it != "Sql" && it != "Script" {
                            diags.push(Diag::spec(
                                arity_path.clone(),
                                "graph.dbtable.predecessor",
                                format!("DbTable predecessor must be Sql or Script, found {}", it),
                            ));
                        }
                    }
                }
            }
            "Sql" => {
                for inp in &d.inputs {
                    if let Some(&j) = key_to_idx.get(inp.as_str()) {
                        let it = descs[j].ty.as_deref().unwrap_or("");
                        if it != "Sql" && it != "DbOutput" {
                            diags.push(Diag::spec(
                                arity_path.clone(),
                                "graph.sql.predecessor",
                                format!("Sql predecessor must be Sql or DbOutput, found {}", it),
                            ));
                        }
                    }
                }
            }
            _ => {}
        }

        // fields requirement / derivability
        let has_fields = sn.get("fields").is_some();
        match ty {
            "Select" | "Output" => {
                if !has_fields {
                    if ty == "Select" {
                        check_select_derivable(sn, d.idx, diags);
                    }
                    // Output always derivable from its single input
                }
            }
            _ => {
                if !has_fields {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/fields", d.idx),
                        "field.required",
                        format!("{} nodes require explicit fields (no shape inference for pivot/join/agg/source types)", ty),
                    ));
                }
            }
        }

        // type-specific config requirements
        let cfg = sn.get("config").and_then(Value::as_object);
        if ty == "ModelTable" {
            let blank = cfg
                .and_then(|c| c.get("moduleTablePath"))
                .and_then(Value::as_str)
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            if blank {
                diags.push(Diag::spec(
                    format!("/nodes/{}/config/moduleTablePath", d.idx),
                    "config.modeltable.path",
                    "ModelTable.moduleTablePath must be a non-blank string",
                ));
            }
        }
        if ty == "Join" && n >= 2 {
            check_spec_join_conditions(sn, d, descs, &key_to_idx, diags);
        }
        if ty == "Union" {
            check_spec_union(sn, d, diags);
        }
        if ty == "ColToRow" {
            check_spec_col_to_row(sn, d.idx, diags);
            check_col_to_row_refs(
                d,
                descs,
                &key_to_idx_s,
                &mut name_memo,
                &mut name_visiting,
                diags,
            );
        }
        if ty == "Select" {
            check_select_step_fields(
                d,
                descs,
                &key_to_idx_s,
                &mut name_memo,
                &mut name_visiting,
                &param_names_s,
                diags,
            );
            check_select_explicit_vs_derived(
                d,
                descs,
                &key_to_idx_s,
                &mut name_memo,
                &mut name_visiting,
                diags,
            );
        }
        if ty == "Update" && n > 1 {
            // updateConditions is not a closed typed config — it may arrive
            // via node `raw` (explicit escape) or `config` (rejected earlier,
            // tolerated here for the semantic check).
            let ok = cfg
                .and_then(|c| c.get("updateConditions"))
                .or_else(|| sn.get("raw").and_then(|r| r.get("updateConditions")))
                .and_then(Value::as_array)
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if !ok {
                diags.push(Diag::spec(
                    format!("/nodes/{}/config/updateConditions", d.idx),
                    "err.dataflow.update.conditions",
                    "Update with >1 input requires updateConditions",
                ));
            }
        }
        // raw collision with generated keys (early check; build re-checks)
        if let Some(raw) = sn.get("raw").and_then(Value::as_object) {
            for k in raw.keys() {
                if GENERATED_NODE_KEYS.contains(&k.as_str()) {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/raw/{}", d.idx, ptr_escape(k)),
                        "structure.node.raw.collision",
                        format!("raw key {:?} collides with a generated node key", k),
                    ));
                }
            }
        }
    }

    // top-level raw collision
    if let Some(raw) = spec.get("raw").and_then(Value::as_object) {
        for k in raw.keys() {
            if GENERATED_ROOT_KEYS.contains(&k.as_str()) {
                diags.push(Diag::spec(
                    format!("/raw/{}", ptr_escape(k)),
                    "structure.raw.collision",
                    format!("raw key {:?} collides with a generated top-level key", k),
                ));
            }
        }
        if raw.contains_key("dataFlow") {
            diags.push(Diag::spec(
                "/raw/dataFlow",
                "structure.raw.dataFlow",
                "raw.dataFlow is forbidden: DataFlow has no whole-document raw bypass",
            ));
        }
    }
}

const GENERATED_NODE_KEYS: [&str; 12] = [
    "type",
    "id",
    "alias",
    "inputNodes",
    "fields",
    "originalFields",
    "dimensions",
    "measures",
    "steps",
    "stepCount",
    "hierarchies",
    "lastModifyTime",
];
const GENERATED_ROOT_KEYS: [&str; 6] = [
    "version",
    "properties",
    "parameters",
    "dimensions",
    "measures",
    "dataFlow",
];

fn check_select_derivable(sn: &Value, idx: usize, diags: &mut Vec<Diag>) {
    let Some(steps) = sn.get("steps").and_then(Value::as_array) else {
        return; // no steps: fields = input clone, derivable
    };
    for (si, s) in steps.iter().enumerate() {
        if s.get("rawStep").is_some() {
            diags.push(Diag::spec(
                format!("/nodes/{}/fields", idx),
                "field.required",
                format!("rawStep at /nodes/{}/steps/{} changes fields unpredictably; provide explicit fields", idx, si),
            ));
            continue;
        }
        let ty = s.get("type").and_then(Value::as_str).unwrap_or("");
        if matches!(ty, "SplitField" | "Text2Id" | "TransformToCalculatedField") {
            diags.push(Diag::spec(
                format!("/nodes/{}/fields", idx),
                "field.required",
                format!(
                    "step {} at /nodes/{}/steps/{} changes field shape; provide explicit fields",
                    ty, idx, si
                ),
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Field-name resolution (provable shapes only) — used for operated-field
// existence checks, ColToRow source references and explicit-vs-derived
// comparison. Returns None whenever the shape cannot be proven.
// ---------------------------------------------------------------------------

fn field_names_of(fields: &Value) -> Vec<String> {
    fields
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Evolve a name list through one typed step (names only). Returns false when
/// the step reshapes fields unpredictably.
fn step_evolve_names(names: &mut Vec<String>, step: &Value) -> bool {
    if step.get("rawStep").is_some() {
        return false;
    }
    let ty = step.get("type").and_then(Value::as_str).unwrap_or("");
    let step_fields: Vec<String> = step
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match ty {
        "Filter" | "ToUppercase" | "ToLowercase" | "Trim" | "DeleteNumbers" | "DeleteLetters"
        | "DeleteSpechars" | "DeletePunctuations" | "ExcludeEmpty" | "ExtractNumber"
        | "SubString" | "Replace" => true,
        "RemoveField" => {
            names.retain(|n| !step_fields.iter().any(|f| f.eq_ignore_ascii_case(n)));
            true
        }
        "Refactor" => {
            // rename is a string applied to the single selected field
            if let Some(rn) = step.pointer("/refactor/rename").and_then(Value::as_str) {
                if !rn.is_empty() {
                    if let Some(pos) = names.iter().position(|n| {
                        step_fields
                            .first()
                            .map(|f| f.eq_ignore_ascii_case(n))
                            .unwrap_or(false)
                    }) {
                        names[pos] = rn.to_string();
                    }
                }
            }
            true
        }
        "AddField" => {
            if let Some(nm) = step.pointer("/addField/name").and_then(Value::as_str) {
                names.push(nm.to_string());
            }
            true
        }
        _ => false,
    }
}

/// Resolve a node's output field names when provable (explicit fields,
/// Output identity clone, or Select step simulation). None = unknown.
fn resolve_field_names(
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
    key: &str,
    memo: &mut HashMap<String, Option<Vec<String>>>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<String>> {
    if let Some(m) = memo.get(key) {
        return m.clone();
    }
    if !visiting.insert(key.to_string()) {
        return None; // cycle guard (graph already diagnosed)
    }
    let res = resolve_field_names_inner(descs, key_to_idx, key, memo, visiting);
    visiting.remove(key);
    memo.insert(key.to_string(), res.clone());
    res
}

fn resolve_field_names_inner(
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
    key: &str,
    memo: &mut HashMap<String, Option<Vec<String>>>,
    visiting: &mut HashSet<String>,
) -> Option<Vec<String>> {
    let &i = key_to_idx.get(key)?;
    let d = &descs[i];
    let sn = d.spec?; // rawNode: shape unknown
    if let Some(f) = sn.get("fields") {
        return Some(field_names_of(f));
    }
    match d.ty.as_deref()? {
        "Output" => {
            let pred = d.inputs.first()?;
            resolve_field_names(descs, key_to_idx, pred, memo, visiting)
        }
        "Select" => {
            let pred = d.inputs.first()?;
            let mut names = resolve_field_names(descs, key_to_idx, pred, memo, visiting)?;
            // absent steps == empty steps == identity projection
            if let Some(steps) = sn.get("steps").and_then(Value::as_array) {
                for st in steps {
                    if !step_evolve_names(&mut names, st) {
                        return None;
                    }
                }
            }
            Some(names)
        }
        _ => None,
    }
}

/// Per-step operated-field existence for Select nodes (names provable only).
fn check_select_step_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
    memo: &mut HashMap<String, Option<Vec<String>>>,
    visiting: &mut HashSet<String>,
    param_names: &HashSet<String>,
    diags: &mut Vec<Diag>,
) {
    let Some(sn) = d.spec else { return };
    let Some(pred_key) = d.inputs.first() else {
        return;
    };
    let Some(mut current) = resolve_field_names(descs, key_to_idx, pred_key, memo, visiting) else {
        return; // cannot prove the input field set
    };
    let Some(steps) = sn.get("steps").and_then(Value::as_array) else {
        return;
    };
    for (si, st) in steps.iter().enumerate() {
        if st.get("rawStep").is_some() {
            break; // tracking lost
        }
        let ty = st.get("type").and_then(Value::as_str).unwrap_or("");
        // Java checkStep exempts Text2Id only when it carries srcField
        let operated = matches!(
            ty,
            "RemoveField"
                | "SplitField"
                | "Refactor"
                | "SubString"
                | "Replace"
                | "ToUppercase"
                | "ToLowercase"
                | "ToNumber"
                | "ToDate"
                | "ToSynonym"
                | "Trim"
                | "DeleteNumbers"
                | "DeleteLetters"
                | "DeleteSpechars"
                | "DeletePunctuations"
                | "ExcludeEmpty"
                | "ExtractNumber"
                | "TransformToCalculatedField"
                | "Distinct"
                | "GroupNumber"
                | "ReGroup"
        ) || (ty == "Text2Id"
            && st
                .pointer("/text2id/srcField")
                .and_then(Value::as_str)
                .map(|x| x.is_empty())
                .unwrap_or(true));
        if operated {
            if let Some(fl) = st.get("fields").and_then(Value::as_array) {
                for (fi, fv) in fl.iter().enumerate() {
                    if let Some(name) = fv.as_str() {
                        if ty != "RemoveField"
                            && !current.iter().any(|c| c.eq_ignore_ascii_case(name))
                        {
                            diags.push(Diag::spec(
                                format!("/nodes/{}/steps/{}/fields/{}", d.idx, si, fi),
                                "err.dataflow.stepField.notfound",
                                format!("step {} references unknown field {:?}", ty, name),
                            ));
                        }
                    }
                }
            }
        }
        // conservative expression check: bracket refs in filter/addField.exp/
        // transformFieldInfo.exp must resolve to a current field or a
        // declared parameter (unqualified, case-insensitive). Qualified refs
        // and unparseable grammar are skipped — never guessed.
        for (ekey, epath) in [
            ("filter", "filter"),
            ("addField", "addField/exp"),
            ("transformFieldInfo", "transformFieldInfo/exp"),
        ] {
            let exp = if ekey == "filter" {
                st.get("filter").and_then(Value::as_str)
            } else {
                st.pointer(&format!("/{}/exp", ekey))
                    .and_then(Value::as_str)
            };
            let Some(exp) = exp else { continue };
            for (name, qualified) in exp_bracket_refs(exp) {
                if qualified || name.is_empty() {
                    continue;
                }
                let lower = name.to_lowercase();
                let known = current.iter().any(|c| c.to_lowercase() == lower)
                    || param_names.contains(&lower);
                if !known {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/steps/{}/{}", d.idx, si, epath),
                        "err.exp.ref.notfound",
                        format!(
                            "expression references unknown field/parameter {:?} (unqualified refs resolve against current fields + parameters)",
                            name
                        ),
                    ));
                }
            }
        }
        if !step_evolve_names(&mut current, st) {
            break; // tracking lost
        }
    }
}

/// Compare a Select's explicit `fields` against the provable derivation:
/// name sequence (case-insensitive) per position. Display/other attributes
/// are never required to match.
fn check_select_explicit_vs_derived(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
    memo: &mut HashMap<String, Option<Vec<String>>>,
    visiting: &mut HashSet<String>,
    diags: &mut Vec<Diag>,
) {
    let Some(sn) = d.spec else { return };
    let Some(explicit) = sn.get("fields").and_then(Value::as_array) else {
        return;
    };
    let Some(pred_key) = d.inputs.first() else {
        return;
    };
    let Some(mut names) = resolve_field_names(descs, key_to_idx, pred_key, memo, visiting) else {
        return;
    };
    let derivable = match sn.get("steps").and_then(Value::as_array) {
        Some(steps) => {
            let mut ok = true;
            for st in steps {
                if !step_evolve_names(&mut names, st) {
                    ok = false;
                    break;
                }
            }
            ok
        }
        None => true,
    };
    if !derivable {
        return;
    }
    let exp_names: Vec<String> = explicit
        .iter()
        .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    if names.len() != exp_names.len()
        || !names
            .iter()
            .zip(exp_names.iter())
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
    {
        diags.push(Diag::spec(
            format!("/nodes/{}/fields", d.idx),
            "field.mismatch",
            format!(
                "explicit fields {:?} do not match derivable output {:?}",
                exp_names, names
            ),
        ));
    }
}

/// ColToRow valuesArray {name,dataType} references must exist in the
/// predecessor's provable field set.
fn check_col_to_row_refs(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
    memo: &mut HashMap<String, Option<Vec<String>>>,
    visiting: &mut HashSet<String>,
    diags: &mut Vec<Diag>,
) {
    let Some(sn) = d.spec else { return };
    let Some(pred_key) = d.inputs.first() else {
        return;
    };
    let Some(pred_names) = resolve_field_names(descs, key_to_idx, pred_key, memo, visiting) else {
        return;
    };
    let Some(info) = sn
        .get("config")
        .and_then(|c| c.get("colToRowInfo"))
        .and_then(Value::as_object)
    else {
        return;
    };
    let Some(rows) = info.get("valuesArray").and_then(Value::as_array) else {
        return;
    };
    for (ri, row) in rows.iter().enumerate() {
        let Some(rarr) = row.as_array() else {
            continue;
        };
        for (ci, cell) in rarr.iter().enumerate() {
            if let Some(co) = cell.as_object() {
                if let Some(nm) = co.get("name").and_then(Value::as_str) {
                    if !pred_names.iter().any(|p| p.eq_ignore_ascii_case(nm)) {
                        diags.push(Diag::spec(
                            format!(
                                "/nodes/{}/config/colToRowInfo/valuesArray/{}/{}",
                                d.idx, ri, ci
                            ),
                            "field.notfound",
                            format!(
                                "ColToRow field reference {:?} not found in predecessor fields {:?}",
                                nm, pred_names
                            ),
                        ));
                    }
                }
            }
        }
    }
}

fn spec_node_alias<'a>(descs: &'a [NodeDesc], key: &str) -> Option<&'a str> {
    descs
        .iter()
        .find(|d| d.key == key)
        .map(|d| d.alias.as_deref().unwrap_or(d.key.as_str()))
}

fn check_spec_join_conditions(
    sn: &Value,
    d: &NodeDesc,
    descs: &[NodeDesc],
    _key_to_idx: &HashMap<&str, usize>,
    diags: &mut Vec<Diag>,
) {
    let cfg = sn.get("config").and_then(Value::as_object);
    let Some(conds) = cfg
        .and_then(|c| c.get("joinConditions"))
        .and_then(Value::as_array)
    else {
        return;
    };
    let n = d.inputs.len();
    if conds.len() != n - 1 {
        diags.push(Diag::spec(
            format!("/nodes/{}/config/joinConditions", d.idx),
            "join.conditions.count",
            format!(
                "joinConditions count {} != inputs-1 ({})",
                conds.len(),
                n - 1
            ),
        ));
    }
    // alias set from input nodes (alias defaults to key)
    let mut aliases: HashSet<String> = HashSet::new();
    for inp in &d.inputs {
        if let Some(a) = spec_node_alias(descs, inp) {
            aliases.insert(a.to_string());
        }
        aliases.insert(inp.clone());
    }
    for (i, c) in conds.iter().enumerate() {
        for side in ["leftTable", "rightTable"] {
            if let Some(t) = c.get(side).and_then(Value::as_str) {
                if !t.is_empty() && !aliases.contains(t) {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/config/joinConditions/{}/{}", d.idx, i, side),
                        "join.condition.alias",
                        format!("{} {:?} does not match any input node alias/key", side, t),
                    ));
                }
            }
        }
    }
}

fn check_spec_union(sn: &Value, d: &NodeDesc, diags: &mut Vec<Diag>) {
    let cfg = sn.get("config").and_then(Value::as_object);
    if let Some(ut) = cfg.and_then(|c| c.get("unionType")) {
        if let Some(s) = ut.as_str() {
            if !["UnionAll", "Union", "Minus", "Intersect"].contains(&s) {
                diags.push(Diag::spec(
                    format!("/nodes/{}/config/unionType", d.idx),
                    "enum.unionType",
                    format!("unknown unionType {:?}", s),
                ));
            }
        } else {
            diags.push(Diag::spec(
                format!("/nodes/{}/config/unionType", d.idx),
                "structure.unionType",
                "unionType must be a string",
            ));
        }
    }
    if let Some(map) = cfg
        .and_then(|c| c.get("unionMapArray"))
        .and_then(Value::as_array)
    {
        for (i, item) in map.iter().enumerate() {
            if let Some(vals) = item.get("values").and_then(Value::as_array) {
                if vals.len() != d.inputs.len() {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/config/unionMapArray/{}/values", d.idx, i),
                        "structure.unionMap",
                        format!(
                            "unionMapArray[{}].values length {} != input count {}",
                            i,
                            vals.len(),
                            d.inputs.len()
                        ),
                    ));
                }
            }
        }
    }
}

fn check_spec_col_to_row(sn: &Value, idx: usize, diags: &mut Vec<Diag>) {
    let Some(info) = sn
        .get("config")
        .and_then(|c| c.get("colToRowInfo"))
        .and_then(Value::as_object)
    else {
        return;
    };
    let headers = info.get("headers").and_then(Value::as_array);
    let values = info.get("valuesArray").and_then(Value::as_array);
    let (Some(h), Some(v)) = (headers, values) else {
        return;
    };
    if h.is_empty() || v.is_empty() {
        return;
    }
    // every header must name a declared output field
    let field_names: HashSet<String> = sn
        .get("fields")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for (i, hv) in h.iter().enumerate() {
        if let Some(name) = hv.as_str() {
            if !name.is_empty() && !field_names.is_empty() && !field_names.contains(name) {
                diags.push(Diag::spec(
                    format!("/nodes/{}/config/colToRowInfo/headers/{}", idx, i),
                    "field.name",
                    format!(
                        "colToRowInfo.headers[{}] {:?} has no matching output field",
                        i, name
                    ),
                ));
            }
        }
    }
    for (r, row) in v.iter().enumerate() {
        if let Some(rarr) = row.as_array() {
            if rarr.len() != h.len() {
                diags.push(Diag::spec(
                    format!("/nodes/{}/config/colToRowInfo/valuesArray/{}", idx, r),
                    "err.dataflow.colToRow.config",
                    format!(
                        "valuesArray[{}] width {} != headers width {}",
                        r,
                        rarr.len(),
                        h.len()
                    ),
                ));
            }
            for (c, cell) in rarr.iter().enumerate() {
                if let Some(co) = cell.as_object() {
                    // {name,dataType} field reference: both required non-empty
                    for k in ["name", "dataType"] {
                        if co
                            .get(k)
                            .and_then(Value::as_str)
                            .map(|s| s.is_empty())
                            .unwrap_or(true)
                        {
                            diags.push(Diag::spec(
                                format!(
                                    "/nodes/{}/config/colToRowInfo/valuesArray/{}/{}",
                                    idx, r, c
                                ),
                                "field.required",
                                format!("valuesArray field reference requires non-empty {}", k),
                            ));
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Archive / rawTable forms
// ---------------------------------------------------------------------------

fn validate_archive(spec: &Value, diags: &mut Vec<Diag>) {
    let Some(obj) = spec.as_object() else { return };
    for k in obj.keys() {
        if k != "format" && k != "archive" {
            diags.push(Diag::spec(
                format!("/{}", ptr_escape(k)),
                "structure.archive.key",
                format!("unexpected key {:?} on archive form", k),
            ));
        }
    }
    let Some(a) = spec.get("archive") else {
        diags.push(Diag::spec(
            "/archive",
            "structure.archive",
            "archive is required",
        ));
        return;
    };
    let Some(aobj) = a.as_object() else {
        diags.push(Diag::spec(
            "/archive",
            "structure.archive",
            "archive must be an object",
        ));
        return;
    };
    for k in aobj.keys() {
        if !["top", "flow", "nodes"].contains(&k.as_str()) {
            diags.push(Diag::spec(
                format!("/archive/{}", ptr_escape(k)),
                "structure.archive.key",
                format!("unexpected archive key {:?}", k),
            ));
        }
    }
    for k in ["top", "flow"] {
        match a.get(k) {
            Some(v) if v.is_object() => {}
            _ => diags.push(Diag::spec(
                format!("/archive/{}", k),
                "structure.archive",
                format!("archive.{} must be an object", k),
            )),
        }
    }
    match a.get("nodes") {
        Some(Value::Array(arr)) => {
            let mut ids = HashSet::new();
            for (i, e) in arr.iter().enumerate() {
                let path = format!("/archive/nodes/{}", i);
                let Some(eobj) = e.as_object() else {
                    diags.push(Diag::spec(
                        path,
                        "structure.archive.node",
                        "archive node entries must be objects",
                    ));
                    continue;
                };
                for k in eobj.keys() {
                    if k != "id" && k != "body" {
                        diags.push(Diag::spec(
                            format!("{}/{}", path, ptr_escape(k)),
                            "structure.archive.node.key",
                            format!("unexpected archive node key {:?}", k),
                        ));
                    }
                }
                match e.get("id").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() => {
                        if !ids.insert(s.to_string()) {
                            diags.push(Diag::spec(
                                format!("{}/id", path),
                                "structure.archive.node.id",
                                format!("duplicate archive node id {:?}", s),
                            ));
                        }
                    }
                    _ => diags.push(Diag::spec(
                        format!("{}/id", path),
                        "structure.archive.node.id",
                        "archive node.id must be a non-empty string",
                    )),
                }
                match e.get("body") {
                    Some(v) if v.is_object() => {}
                    _ => diags.push(Diag::spec(
                        format!("{}/body", path),
                        "structure.archive.node",
                        "archive node.body must be a wire node object",
                    )),
                }
            }
        }
        _ => diags.push(Diag::spec(
            "/archive/nodes",
            "structure.archive.nodes",
            "archive.nodes must be an array",
        )),
    }
    // collisions: top must not contain dataFlow; flow must not contain nodes
    if let Some(top) = a.get("top").and_then(Value::as_object) {
        if top.contains_key("dataFlow") {
            diags.push(Diag::spec(
                "/archive/top/dataFlow",
                "structure.archive.collision",
                "archive.top must not contain dataFlow",
            ));
        }
    }
    if let Some(flow) = a.get("flow").and_then(Value::as_object) {
        if flow.contains_key("nodes") {
            diags.push(Diag::spec(
                "/archive/flow/nodes",
                "structure.archive.collision",
                "archive.flow must not contain nodes",
            ));
        }
    }
}

fn validate_rawtable(spec: &Value, diags: &mut Vec<Diag>) {
    if let Some(obj) = spec.as_object() {
        for k in obj.keys() {
            if k != "format" && k != "rawTable" {
                diags.push(Diag::spec(
                    format!("/{}", ptr_escape(k)),
                    "structure.rawTable.key",
                    format!("unexpected key {:?} on rawTable form", k),
                ));
            }
        }
    }
    match spec.get("rawTable") {
        Some(v) if v.is_object() => {}
        _ => diags.push(Diag::spec(
            "/rawTable",
            "structure.rawTable",
            "rawTable must be a .tbl object",
        )),
    }
}

fn build_archive(spec: &Value) -> Result<Value, Diag> {
    let diags = {
        let mut d = Vec::new();
        validate_archive(spec, &mut d);
        d
    };
    if let Some(e) = diags.into_iter().next() {
        return Err(e);
    }
    let a = &spec["archive"];
    let mut tbl = a["top"].clone();
    let mut flow = a["flow"].as_object().cloned().unwrap_or_default();
    let mut nodes = Map::new();
    for e in a["nodes"].as_array().cloned().unwrap_or_default() {
        let id = e["id"].as_str().unwrap_or_default().to_string();
        nodes.insert(id, e["body"].clone());
    }
    flow.insert("nodes".into(), Value::Object(nodes));
    if let Value::Object(ref mut t) = tbl {
        t.insert("dataFlow".into(), Value::Object(flow));
    }
    Ok(tbl)
}

fn build_rawtable(spec: &Value) -> Result<Value, Diag> {
    match spec.get("rawTable") {
        Some(v) if v.is_object() => Ok(v.clone()),
        _ => Err(Diag::spec(
            "/rawTable",
            "structure.rawTable",
            "rawTable must be a .tbl object",
        )),
    }
}

// ---------------------------------------------------------------------------
// Generated form build
// ---------------------------------------------------------------------------

fn build_generated(spec: &Value) -> Result<Value, Diag> {
    let mut diags = Vec::new();
    let descs = prepare_nodes(spec, &mut diags);
    if let Some(d) = diags.into_iter().next() {
        return Err(d);
    }

    // compute output fields in topo order
    let key_to_idx: HashMap<String, usize> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key.clone(), i))
        .collect();
    let mut descs = descs;
    for i in 0..descs.len() {
        let (fields, err) = compute_node_fields(&descs, i, &key_to_idx);
        match (fields, err) {
            (Some(f), _) => descs[i].out_fields = Some(f),
            (None, Some(e)) => return Err(e),
            (None, None) => {}
        }
    }

    // emit nodes
    let mut nodes_map = Map::new();
    for d in &descs {
        let body = if let Some(rn) = d.raw_body {
            rn.clone() // verbatim, never mutated
        } else {
            emit_typed_node(d)?
        };
        nodes_map.insert(d.wire_key.clone(), body);
    }

    // dataFlow
    let mut flow = Map::new();
    flow.insert("nodes".into(), Value::Object(nodes_map));
    if let Some(sp) = spec.get("subProcesses") {
        flow.insert("subProcesses".into(), emit_subprocesses(sp)?);
    }

    // top-level
    let mut tbl = Map::new();
    if let Some(v) = spec.get("version") {
        tbl.insert("version".into(), v.clone());
    }
    tbl.insert("properties".into(), emit_properties(spec)?);
    if let Some(p) = spec.get("parameters") {
        tbl.insert("parameters".into(), emit_parameters(p)?);
    }
    // top dimensions/measures from default Output
    let (dims, measures) = default_output_partition(&descs);
    tbl.insert("dimensions".into(), Value::Array(dims));
    tbl.insert("measures".into(), Value::Array(measures));
    tbl.insert("dataFlow".into(), Value::Object(flow));
    // top raw merge (collisions already validated)
    if let Some(raw) = spec.get("raw").and_then(Value::as_object) {
        for (k, v) in raw {
            tbl.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(tbl))
}

fn compute_node_fields(
    descs: &[NodeDesc],
    i: usize,
    key_to_idx: &HashMap<String, usize>,
) -> (Option<Vec<Value>>, Option<Diag>) {
    let d = &descs[i];
    let Some(sn) = d.spec else {
        return (None, None);
    }; // rawNode
    let ty = d.ty.as_deref().unwrap_or("");
    if let Some(f) = sn.get("fields") {
        let mut fields = f.as_array().cloned().unwrap_or_default();
        if ty == "FileInput" {
            fill_fileinput_identity(&mut fields);
        }
        return (Some(fields), None);
    }
    match ty {
        "Output" => {
            // identity clone of the single input's fields
            let Some(pred_key) = d.inputs.first() else {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/inputs", d.idx),
                        "err.dataflow.node.input",
                        "Output requires 1 input for field derivation",
                    )),
                );
            };
            let Some(&pi) = key_to_idx.get(pred_key) else {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/inputs", d.idx),
                        "graph.danglingInput",
                        "unknown input key",
                    )),
                );
            };
            let pred = &descs[pi];
            let Some(pred_fields) = &pred.out_fields else {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/fields", d.idx),
                        "field.required",
                        "predecessor fields not computed",
                    )),
                );
            };
            let pred_alias = pred.alias.as_deref().unwrap_or(pred.key.as_str());
            let out = pred_fields
                .iter()
                .enumerate()
                .map(|(fi, f)| clone_field(f, pred_alias, field_name(f, fi)))
                .collect();
            (Some(out), None)
        }
        "Select" => derive_select_fields(d, descs, key_to_idx),
        _ => (None, None), // validated: explicit fields required
    }
}

fn field_name(f: &Value, idx: usize) -> String {
    f.get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("field{}", idx))
}

/// Identity clone: copy field minus exp/aggType/lineage keys, then set
/// inputNode=predecessor alias, inputField=predecessor name, and fill
/// originalNode/originalField from the oldest known origin.
fn clone_field(f: &Value, pred_alias: &str, pred_name: String) -> Value {
    let mut out = f.as_object().cloned().unwrap_or_default();
    for k in [
        "exp",
        "aggType",
        "inputField",
        "inputNode",
        "originalField",
        "originalNode",
    ] {
        out.remove(k);
    }
    out.insert("inputNode".into(), Value::String(pred_alias.to_string()));
    out.insert("inputField".into(), Value::String(pred_name.clone()));
    let orig_node = f
        .get("originalNode")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| pred_alias.to_string());
    let orig_field = f
        .get("originalField")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| pred_name);
    out.insert("originalNode".into(), Value::String(orig_node));
    out.insert("originalField".into(), Value::String(orig_field));
    Value::Object(out)
}

struct WorkField {
    /// current output name (after renames)
    out_name: String,
    /// name in the Select's direct predecessor (inputField)
    src_name: String,
    /// field template (predecessor field minus lineage/exp/aggType)
    template: Map<String, Value>,
    /// predecessor's original lineage
    orig_node: String,
    orig_field: String,
    is_new: bool, // AddField result: emit addField verbatim instead
    add_field: Option<Value>,
}

fn derive_select_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<Vec<Value>>, Option<Diag>) {
    let sn = d.spec.unwrap();
    let Some(pred_key) = d.inputs.first() else {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs", d.idx),
                "err.dataflow.node.input",
                "Select requires 1 input",
            )),
        );
    };
    let Some(&pi) = key_to_idx.get(pred_key) else {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs", d.idx),
                "graph.danglingInput",
                "unknown input key",
            )),
        );
    };
    let pred = &descs[pi];
    let Some(pred_fields) = &pred.out_fields else {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/fields", d.idx),
                "field.required",
                "predecessor fields not computed",
            )),
        );
    };
    let pred_alias = pred.alias.as_deref().unwrap_or(pred.key.as_str());

    // init working set from predecessor fields (strip lineage; keep template)
    let mut work: Vec<WorkField> = Vec::new();
    for (fi, f) in pred_fields.iter().enumerate() {
        let name = field_name(f, fi);
        let mut template = f.as_object().cloned().unwrap_or_default();
        for k in [
            "exp",
            "aggType",
            "inputField",
            "inputNode",
            "originalField",
            "originalNode",
        ] {
            template.remove(k);
        }
        work.push(WorkField {
            out_name: name.clone(),
            src_name: name.clone(),
            orig_node: f
                .get("originalNode")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| pred_alias.to_string()),
            orig_field: f
                .get("originalField")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| field_name(f, fi)),
            template,
            is_new: false,
            add_field: None,
        });
    }

    let steps = sn
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for (si, s) in steps.iter().enumerate() {
        // rawStep: cannot derive through it
        if s.get("rawStep").is_some() {
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/steps/{}", d.idx, si),
                    "field.required",
                    "rawStep changes fields unpredictably; provide explicit fields",
                )),
            );
        }
        let ty = s.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "Filter" | "ToUppercase" | "ToLowercase" | "Trim" | "DeleteNumbers"
            | "DeleteLetters" | "DeleteSpechars" | "DeletePunctuations" | "ExcludeEmpty"
            | "ExtractNumber" | "SubString" | "Replace" | "Distinct" => {}
            "RemoveField" => {
                if let Some(fl) = s.get("fields").and_then(Value::as_array) {
                    let names: HashSet<String> = fl
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    work.retain(|w| !names.contains(&w.out_name));
                }
            }
            "Refactor" => {
                // Java: rename is a STRING applied to the single selected
                // field's current name (contributor_select renames the
                // fields entry). Type/desc conversions that change
                // length/decimal cannot be inferred -> explicit fields.
                let has_type_conv = s
                    .pointer("/refactor")
                    .and_then(Value::as_object)
                    .map(|o| {
                        o.keys().any(|k| {
                            matches!(
                                k.as_str(),
                                "dataType" | "length" | "decimal" | "aggType" | "dateFormat"
                            )
                        })
                    })
                    .unwrap_or(false);
                if has_type_conv {
                    return (
                        None,
                        Some(Diag::spec(
                            format!("/nodes/{}/fields", d.idx),
                            "field.required",
                            "Refactor type conversion cannot infer length/decimal; provide explicit fields",
                        )),
                    );
                }
                if let Some(rn) = s.pointer("/refactor/rename").and_then(Value::as_str) {
                    if !rn.is_empty() {
                        // find the selected field (validation guarantees
                        // exactly one); match case-insensitively
                        let sel = s
                            .get("fields")
                            .and_then(Value::as_array)
                            .and_then(|a| a.first())
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if let Some(w) = work
                            .iter_mut()
                            .find(|w| w.out_name.eq_ignore_ascii_case(sel))
                        {
                            w.out_name = rn.to_string();
                        }
                    }
                }
            }
            "AddField" => {
                let af = s.get("addField").cloned().unwrap_or(Value::Null);
                let name = af
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let dt = af
                    .get("dataType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let has_role = af
                    .get("isDimension")
                    .map(Value::is_boolean)
                    .unwrap_or(false);
                let has_exp = af
                    .get("exp")
                    .and_then(Value::as_str)
                    .map(|x| !x.is_empty())
                    .unwrap_or(false);
                if name.is_empty() || dt.is_empty() || !has_role || !has_exp {
                    return (None, Some(Diag::spec(
                        format!("/nodes/{}/steps/{}/addField", d.idx, si),
                        "field.required",
                        "AddField requires declared name, dataType, isDimension role and exp for derivation",
                    )));
                }
                work.push(WorkField {
                    out_name: name.clone(),
                    src_name: name,
                    template: Map::new(),
                    orig_node: String::new(),
                    orig_field: String::new(),
                    is_new: true,
                    add_field: Some(af),
                });
            }
            "SplitField" | "Text2Id" | "TransformToCalculatedField" => {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/fields", d.idx),
                        "field.required",
                        format!("step {} changes field shape; provide explicit fields", ty),
                    )),
                );
            }
            _ => {
                // unsupported step types already rejected; defensive
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/steps/{}", d.idx, si),
                        "field.required",
                        format!("step {} cannot be used for field derivation", ty),
                    )),
                );
            }
        }
    }

    let out: Vec<Value> = work
        .iter()
        .map(|w| {
            if w.is_new {
                // emit the declared addField payload but under the CURRENT
                // name (a later Refactor may have renamed it)
                let mut af = w.add_field.clone().unwrap_or(Value::Null);
                if let Some(m) = af.as_object_mut() {
                    m.insert("name".into(), Value::String(w.out_name.clone()));
                }
                return af;
            }
            let mut m = w.template.clone();
            m.insert("name".into(), Value::String(w.out_name.clone()));
            m.insert("inputNode".into(), Value::String(pred_alias.to_string()));
            m.insert("inputField".into(), Value::String(w.src_name.clone()));
            m.insert("originalNode".into(), Value::String(w.orig_node.clone()));
            m.insert("originalField".into(), Value::String(w.orig_field.clone()));
            Value::Object(m)
        })
        .collect();
    (Some(out), None)
}

/// DwDataFlowContext#getFileTempFields (206-229) builds the temp-field map
/// ONLY from fields with a nonblank `inputField`; unmatched input columns
/// get setExtractable(false). For a typed FileInput, a spec field that omits
/// `inputField` and has no `exp` refers to the physical column of the same
/// name, so we fill the identity mapping. Explicit inputField aliases and
/// exp-bearing fields are preserved.
fn fill_fileinput_identity(fields: &mut [Value]) {
    for f in fields.iter_mut() {
        let Some(m) = f.as_object_mut() else {
            continue;
        };
        if m.contains_key("inputField") || m.contains_key("exp") {
            continue;
        }
        if let Some(nm) = m.get("name").and_then(Value::as_str) {
            m.insert("inputField".into(), Value::String(nm.to_string()));
        }
    }
}

fn emit_typed_node(d: &NodeDesc) -> Result<Value, Diag> {
    let sn = d.spec.unwrap();
    let ty = d.ty.as_deref().unwrap_or("");
    let mut body = Map::new();
    body.insert("type".into(), Value::String(ty.to_string()));
    body.insert("id".into(), Value::String(d.wire_key.clone()));
    let alias = d.alias.clone().unwrap_or_else(|| d.key.clone());
    body.insert("alias".into(), Value::String(alias));
    if !d.inputs.is_empty() {
        // resolve keys -> wire keys
        let mut ids = Vec::new();
        // need descs lookup — handled by caller via emit pass; we stored wire keys
        // in d.inputs_wire during build (see below)
        for wk in &d.inputs_wire {
            ids.push(Value::String(wk.clone()));
        }
        body.insert("inputNodes".into(), Value::Array(ids));
    }
    // fields partition
    if let Some(fields) = &d.out_fields {
        let (dims, meas) = partition_fields(fields);
        body.insert("fields".into(), Value::Array(fields.clone()));
        body.insert("dimensions".into(), Value::Array(dims));
        body.insert("measures".into(), Value::Array(meas));
    }
    if let Some(of) = sn.get("originalFields") {
        body.insert("originalFields".into(), of.clone());
    } else if ty == "FileInput" {
        // source nodes copy source fields into originalFields by default
        if let Some(fields) = &d.out_fields {
            body.insert("originalFields".into(), Value::Array(fields.clone()));
        }
    }
    if ty == "Select" {
        // steps: typed verbatim / rawStep verbatim
        if let Some(steps) = sn.get("steps").and_then(Value::as_array) {
            let mut out_steps = Vec::new();
            for s in steps {
                if let Some(rs) = s.get("rawStep") {
                    out_steps.push(rs.clone());
                } else {
                    out_steps.push(s.clone());
                }
            }
            body.insert("steps".into(), Value::Array(out_steps));
        } else {
            body.insert("steps".into(), Value::Array(vec![]));
        }
        // stepCount = max(config.stepCount, steps.len)
        let len = sn
            .get("steps")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0) as u64;
        let cfg_sc = sn
            .pointer("/config/stepCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        body.insert("stepCount".into(), Value::from(cfg_sc.max(len)));
    }
    // config verbatim
    if let Some(cfg) = sn.get("config").and_then(Value::as_object) {
        for (k, v) in cfg {
            // stepCount already handled for Select; emit others verbatim
            if ty == "Select" && k == "stepCount" {
                continue;
            }
            body.insert(k.clone(), v.clone());
        }
    }
    // raw merge (collisions validated)
    if let Some(raw) = sn.get("raw").and_then(Value::as_object) {
        for (k, v) in raw {
            body.insert(k.clone(), v.clone());
        }
    }
    Ok(Value::Object(body))
}

pub(crate) fn partition_fields(fields: &[Value]) -> (Vec<Value>, Vec<Value>) {
    let mut dims = Vec::new();
    let mut meas = Vec::new();
    for f in fields {
        let is_dim = f
            .get("isDimension")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if is_dim {
            dims.push(f.clone());
        } else {
            meas.push(f.clone());
        }
    }
    (dims, meas)
}

fn default_output_partition(descs: &[NodeDesc]) -> (Vec<Value>, Vec<Value>) {
    for d in descs {
        if d.ty.as_deref() == Some("Output") && d.wire_key == "default" {
            if let Some(fields) = &d.out_fields {
                return partition_fields(fields);
            }
        }
    }
    (vec![], vec![])
}

fn emit_properties(spec: &Value) -> Result<Value, Diag> {
    let mut m = Map::new();
    if let Some(p) = spec.get("properties").and_then(Value::as_object) {
        // raw first so typed keys win (equal values were validated)
        if let Some(raw) = p.get("raw").and_then(Value::as_object) {
            for (k, v) in raw {
                m.insert(k.clone(), v.clone());
            }
        }
        for (k, _) in TYPED_PROPERTIES {
            if let Some(v) = p.get(*k) {
                m.insert((*k).into(), v.clone());
            }
        }
    }
    m.insert("modelDataType".into(), Value::String("DataFlow".into()));
    Ok(Value::Object(m))
}

fn emit_parameters(p: &Value) -> Result<Value, Diag> {
    let arr = p.as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    for item in arr {
        let mut m = item.as_object().cloned().unwrap_or_default();
        m.remove("raw");
        if let Some(raw) = item.get("raw").and_then(Value::as_object) {
            for (k, v) in raw {
                m.insert(k.clone(), v.clone());
            }
        }
        out.push(Value::Object(m));
    }
    Ok(Value::Array(out))
}

fn emit_subprocesses(sp: &Value) -> Result<Value, Diag> {
    let obj = sp.as_object().cloned().unwrap_or_default();
    let mut out = Map::new();
    for (key, item) in obj {
        let mut m = item.as_object().cloned().unwrap_or_default();
        let raw = m.remove("raw");
        // id defaults to map key
        if !m.contains_key("id") {
            m.insert("id".into(), Value::String(key.clone()));
        }
        if let Some(Value::Object(r)) = raw {
            for (k, v) in r {
                m.insert(k, v);
            }
        }
        out.insert(key, Value::Object(m));
    }
    Ok(Value::Object(out))
}
