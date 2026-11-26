//! `generate` — compile a spec-format v1 document into a `.tbl` wire object.
//!
//! Pipeline: dispatch on form (generated / archive / rawTable) →
//! `validate_spec` (all diagnostics, spec RFC 6901 paths) → `build` (field
//! projection, topo ordering, verbatim merges). `generate` returns the first
//! diagnostic as `Err`; `validate_spec` is exposed for `check` on spec input.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde_json::{Map, Value};

use crate::model::*;

/// Optional stage timings for the generated-form compile path. The public
/// generator never collects these; the hidden profile entry point is for
/// benchmarks only.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedCompileProfile {
    pub validate_ns: u128,
    pub spec_validation_ns: u128,
    pub node_entry_validation_ns: u128,
    pub prepare_ns: u128,
    pub topo_sort_ns: u128,
    pub graph_semantics_ns: u128,
    pub field_compute_ns: u128,
    pub wire_emit_ns: u128,
    pub wire_check_ns: u128,
    pub diag_remap_ns: u128,
    pub nodes: usize,
    pub fields: usize,
    pub steps: usize,
    pub input_edges: usize,
    pub wire_diagnostics: usize,
    pub mapped_diagnostics: usize,
}

fn profile_start(profile: &Option<&mut GeneratedCompileProfile>) -> Option<Instant> {
    profile.as_ref().map(|_| Instant::now())
}

macro_rules! profile_stage {
    ($profile:ident, $field:ident, $start:expr) => {
        if let Some(__start) = $start {
            if let Some(__p) = $profile.as_mut().map(|__p| &mut **__p) {
                __p.$field += __start.elapsed().as_nanos();
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Compile a spec document, returning the wire value (when compilation
/// succeeds) plus ALL diagnostics — warnings included. Single entry for
/// the `gen`/`new` CLI so validation and emission share one compile pass.
pub fn generate_with_diagnostics(spec: &Value) -> (Option<Value>, Vec<Diag>) {
    generate_impl(spec, None)
}

/// Benchmark-only entry point: same diagnostics and wire output as
/// [`generate_with_diagnostics`], plus generated-form stage timings.
#[doc(hidden)]
pub fn generate_with_diagnostics_profile(
    spec: &Value,
) -> (Option<Value>, Vec<Diag>, GeneratedCompileProfile) {
    let mut profile = GeneratedCompileProfile::default();
    let (wire, diags) = generate_impl(spec, Some(&mut profile));
    (wire, diags, profile)
}

fn generate_impl(
    spec: &Value,
    profile: Option<&mut GeneratedCompileProfile>,
) -> (Option<Value>, Vec<Diag>) {
    let mut diags = Vec::new();
    if !spec.is_object() {
        diags.push(Diag::spec(
            "",
            "structure.spec",
            "spec must be a JSON object",
        ));
        return (None, diags);
    }
    match spec.get("format") {
        Some(v) if v.as_u64() == Some(1) => {}
        _ => {
            diags.push(Diag::spec(
                "/format",
                "structure.format",
                "spec.format must be 1",
            ));
            return (None, diags);
        }
    }
    let has_archive = spec.get("archive").is_some();
    let has_rawtable = spec.get("rawTable").is_some();
    let has_nodes = spec.get("nodes").is_some();
    match (has_archive, has_rawtable, has_nodes) {
        (true, false, false) => {
            validate_archive(spec, &mut diags);
            if diags.iter().any(|d| !d.is_warning()) {
                return (None, diags);
            }
            match build_archive(spec) {
                Ok(tbl) => (Some(tbl), diags),
                Err(d) => {
                    diags.push(d);
                    (None, diags)
                }
            }
        }
        (false, true, false) => {
            validate_rawtable(spec, &mut diags);
            if diags.iter().any(|d| !d.is_warning()) {
                return (None, diags);
            }
            match build_rawtable(spec) {
                Ok(tbl) => (Some(tbl), diags),
                Err(d) => {
                    diags.push(d);
                    (None, diags)
                }
            }
        }
        (false, false, true) => {
            let wire = compile_generated(spec, &mut diags, profile);
            (wire, diags)
        }
        _ => {
            diags.push(Diag::spec(
                "",
                "structure.form",
                "spec must contain exactly one of: nodes (generated), archive, rawTable",
            ));
            (None, diags)
        }
    }
}

/// Compile a spec document to a `.tbl` wire `Value`.
pub fn generate(spec: &Value) -> Result<Value, Diag> {
    let (wire, diags) = generate_with_diagnostics(spec);
    match wire {
        Some(w) => Ok(w),
        None => Err(diags
            .into_iter()
            .find(|d| !d.is_warning())
            .unwrap_or_else(|| Diag::spec("", "structure.spec", "spec failed to compile"))),
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
        (false, false, true) => {
            compile_generated(spec, &mut d, None);
        }
        _ => d.push(Diag::spec(
            "",
            "structure.form",
            "spec must contain exactly one of: nodes (generated), archive, rawTable",
        )),
    }
    d
}

/// Single compile path for the generated form: structural/semantic
/// validation, one build pass collecting diagnostics, then the pure wire
/// `check` so errors only visible on the wire (rawNode bodies, emitted step
/// payloads, top-level partition) surface with spec paths. Returns the wire
/// document when no hard diagnostic was produced.
fn compile_generated(
    spec: &Value,
    diags: &mut Vec<Diag>,
    mut profile: Option<&mut GeneratedCompileProfile>,
) -> Option<Value> {
    // one prepare pass shared by validation, build and wire-diag mapping
    let validate_start = profile_start(&profile);
    let descs = validate_generated_inner(spec, diags, profile.as_mut().map(|p| &mut **p));
    profile_stage!(profile, validate_ns, validate_start);
    if diags.iter().any(|d| !d.is_warning()) {
        return None;
    }
    let (wire, descs) =
        build_generated_collect_inner(spec, descs, diags, profile.as_mut().map(|p| &mut **p));
    let Some(wire) = wire else {
        return None;
    };
    // wire invariants: pure check, no filesystem, no recursion into generate
    let check_start = profile_start(&profile);
    let wire_diags = crate::check::check(&wire);
    profile_stage!(profile, wire_check_ns, check_start);
    if let Some(p) = profile.as_mut().map(|p| &mut **p) {
        p.wire_diagnostics = wire_diags.len();
    }
    if wire_diags.is_empty() {
        return Some(wire);
    }
    let remap_start = profile_start(&profile);
    let mut mapped = 0usize;
    for wd in wire_diags {
        // warnings the spec side already reports with better paths
        if matches!(
            wd.rule.as_str(),
            "warn.properties.runMode.unset"
                | "warn.properties.runMode.noTable"
                | "warn.extract.dbfield"
        ) {
            continue;
        }
        diags.push(map_wire_diag(wd, &descs));
        mapped += 1;
    }
    profile_stage!(profile, diag_remap_ns, remap_start);
    if let Some(p) = profile.as_mut().map(|p| &mut **p) {
        p.mapped_diagnostics = mapped;
    }
    if diags.iter().any(|d| !d.is_warning()) {
        None
    } else {
        Some(wire)
    }
}

/// Translate a wire `.tbl` diagnostic path into the spec path that produced
/// it. Node bodies map by wire map key; generated keys keep their tail,
/// config keys route under `/config`, everything else under `/raw` (or
/// `/rawNode` for rawNode entries whose body is verbatim).
fn map_wire_diag(wd: Diag, descs: &[NodeDesc]) -> Diag {
    let p = wd.path;
    let remap = |path: String| Diag {
        path,
        rule: wd.rule.clone(),
        source: wd.source.clone(),
        message: wd.message.clone(),
    };
    if let Some(rest) = p.strip_prefix("/dataFlow/nodes/") {
        let (wk, tail) = match rest.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (rest, None),
        };
        // wk is the escaped path segment from the wire pointer
        if let Some(d) = descs.iter().find(|d| ptr_escape(&d.wire_key) == wk) {
            let base = format!("/nodes/{}", d.idx);
            let spec_path = if d.raw_body.is_some() {
                match tail {
                    None => base,
                    Some(t) => format!("{}/rawNode/{}", base, t),
                }
            } else {
                match tail {
                    None => base,
                    Some(t) => {
                        let seg = t.split('/').next().unwrap_or("");
                        if GENERATED_NODE_KEYS.contains(&seg) {
                            // a wire step that decompiles to rawStep lives
                            // under steps/N/rawStep in the spec
                            if let Some((n, rest)) =
                                t.strip_prefix("steps/").and_then(|r| r.split_once('/'))
                            {
                                if let Ok(si) = n.parse::<usize>() {
                                    let is_raw = d
                                        .spec
                                        .and_then(|s| s.get("steps"))
                                        .and_then(Value::as_array)
                                        .and_then(|a| a.get(si))
                                        .map(|s| s.get("rawStep").is_some())
                                        .unwrap_or(false);
                                    if is_raw {
                                        return remap(format!(
                                            "{}/steps/{}/rawStep/{}",
                                            base, si, rest
                                        ));
                                    }
                                }
                            }
                            format!("{}/{}", base, t)
                        } else if seg == "inputNodes" {
                            format!("{}/inputs/{}", base, &t["inputNodes/".len()..])
                        } else {
                            // route by where the spec actually put the key:
                            // config first, then raw, then the type whitelist
                            let in_cfg = d
                                .spec
                                .and_then(|s| s.get("config"))
                                .and_then(Value::as_object)
                                .map(|c| c.contains_key(seg))
                                .unwrap_or(false);
                            let in_raw = d
                                .spec
                                .and_then(|s| s.get("raw"))
                                .and_then(Value::as_object)
                                .map(|r| r.contains_key(seg))
                                .unwrap_or(false);
                            if in_cfg
                                || (!in_raw
                                    && config_key_allowed(d.ty.as_deref().unwrap_or(""), seg))
                            {
                                format!("{}/config/{}", base, t)
                            } else {
                                format!("{}/raw/{}", base, t)
                            }
                        }
                    }
                }
            };
            return remap(spec_path);
        }
        return remap("/nodes".to_string());
    }
    if let Some(tail) = p.strip_prefix("/dataFlow/subProcesses") {
        return remap(format!("/subProcesses{}", tail));
    }
    if p.starts_with("/dataFlow") {
        return remap("/nodes".to_string());
    }
    // top-level dimensions/measures have no spec counterpart: they derive
    // from the default Output — point at the node list.
    if p.starts_with("/dimensions") || p.starts_with("/measures") {
        return remap("/nodes".to_string());
    }
    remap(p)
}

// ---------------------------------------------------------------------------
// Node preparation: keys, ids, topo
// ---------------------------------------------------------------------------

struct NodeDesc<'a> {
    idx: usize,
    key: String,
    /// resolved wire map key / node id (typed nodes); for rawNode = spec key.
    /// `DwTableLoader` routes `inputNodes` through the nodes-map key, so this
    /// is the only identifier consumers may reference — a rawNode body's own
    /// `id` is verbatim payload reserved for duplicate detection only.
    wire_key: String,
    /// spec node object (typed) — None for rawNode entries
    spec: Option<&'a Value>,
    /// rawNode body for raw entries
    raw_body: Option<&'a Value>,
    ty: Option<String>,
    inputs: Vec<String>,
    /// resolved wire node ids for `inputs`, filled post-resolution
    inputs_wire: Vec<String>,
    alias: Option<String>,
    /// computed output fields (wire-shaped), filled during build
    out_fields: Option<Vec<Value>>,
    /// per-step `originFieldInfo` hints for typed Refactor renames,
    /// parallel to the spec `steps` array (Select only)
    step_hints: Vec<Option<Value>>,
}

fn node_ty(spec_node: &Value) -> Option<String> {
    spec_node
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn prepare_nodes_inner<'a>(
    spec: &'a Value,
    diags: &mut Vec<Diag>,
    profile: Option<&mut GeneratedCompileProfile>,
) -> Vec<NodeDesc<'a>> {
    // borrow the spec's node array (lifetime 'a) — do not clone
    let nodes: &[Value] = spec
        .get("nodes")
        .and_then(Value::as_array)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let mut descs: Vec<NodeDesc> = Vec::new();
    let mut keys: HashSet<String> = HashSet::new();

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
            ty: node_ty(n).or_else(|| {
                n.get("rawNode")
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            }),
            inputs: Vec::new(),
            inputs_wire: Vec::new(),
            alias: n
                .get("alias")
                .and_then(Value::as_str)
                .or_else(|| {
                    n.get("rawNode")
                        .and_then(|b| b.get("alias"))
                        .and_then(Value::as_str)
                })
                .map(str::to_string),
            out_fields: None,
            step_hints: Vec::new(),
        });
    }

    // second pass: inputs + resolved ids
    let key_set: HashSet<String> = descs.iter().map(|d| d.key.clone()).collect();
    // first declared Output index (typed only)
    let first_output_idx = descs
        .iter()
        .position(|d| d.spec.is_some() && d.ty.as_deref() == Some("Output"));
    // single owner map over every wire-visible identifier: typed resolved
    // ids, rawNode spec keys (nodes map keys) and rawNode body ids. The same
    // owner may claim the same identifier twice (e.g. key == body.id).
    let mut owners: HashMap<String, usize> = HashMap::new();
    let mut claim = |id: &str, owner: usize, path: String, diags: &mut Vec<Diag>| {
        if let Some(&prev) = owners.get(id) {
            if prev != owner {
                diags.push(Diag::spec(
                    path,
                    "structure.node.id.duplicate",
                    format!("wire node id {:?} already owned by node {}", id, prev),
                ));
            }
        } else {
            owners.insert(id.to_string(), owner);
        }
    };

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
            d.wire_key = resolved.clone();
            claim(&resolved, d.idx, format!("/nodes/{}/id", d.idx), diags);
        } else {
            // rawNode: wire map key = spec key, body untouched. The body's
            // own `id` is reserved for the duplicate-owners check only —
            // consumers route through the map key, never through body.id.
            d.wire_key = d.key.clone();
            let body_id = d
                .raw_body
                .and_then(|b| b.get("id"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            claim(&d.key, d.idx, format!("/nodes/{}/key", d.idx), diags);
            if let Some(bid) = body_id {
                if bid != d.key {
                    claim(&bid, d.idx, format!("/nodes/{}/rawNode/id", d.idx), diags);
                }
            }
        }
    }

    // resolve input keys -> wire map keys now that every wire_key is known
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
    let topo_start = profile_start(&profile);
    topo_sort(&mut descs, diags);
    if let Some(p) = profile {
        if let Some(start) = topo_start {
            p.topo_sort_ns += start.elapsed().as_nanos();
        }
    }
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
            step_hints: self.step_hints.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Spec validation
// ---------------------------------------------------------------------------

/// Validate the generated form, returning the prepared node descriptors
/// (topo-sorted, wire keys resolved) for reuse by build + diag mapping.
/// Empty when structural validation failed before preparation.
fn validate_generated_inner<'a>(
    spec: &'a Value,
    diags: &mut Vec<Diag>,
    mut profile: Option<&mut GeneratedCompileProfile>,
) -> Vec<NodeDesc<'a>> {
    let spec_start = profile_start(&profile);
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
    profile_stage!(profile, spec_validation_ns, spec_start);

    let nodes = spec.get("nodes").and_then(Value::as_array);
    let Some(nodes) = nodes else {
        diags.push(Diag::spec(
            "/nodes",
            "structure.nodes",
            "nodes must be an array",
        ));
        return Vec::new();
    };

    let entry_start = profile_start(&profile);
    // per-node structural validation
    for (i, n) in nodes.iter().enumerate() {
        validate_node_entry(n, i, diags);
    }
    profile_stage!(profile, node_entry_validation_ns, entry_start);

    // graph + semantic validation needs prepared descriptors
    let mut graph_diags = Vec::new();
    let prepare_start = profile_start(&profile);
    let descs = prepare_nodes_inner(spec, &mut graph_diags, profile.as_mut().map(|p| &mut **p));
    profile_stage!(profile, prepare_ns, prepare_start);
    diags.extend(graph_diags);
    let graph_start = profile_start(&profile);
    validate_graph_semantics(spec, &descs, diags);
    profile_stage!(profile, graph_semantics_ns, graph_start);
    descs
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
        // Join fields may be name-only references resolved against the
        // inputs at build time (metadata is merged from the predecessor);
        // every other node type declares complete fields.
        validate_field_array(v, &format!("{}/fields", path), ty == "Join", diags);
    }
    if let Some(v) = n.get("originalFields") {
        validate_field_array(v, &format!("{}/originalFields", path), false, diags);
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

fn validate_field_array(v: &Value, path: &str, partial: bool, diags: &mut Vec<Diag>) {
    let Some(arr) = v.as_array() else {
        diags.push(Diag::spec(
            path,
            "structure.fields",
            "fields must be an array",
        ));
        return;
    };
    for (i, f) in arr.iter().enumerate() {
        validate_field(f, &format!("{}/{}", path, i), partial, diags);
    }
}

fn validate_field(f: &Value, path: &str, partial: bool, diags: &mut Vec<Diag>) {
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
        // partial declarations (Join field refs) require only `name`;
        // dataType/isDimension merge from the resolved predecessor.
        if partial && k == "dataType" {
            continue;
        }
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
        // closed object/array payloads (per-type only)
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
                            } else if !co[k].is_string() {
                                diags.push(Diag::spec(
                                    format!("{}/{}", cp, k),
                                    "structure.config.type",
                                    format!("clause.{} must be a string", k),
                                ));
                            }
                        }
                        if co
                            .get("exp")
                            .and_then(Value::as_str)
                            .is_some_and(|s| !s.trim().is_empty())
                        {
                            continue;
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
        ClosedKind::Clauses => {
            // incrementRange[]: clause objects; the loader accepts arbitrary
            // keys per clause (ClauseFilterImpl.loadJsonMap). Typed specs
            // reject null — only the wire tolerates it as absence.
            let Some(arr) = v.as_array() else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "incrementRange must be an array of clause objects",
                ));
                return;
            };
            for (i, item) in arr.iter().enumerate() {
                if !item.is_object() {
                    diags.push(Diag::spec(
                        format!("{}/{}", path, i),
                        "structure.config",
                        "incrementRange entries must be objects",
                    ));
                }
            }
        }
        ClosedKind::Condition => {
            // filter object: {matchAll?,enabled?,clauses?:[object]}.
            // Typed specs reject null — only the wire tolerates it.
            let Some(o) = v.as_object() else {
                diags.push(Diag::spec(
                    path,
                    "structure.config",
                    "filter must be an object",
                ));
                return;
            };
            for k in o.keys() {
                if !["matchAll", "enabled", "clauses"].contains(&k.as_str()) {
                    diags.push(Diag::spec(
                        format!("{}/{}", path, ptr_escape(k)),
                        "structure.config.key",
                        "unexpected filter key",
                    ));
                }
            }
            for k in ["matchAll", "enabled"] {
                if let Some(x) = o.get(k) {
                    if !x.is_boolean() {
                        diags.push(Diag::spec(
                            format!("{}/{}", path, k),
                            "structure.config.type",
                            format!("filter.{} must be a boolean", k),
                        ));
                    }
                }
            }
            if let Some(cl) = o.get("clauses") {
                match cl.as_array() {
                    Some(arr) => {
                        for (i, item) in arr.iter().enumerate() {
                            if !item.is_object() {
                                diags.push(Diag::spec(
                                    format!("{}/clauses/{}", path, i),
                                    "structure.config",
                                    "filter.clauses entries must be objects",
                                ));
                            }
                        }
                    }
                    None => diags.push(Diag::spec(
                        format!("{}/clauses", path),
                        "structure.config",
                        "filter.clauses must be an array",
                    )),
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
                        if !seen.insert(s.to_uppercase()) {
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
/// every step but AddField needs a non-empty fields operand (Java applies a
/// step once per operand, so an empty one silently skips it);
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
    if ty != "AddField" && fields.is_empty() {
        diags.push(Diag::spec(
            format!("{}/fields", path),
            "field.required",
            format!(
                "{} step requires a non-empty fields operand (Java skips the step otherwise)",
                ty
            ),
        ));
    }
    match ty {
        "AddField" => {
            // fields is the platform's own operand selection: omitted is
            // synthesized as [addField.name] at emission; an explicit
            // non-empty array is preserved verbatim (it may legitimately
            // differ — the wire checker only requires it be non-empty).
            if let Some(Value::Array(a)) = obj.get("fields") {
                if a.is_empty() {
                    diags.push(Diag::spec(
                        format!("{}/fields", path),
                        "field.required",
                        "AddField.fields must be non-empty when present",
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
                .map(|n| n.to_uppercase())
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
            "Agg" | "Distinct" => {
                // the contributors read the FIRST input only — extra inputs
                // would be silently ignored
                if n != 1 {
                    diags.push(Diag::spec(
                        arity_path,
                        "err.dataflow.node.input",
                        format!("{} requires exactly 1 input, found {}", ty, n),
                    ));
                }
            }
            "RowToCol" | "ColToRow" | "Snapshot" | "Update" => {
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
        // raw collision with generated keys AND config keys: both are
        // emitted onto the same wire object, so an overlap would silently
        // overwrite regardless of whether the values agree.
        if let Some(raw) = sn.get("raw").and_then(Value::as_object) {
            let cfg = sn.get("config").and_then(Value::as_object);
            for k in raw.keys() {
                if GENERATED_NODE_KEYS.contains(&k.as_str())
                    || cfg.is_some_and(|c| c.contains_key(k))
                {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/raw/{}", d.idx, ptr_escape(k)),
                        "structure.node.raw.collision",
                        format!(
                            "raw key {:?} collides with a generated node or config key",
                            k
                        ),
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

    // a runnable flow needs a default Output (typed or rawNode)
    if !descs.iter().any(is_default_output) {
        diags.push(Diag::spec(
            "/nodes",
            "flow.noDefaultOutput",
            "no node resolves to the default Output (id \"default\")",
        ));
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
        if matches!(ty, "SplitField" | "Text2Id") {
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

/// Evolve a name list through one typed step (names only). Delegates to the
/// shared `evolve_names`; `Err` means a step-time name collision (reported
/// separately by the caller), `Ok(false)` means the shape is unprovable.
fn step_evolve_names(names: &mut Vec<String>, step: &Value) -> bool {
    match evolve_names(names, step) {
        Ok(true) => true,
        _ => false,
    }
}

/// Resolve a node's output field names when provable (explicit fields,
/// Output identity clone, or Select step simulation; a rawNode contributes
/// its verbatim body's `fields` when present). None = unknown.
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
    let Some(sn) = d.spec else {
        // rawNode: the verbatim body's fields are the only provable names
        return d.raw_body.and_then(|b| b.get("fields")).map(field_names_of);
    };
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
                        if ty != "RemoveField" && !current.iter().any(|c| field_eq(c, name)) {
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
                let upper = name.to_uppercase();
                let known =
                    current.iter().any(|c| field_eq(c, &name)) || param_names.contains(&upper);
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
        match evolve_names(&mut current, st) {
            Ok(true) => {}
            Ok(false) => break, // tracking lost
            Err(tag) => {
                // step-time name collision (AddField/Refactor rename): the
                // platform would produce two fields with the same name
                diags.push(Diag::spec(
                    format!("/nodes/{}/steps/{}/{}", d.idx, si, tag),
                    "field.duplicate",
                    format!("step {} creates a duplicate field name", ty),
                ));
                break;
            }
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
                match evolve_names(&mut names, st) {
                    Ok(true) => {}
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            ok
        }
        None => true,
    };
    if !derivable {
        return;
    }
    // multiset comparison, order-insensitive, Unicode-aware field_eq —
    // mirrors QueryImpl name semantics (toUpperCase)
    let exp_names: Vec<String> = explicit
        .iter()
        .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let mut remaining = names.clone();
    let mut matched = true;
    'outer: for en in &exp_names {
        for (i, dn) in remaining.iter().enumerate() {
            if field_eq(en, dn) {
                remaining.remove(i);
                continue 'outer;
            }
        }
        matched = false;
        break;
    }
    if !matched || !remaining.is_empty() {
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
                    if !pred_names.iter().any(|p| field_eq(p, nm)) {
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
    // Java resolves leftTable/rightTable against node ALIASES only — the
    // spec key is not an alias unless it was declared as one.
    let mut aliases: HashSet<String> = HashSet::new();
    for inp in &d.inputs {
        if let Some(a) = spec_node_alias(descs, inp) {
            aliases.insert(a.to_string());
        }
    }
    for (i, c) in conds.iter().enumerate() {
        for side in ["leftTable", "rightTable"] {
            if let Some(t) = c.get(side).and_then(Value::as_str) {
                if !t.is_empty() && !aliases.iter().any(|a| field_eq(a, t)) {
                    diags.push(Diag::spec(
                        format!("/nodes/{}/config/joinConditions/{}/{}", d.idx, i, side),
                        "join.condition.alias",
                        format!("{} {:?} does not match any input node alias", side, t),
                    ));
                }
            }
        }
        // clause AND condition-level expressions qualify fields as
        // [alias].[field]; a qualifier that is an input's spec KEY (but not
        // its alias) resolves to nothing at runtime — reject it. Quoted
        // literals are skipped by the shared scanner.
        let mut scan = |e: &str, epath: String| {
            for q in exp_table_qualifiers(e) {
                let is_key = d.inputs.iter().any(|k| field_eq(k, &q));
                let is_alias = d
                    .inputs
                    .iter()
                    .filter_map(|k| spec_node_alias(descs, k))
                    .any(|a| field_eq(a, &q));
                if is_key && !is_alias {
                    diags.push(Diag::spec(
                        epath.clone(),
                        "join.condition.qualifier",
                        format!("expression qualifier {:?} is a node key, not an alias", q),
                    ));
                }
            }
        };
        if let Some(e) = c.get("exp").and_then(Value::as_str) {
            scan(
                e,
                format!("/nodes/{}/config/joinConditions/{}/exp", d.idx, i),
            );
        }
        for (ci, cl) in c
            .get("clauses")
            .and_then(Value::as_array)
            .map(|a| a.iter().enumerate().collect::<Vec<_>>())
            .unwrap_or_default()
        {
            for ek in ["leftExp", "rightExp", "exp"] {
                if let Some(e) = cl.get(ek).and_then(Value::as_str) {
                    scan(
                        e,
                        format!(
                            "/nodes/{}/config/joinConditions/{}/clauses/{}/{}",
                            d.idx, i, ci, ek
                        ),
                    );
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

/// Computed output fields for one node plus per-step emission hints.
struct ComputedNode {
    fields: Vec<Value>,
    /// `originFieldInfo` hints parallel to the spec `steps` array
    /// (Select Refactor renames only; empty for other node types)
    step_hints: Vec<Option<Value>>,
}

/// Compute output fields in topo order; returns the field-annotated
/// descriptors for wire-diag mapping.
fn compute_generated_fields<'a>(
    descs: Vec<NodeDesc<'a>>,
    diags: &mut Vec<Diag>,
) -> Vec<NodeDesc<'a>> {
    let key_to_idx: HashMap<String, usize> = descs
        .iter()
        .enumerate()
        .map(|(i, d)| (d.key.clone(), i))
        .collect();
    let mut descs = descs;
    for i in 0..descs.len() {
        let (cf, err) = compute_node_fields(&descs, i, &key_to_idx);
        match (cf, err) {
            (Some(c), _) => {
                descs[i].out_fields = Some(c.fields);
                descs[i].step_hints = c.step_hints;
            }
            (None, Some(e)) => diags.push(e),
            (None, None) => {}
        }
    }
    descs
}

/// Emit the wire document from field-annotated descriptors; returns the wire
/// value alongside the descriptors for wire-diag mapping.
fn emit_generated_wire<'a>(
    spec: &Value,
    descs: Vec<NodeDesc<'a>>,
    diags: &mut Vec<Diag>,
) -> (Option<Value>, Vec<NodeDesc<'a>>) {
    // emit nodes
    let mut nodes_map = Map::new();
    for d in &descs {
        let body = if let Some(rn) = d.raw_body {
            rn.clone() // verbatim, never mutated
        } else {
            match emit_typed_node(d) {
                Ok(b) => b,
                Err(e) => {
                    diags.push(e);
                    continue;
                }
            }
        };
        nodes_map.insert(d.wire_key.clone(), body);
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return (None, descs);
    }

    // dataFlow
    let mut flow = Map::new();
    flow.insert("nodes".into(), Value::Object(nodes_map));
    if let Some(sp) = spec.get("subProcesses") {
        match emit_subprocesses(sp) {
            Ok(v) => {
                flow.insert("subProcesses".into(), v);
            }
            Err(e) => {
                diags.push(e);
                return (None, descs);
            }
        }
    }

    // top-level
    let mut tbl = Map::new();
    tbl.insert(
        "version".into(),
        spec.get("version")
            .cloned()
            .unwrap_or_else(|| Value::from(SAVE_VERSION)),
    );
    match emit_properties(spec) {
        Ok(v) => {
            tbl.insert("properties".into(), v);
        }
        Err(e) => {
            diags.push(e);
            return (None, descs);
        }
    }
    if let Some(p) = spec.get("parameters") {
        match emit_parameters(p) {
            Ok(v) => {
                tbl.insert("parameters".into(), v);
            }
            Err(e) => {
                diags.push(e);
                return (None, descs);
            }
        }
    }
    // top dimensions/measures from the default Output; empty partitions are
    // omitted entirely (the platform writes no empty arrays)
    let (dims, measures) = default_output_partition(&descs, diags);
    // extraction needs a physical column name per top field; the wire check
    // flags it on /dimensions//measures — report it here on the spec side,
    // routed to the default Output's fields.
    let extract = spec
        .pointer("/properties/extractDataEnabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if extract {
        if let Some(d) = descs.iter().find(|d| is_default_output(d)) {
            if let Some(fields) = &d.out_fields {
                for (fi, f) in fields.iter().enumerate() {
                    let ok = f
                        .get("dbfield")
                        .and_then(Value::as_str)
                        .map(|s| !s.trim().is_empty() && s.chars().all(|c| c.is_ascii()))
                        .unwrap_or(false);
                    if !ok {
                        diags.push(Diag::spec(
                            format!("/nodes/{}/fields/{}/dbfield", d.idx, fi),
                            "warn.extract.dbfield",
                            "extractDataEnabled is set but this output field has no ASCII dbfield",
                        ));
                    }
                }
            }
        }
    }
    if !dims.is_empty() {
        tbl.insert("dimensions".into(), Value::Array(dims));
    }
    if !measures.is_empty() {
        tbl.insert("measures".into(), Value::Array(measures));
    }
    tbl.insert("dataFlow".into(), Value::Object(flow));
    // top raw merge (collisions already validated)
    if let Some(raw) = spec.get("raw").and_then(Value::as_object) {
        for (k, v) in raw {
            tbl.insert(k.clone(), v.clone());
        }
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return (None, descs);
    }
    (Some(Value::Object(tbl)), descs)
}

/// Build the wire document from already-prepared descriptors; returns the
/// (possibly field-annotated) descriptors alongside for wire-diag mapping.
fn build_generated_collect_inner<'a>(
    spec: &Value,
    descs: Vec<NodeDesc<'a>>,
    diags: &mut Vec<Diag>,
    mut profile: Option<&mut GeneratedCompileProfile>,
) -> (Option<Value>, Vec<NodeDesc<'a>>) {
    let field_start = profile_start(&profile);
    let descs = compute_generated_fields(descs, diags);
    profile_stage!(profile, field_compute_ns, field_start);
    if let Some(p) = profile.as_mut().map(|p| &mut **p) {
        p.nodes = descs.len();
        p.input_edges = descs.iter().map(|d| d.inputs.len()).sum();
        p.fields = descs
            .iter()
            .map(|d| d.out_fields.as_ref().map_or(0, Vec::len))
            .sum();
        p.steps = descs
            .iter()
            .filter_map(|d| d.spec)
            .filter_map(|sn| sn.get("steps").and_then(Value::as_array))
            .map(Vec::len)
            .sum();
    }
    if diags.iter().any(|d| !d.is_warning()) {
        return (None, descs);
    }

    let emit_start = profile_start(&profile);
    let result = emit_generated_wire(spec, descs, diags);
    profile_stage!(profile, wire_emit_ns, emit_start);
    result
}

/// Resolve a spec input key to its prepared descriptor.
fn input_desc<'a>(
    d: &NodeDesc,
    descs: &'a [NodeDesc<'a>],
    key_to_idx: &HashMap<String, usize>,
    pos: usize,
) -> Option<&'a NodeDesc<'a>> {
    let key = d.inputs.get(pos)?;
    let &i = key_to_idx.get(key)?;
    Some(&descs[i])
}

/// The alias consumers see for a node (typed: `alias` or key; rawNode: body
/// alias or spec key).
fn node_alias(d: &NodeDesc) -> String {
    if let Some(a) = &d.alias {
        return a.clone();
    }
    if let Some(b) = d.raw_body {
        if let Some(a) = b.get("alias").and_then(Value::as_str) {
            return a.to_string();
        }
    }
    d.key.clone()
}

/// Whether the node is a lineage source, including rawNode bodies whose
/// `type` says FileInput/ModelTable/etc.
fn node_is_source(d: &NodeDesc) -> bool {
    let ty =
        d.ty.as_deref()
            .or_else(|| {
                d.raw_body
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("");
    is_source(ty)
}

/// Proven root lineage of a predecessor field: the oldest `(node, field)`
/// pair we can stand behind. Computed fields (nonblank `exp`) and
/// pivot-born/unmatched columns carry NO origin — never fabricated. A
/// source's own fields root at the source itself, so stale explicit
/// `original*` on them is rewritten rather than trusted.
fn field_origin(f: &Value, pred: &NodeDesc) -> Option<(String, String)> {
    let nonblank = |k: &str| {
        f.get(k)
            .and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    };
    if nonblank("exp") {
        return None;
    }
    if node_is_source(pred) {
        let name = f.get("name").and_then(Value::as_str).unwrap_or("");
        if !name.is_empty() {
            return Some((node_alias(pred), name.to_string()));
        }
        return None;
    }
    if nonblank("originalNode") && nonblank("originalField") {
        return Some((
            f["originalNode"].as_str().unwrap_or("").to_string(),
            f["originalField"].as_str().unwrap_or("").to_string(),
        ));
    }
    None
}

/// Field keys that carry computed/lineage meaning and are always enforced
/// by the projection rather than trusted from declarations.
const ENFORCED_FIELD_KEYS: [&str; 6] = [
    "name",
    "inputField",
    "inputNode",
    "originalField",
    "originalNode",
    "exp",
];

/// Merge a declared field with its resolved predecessor field: upstream
/// metadata (dataType, dbfield, isDimension, length, ...) forms the base,
/// declared non-lineage keys override it, and direct lineage is set from the
/// resolution — `inputField` = predecessor field name, `inputNode` = input
/// alias (when `emit_input_node`), `original*` = proven origin only.
fn merge_declared(
    declared: &Value,
    pred_field: &Value,
    input_field: &str,
    input_node: Option<&str>,
    origin: Option<(String, String)>,
) -> Value {
    let mut m = pred_field.as_object().cloned().unwrap_or_default();
    for k in [
        "name",
        "exp",
        "aggType",
        "inputField",
        "inputNode",
        "originalField",
        "originalNode",
    ] {
        m.remove(k);
    }
    if let Some(do_) = declared.as_object() {
        for (k, v) in do_ {
            if ENFORCED_FIELD_KEYS.contains(&k.as_str()) {
                continue;
            }
            m.insert(k.clone(), v.clone());
        }
    }
    if let Some(nm) = declared.get("name") {
        m.insert("name".into(), nm.clone());
    }
    m.insert("inputField".into(), Value::String(input_field.to_string()));
    if let Some(ia) = input_node {
        m.insert("inputNode".into(), Value::String(ia.to_string()));
    }
    if let Some((on, of)) = origin {
        m.insert("originalNode".into(), Value::String(on));
        m.insert("originalField".into(), Value::String(of));
    }
    Value::Object(m)
}

/// A field born by computation (`exp` present): emit the declaration minus
/// lineage and inputNode — computed values have no proven single input.
fn emit_computed(declared: &Value) -> Value {
    let mut m = declared.as_object().cloned().unwrap_or_default();
    for k in ["originalNode", "originalField", "inputNode"] {
        m.remove(k);
    }
    Value::Object(m)
}

/// Strip unproven lineage from a declared field that could not be resolved
/// (opaque reshaping shapes, hidden Union members): keep the declaration —
/// including any explicit direct refs — but drop the original* pair.
fn emit_unresolved(declared: &Value) -> Value {
    let mut m = declared.as_object().cloned().unwrap_or_default();
    m.remove("originalNode");
    m.remove("originalField");
    Value::Object(m)
}

fn has_exp(f: &Value) -> bool {
    f.get("exp")
        .and_then(Value::as_str)
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

fn field_str<'a>(f: &'a Value, k: &str) -> Option<&'a str> {
    f.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The name a field is matched by: declared `inputField` when present else
/// `name` (Join/Union/Distinct resolution semantics).
fn field_match_name(f: &Value) -> &str {
    field_str(f, "inputField")
        .or_else(|| field_str(f, "name"))
        .unwrap_or("")
}

/// Find a predecessor field by name (Unicode-aware).
fn find_pred_field<'a>(fields: &'a [Value], want: &str) -> Option<&'a Value> {
    fields.iter().find(|pf| {
        pf.get("name")
            .and_then(Value::as_str)
            .map(|n| field_eq(n, want))
            .unwrap_or(false)
    })
}

fn compute_node_fields(
    descs: &[NodeDesc],
    i: usize,
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let d = &descs[i];
    // rawNode: fields come from the verbatim body when present (absent =
    // unknown shape; the default-Output partition check reports that)
    if d.spec.is_none() {
        let known = d.raw_body.and_then(|b| b.get("fields")).is_some();
        let fields = d
            .raw_body
            .and_then(|b| b.get("fields"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        return (
            known.then(|| ComputedNode {
                fields,
                step_hints: Vec::new(),
            }),
            None,
        );
    }
    let sn = d.spec.unwrap();
    let ty = d.ty.as_deref().unwrap_or("");
    // sources: declared fields are the root of lineage — each ordinary
    // field's original* pair is seeded/rewritten to (own alias, own name);
    // computed (exp) fields shed it. FileInput additionally gets the
    // identity inputField fill; ModelTable keeps no inputField.
    if is_source(ty) || ty == "SubProcess" {
        let Some(f) = sn.get("fields") else {
            return (None, None);
        };
        let mut fields = f.as_array().cloned().unwrap_or_default();
        if is_source(ty) {
            let alias = node_alias(d);
            for f in fields.iter_mut() {
                let computed = has_exp(f);
                let Some(m) = f.as_object_mut() else {
                    continue;
                };
                let name = m
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if computed || name.is_empty() {
                    m.remove("originalNode");
                    m.remove("originalField");
                } else {
                    m.insert("originalNode".into(), Value::String(alias.clone()));
                    m.insert("originalField".into(), Value::String(name));
                }
            }
        }
        if ty == "FileInput" {
            fill_fileinput_identity(&mut fields);
        }
        return (
            Some(ComputedNode {
                fields,
                step_hints: Vec::new(),
            }),
            None,
        );
    }
    match ty {
        "Select" => compute_select_fields(d, descs, key_to_idx),
        "Output" => compute_output_fields(d, descs, key_to_idx),
        "Distinct" => compute_distinct_fields(d, descs, key_to_idx),
        "Join" => compute_join_fields(d, descs, key_to_idx),
        "Union" => compute_union_fields(d, descs, key_to_idx),
        _ => compute_reshaped_fields(d, descs, key_to_idx),
    }
}

/// Output: explicit fields resolve against the single input; absent fields
/// derive by cloning predecessor fields with rewritten direct lineage.
fn compute_output_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let Some(pred) = input_desc(d, descs, key_to_idx, 0) else {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs", d.idx),
                "err.dataflow.node.input",
                "Output requires 1 input",
            )),
        );
    };
    let sn = d.spec.unwrap();
    if sn.get("fields").and_then(Value::as_array).is_some() {
        let Some(pred_fields) = &pred.out_fields else {
            // raw predecessor without declared fields: the Output's own
            // declarations ARE the shape — emit them unresolved
            if pred.raw_body.is_some() {
                let fields: Vec<Value> = sn
                    .get("fields")
                    .and_then(Value::as_array)
                    .unwrap()
                    .iter()
                    .map(emit_computed)
                    .collect();
                return (
                    Some(ComputedNode {
                        fields,
                        step_hints: Vec::new(),
                    }),
                    None,
                );
            }
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/fields", d.idx),
                    "field.required",
                    "predecessor fields not computed",
                )),
            );
        };
        match resolve_declared_fields(d, &[pred], &[pred_fields], true, false) {
            Ok(fields) => {
                return (
                    Some(ComputedNode {
                        fields,
                        step_hints: Vec::new(),
                    }),
                    None,
                );
            }
            Err(e) => return (None, Some(e)),
        }
    }
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
    let out = pred_fields
        .iter()
        .map(|f| clone_passthrough(f, pred))
        .collect();
    (
        Some(ComputedNode {
            fields: out,
            step_hints: Vec::new(),
        }),
        None,
    )
}

/// Identity projection for Output: strip exp/aggType/lineage, set
/// inputField = predecessor name, original* = proven origin, never inputNode.
fn clone_passthrough(f: &Value, pred: &NodeDesc) -> Value {
    let mut m = f.as_object().cloned().unwrap_or_default();
    for k in [
        "exp",
        "aggType",
        "inputField",
        "inputNode",
        "originalField",
        "originalNode",
    ] {
        m.remove(k);
    }
    let name = field_name(f, 0);
    m.insert("name".into(), Value::String(name.clone()));
    m.insert("inputField".into(), Value::String(name));
    if let Some((on, of)) = field_origin(f, pred) {
        m.insert("originalNode".into(), Value::String(on));
        m.insert("originalField".into(), Value::String(of));
    }
    Value::Object(m)
}

/// Resolve explicit `fields` declarations against predecessor field sets.
/// `strict` reports notfound/ambiguous; lenient reshaping nodes fall back to
/// the declaration minus original*. `emit_input_node` controls whether a
/// resolved input alias is written (Join/Union yes; Output/Distinct no).
fn resolve_declared_fields(
    d: &NodeDesc,
    preds: &[&NodeDesc],
    fields_of: &[&Vec<Value>],
    strict: bool,
    emit_input_node: bool,
) -> Result<Vec<Value>, Diag> {
    let declared = d
        .spec
        .and_then(|s| s.get("fields"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(declared.len());
    for (fi, df) in declared.iter().enumerate() {
        if has_exp(df) {
            out.push(emit_computed(df));
            continue;
        }
        let want = field_match_name(df);
        let want_node = field_str(df, "inputNode");
        let mut matches: Vec<(&NodeDesc, &Value)> = Vec::new();
        for (pi, pred) in preds.iter().enumerate() {
            if let Some(wn) = want_node {
                if !field_eq(wn, &node_alias(pred)) {
                    continue;
                }
            }
            if let Some(pf) = find_pred_field(fields_of[pi], want) {
                matches.push((pred, pf));
            }
        }
        match matches.len() {
            0 => {
                if strict {
                    return Err(Diag::spec(
                        format!("/nodes/{}/fields/{}", d.idx, fi),
                        "field.notfound",
                        format!("field {:?} does not resolve to any input field", want),
                    ));
                }
                out.push(emit_unresolved(df));
            }
            1 => {
                let (pred, pf) = matches[0];
                let pname = pf
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(want)
                    .to_string();
                let in_node = if emit_input_node {
                    Some(node_alias(pred))
                } else {
                    None
                };
                out.push(merge_declared(
                    df,
                    pf,
                    &pname,
                    in_node.as_deref(),
                    field_origin(pf, pred),
                ));
            }
            _ => {
                if strict {
                    return Err(Diag::spec(
                        format!("/nodes/{}/fields/{}", d.idx, fi),
                        "field.ambiguous",
                        format!("field {:?} matches multiple input fields", want),
                    ));
                }
                // never guess matches[0] on ambiguity: keep the declaration
                // minus the unproven original* pair (explicit direct refs
                // are preserved)
                out.push(emit_unresolved(df));
            }
        }
    }
    Ok(out)
}

/// Distinct: explicit fields resolve uniquely across inputs (declared
/// inputNode narrows); no inputNode is emitted (single semantic stream).
fn compute_distinct_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let mut preds = Vec::new();
    let mut fields_of = Vec::new();
    for pos in 0..d.inputs.len() {
        let Some(pred) = input_desc(d, descs, key_to_idx, pos) else {
            continue;
        };
        let Some(pf) = &pred.out_fields else {
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/fields", d.idx),
                    "field.required",
                    "predecessor fields not computed",
                )),
            );
        };
        preds.push(pred);
        fields_of.push(pf);
    }
    if preds.is_empty() {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs", d.idx),
                "err.dataflow.node.input",
                "Distinct requires at least 1 input",
            )),
        );
    }
    match resolve_declared_fields(d, &preds, &fields_of, true, false) {
        Ok(fields) => (
            Some(ComputedNode {
                fields,
                step_hints: Vec::new(),
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    }
}

/// Join: every declared field must resolve to exactly one predecessor
/// field; `inputNode` (an input alias) disambiguates shared names.
fn compute_join_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let mut preds = Vec::new();
    let mut fields_of = Vec::new();
    for pos in 0..d.inputs.len() {
        let Some(pred) = input_desc(d, descs, key_to_idx, pos) else {
            continue;
        };
        let Some(pf) = &pred.out_fields else {
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/fields", d.idx),
                    "field.required",
                    "predecessor fields not computed",
                )),
            );
        };
        preds.push(pred);
        fields_of.push(pf);
    }
    match resolve_declared_fields(d, &preds, &fields_of, true, true) {
        Ok(fields) => (
            Some(ComputedNode {
                fields,
                step_hints: Vec::new(),
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    }
}

/// Union: declared fields resolve through `unionMapArray` rows exactly like
/// `DwDataFlowNodeContributor_union#sortFields` — the map key is the first
/// non-empty `values[i]` paired with input i's alias; the emitted
/// inputNode/inputField are always that canonical pair. Without a map the
/// legacy per-input name lookup applies (inputField = declared name).
fn compute_union_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let mut preds = Vec::new();
    let mut fields_of = Vec::new();
    for pos in 0..d.inputs.len() {
        let Some(pred) = input_desc(d, descs, key_to_idx, pos) else {
            continue;
        };
        let Some(pf) = &pred.out_fields else {
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/fields", d.idx),
                    "field.required",
                    "predecessor fields not computed",
                )),
            );
        };
        preds.push(pred);
        fields_of.push(pf);
    }
    if preds.is_empty() {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs", d.idx),
                "err.dataflow.node.input",
                "Union requires at least 1 input",
            )),
        );
    }
    let sn = d.spec.unwrap();
    let map = sn
        .get("config")
        .and_then(|c| c.get("unionMapArray"))
        .and_then(Value::as_array);

    // canonical map keys: (input position of first non-empty value, alias,
    // field name) per visible row; duplicate keys are an error
    struct URow {
        key_pos: usize,
        key_alias: String,
        key_field: String,
        values: Vec<Option<String>>,
    }
    let mut rows: Vec<URow> = Vec::new();
    let mut key_seen: HashSet<(String, String)> = HashSet::new();
    if let Some(arr) = map {
        for (ri, item) in arr.iter().enumerate() {
            let visible = item.get("visible").and_then(Value::as_bool).unwrap_or(true);
            if !visible {
                continue;
            }
            let Some(vals) = item.get("values").and_then(Value::as_array) else {
                continue; // shape already diagnosed by validate_closed_config
            };
            let mut first: Option<(usize, String)> = None;
            let mut values: Vec<Option<String>> = Vec::with_capacity(vals.len());
            for (vi, v) in vals.iter().enumerate() {
                let s = v.as_str().map(str::to_string);
                values.push(s.clone());
                if first.is_none() {
                    if let Some(sv) = s.as_ref().filter(|x| !x.is_empty()) {
                        first = Some((vi, sv.clone()));
                    }
                }
            }
            let Some((kpos, kfield)) = first else {
                continue; // all empty: Java produces no map key
            };
            if kpos >= preds.len() {
                continue; // width mismatch already diagnosed
            }
            let kalias = node_alias(preds[kpos]);
            if !key_seen.insert((kalias.to_uppercase(), kfield.to_uppercase())) {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/config/unionMapArray/{}", d.idx, ri),
                        "union.mapkey",
                        "unionMapArray rows produce a duplicate map key",
                    )),
                );
            }
            rows.push(URow {
                key_pos: kpos,
                key_alias: kalias,
                key_field: kfield,
                values,
            });
        }
    }

    let declared = sn
        .get("fields")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::with_capacity(declared.len());
    // canonical map keys already consumed by a visible noncomputed field —
    // two fields resolving to the same key is an ambiguous runtime
    // projection, never a legitimate duplicate
    let mut used_keys: HashSet<(String, String)> = HashSet::new();
    for (fi, df) in declared.iter().enumerate() {
        // hidden/invalid declared fields are never read at runtime: they
        // bypass map matching entirely and carry no proven origin
        let hidden = df.get("hidden").and_then(Value::as_bool) == Some(true) || skipped_field(df);
        if hidden {
            out.push(emit_unresolved(df));
            continue;
        }
        if has_exp(df) {
            out.push(emit_computed(df));
            continue;
        }
        if rows.is_empty() && map.is_none() {
            // legacy no-map resolution: inputField = declared name, origin
            // looked up in the first input
            let want = field_match_name(df);
            let origin = find_pred_field(fields_of[0], want)
                .map(|pf| field_origin(pf, preds[0]))
                .flatten();
            let mut m = df.as_object().cloned().unwrap_or_default();
            m.remove("originalNode");
            m.remove("originalField");
            m.remove("inputNode");
            m.insert("inputField".into(), Value::String(want.to_string()));
            m.insert("inputNode".into(), Value::String(node_alias(preds[0])));
            if let Some((on, of)) = origin {
                m.insert("originalNode".into(), Value::String(on));
                m.insert("originalField".into(), Value::String(of));
            }
            out.push(Value::Object(m));
            continue;
        }
        if rows.is_empty() {
            // map present but produced no keys: only computed/hidden fields
            // may remain; every visible passthrough field is an error
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/fields/{}", d.idx, fi),
                    "union.mapkey",
                    "no visible unionMapArray row can produce this field",
                )),
            );
        }
        let want_field = field_match_name(df);
        let want_node = field_str(df, "inputNode");
        // primary: match the canonical key (field + optional alias)
        let mut primary: Vec<&URow> = rows
            .iter()
            .filter(|r| {
                field_eq(&r.key_field, want_field)
                    && want_node
                        .map(|wn| field_eq(wn, &r.key_alias))
                        .unwrap_or(true)
            })
            .collect();
        // secondary: the field may be renamed — match any other populated
        // values[j] together with that input's alias
        if primary.is_empty() {
            let mut secondary: Vec<&URow> = Vec::new();
            for r in &rows {
                for (j, v) in r.values.iter().enumerate() {
                    let Some(sv) = v.as_ref().filter(|s| !s.is_empty()) else {
                        continue;
                    };
                    if j == r.key_pos || !field_eq(sv, want_field) {
                        continue;
                    }
                    let alias_j = node_alias(preds[j]);
                    if want_node.map(|wn| !field_eq(wn, &alias_j)).unwrap_or(false) {
                        continue;
                    }
                    secondary.push(r);
                    break;
                }
            }
            primary = secondary;
        }
        match primary.len() {
            1 => {
                let r = primary[0];
                let Some(pf) = find_pred_field(fields_of[r.key_pos], &r.key_field) else {
                    return (
                        None,
                        Some(Diag::spec(
                            format!("/nodes/{}/fields/{}", d.idx, fi),
                            "union.mapkey",
                            format!(
                                "unionMapArray key {:?} has no field in input {}",
                                r.key_field, r.key_pos
                            ),
                        )),
                    );
                };
                // two visible noncomputed fields resolving to the same
                // canonical map key would read the same runtime column
                if !used_keys.insert((r.key_alias.to_uppercase(), r.key_field.to_uppercase())) {
                    return (
                        None,
                        Some(Diag::spec(
                            format!("/nodes/{}/fields/{}", d.idx, fi),
                            "union.mapkey",
                            format!(
                                "field {:?} reuses unionMapArray key {:?} already claimed by another field",
                                want_field, r.key_field
                            ),
                        )),
                    );
                }
                let origin = field_origin(pf, preds[r.key_pos]);
                let pname = r.key_field.clone();
                let kalias = r.key_alias.clone();
                out.push(merge_declared(df, pf, &pname, Some(&kalias), origin));
            }
            0 => {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/fields/{}", d.idx, fi),
                        "union.mapkey",
                        format!("no unionMapArray row matches field {:?}", want_field),
                    )),
                );
            }
            _ => {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/fields/{}", d.idx, fi),
                        "union.mapkey",
                        format!("field {:?} matches multiple unionMapArray rows", want_field),
                    )),
                );
            }
        }
    }
    (
        Some(ComputedNode {
            fields: out,
            step_hints: Vec::new(),
        }),
        None,
    )
}

/// Reshaping nodes (Agg, RowToCol, ColToRow, Snapshot, Update, DbOutput,
/// SlowChanging, GeoFindLocation, Extend): explicit fields only. A unique
/// predecessor match inherits proven passthrough lineage; anything else
/// keeps the declaration minus the unproven original* pair.
fn compute_reshaped_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let mut preds = Vec::new();
    let mut fields_of = Vec::new();
    for pos in 0..d.inputs.len() {
        if let Some(pred) = input_desc(d, descs, key_to_idx, pos) {
            if let Some(pf) = &pred.out_fields {
                preds.push(pred);
                fields_of.push(pf);
            }
        }
    }
    // reshaping nodes never synthesize inputNode: an invented direct ref
    // would alter the runtime inputFieldExp (e.g. on pivot-born columns)
    match resolve_declared_fields(d, &preds, &fields_of, false, false) {
        Ok(fields) => (
            Some(ComputedNode {
                fields,
                step_hints: Vec::new(),
            }),
            None,
        ),
        Err(e) => (None, Some(e)),
    }
}

fn field_name(f: &Value, idx: usize) -> String {
    f.get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("field{}", idx))
}

/// Sequential Select simulation: evolves the predecessor field set through
/// every step exactly like `QueryImpl` does, tracking wire-shaped field
/// values plus the `originFieldInfo` rename hints the editor writes back.
struct SimResult {
    /// field state after the last applied step — when `opaque_at` is set
    /// this is the proven state BEFORE that step
    fields: Vec<Value>,
    /// per-step originFieldInfo hints (None = no rename hint emitted)
    step_hints: Vec<Option<Value>>,
    /// index of the first step whose shape cannot be modeled
    /// (rawStep/SplitField/Text2Id/unknown); None when all applied
    opaque_at: Option<usize>,
}

/// Apply one provable step's object-level mutation to wire-shaped fields,
/// mirroring the name evolution `evolve_names` already performed on the
/// same step. Rename hints capture the CURRENT name/dataType before any
/// Refactor mutation (the editor dereferences originFieldInfo.name).
/// `explicit_fields` marks whether the Select declares `fields` — a
/// type-converting Refactor is only derivable then.
fn apply_step_mutation(
    fields: &mut Vec<Value>,
    s: &Value,
    si: usize,
    d_idx: usize,
    step_hints: &mut [Option<Value>],
    explicit_fields: bool,
) -> Result<(), Diag> {
    let ty = s.get("type").and_then(Value::as_str).unwrap_or("");
    let operands: Vec<String> = s
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match ty {
        "RemoveField" => {
            fields.retain(|f| {
                let n = f.get("name").and_then(Value::as_str).unwrap_or("");
                !operands.iter().any(|o| field_eq(o, n))
            });
        }
        "Refactor" => {
            let refactor = s.get("refactor").and_then(Value::as_object);
            let type_keys = ["dataType", "length", "decimal", "aggType", "dateFormat"];
            let has_type_conv = refactor
                .map(|o| o.keys().any(|k| type_keys.contains(&k.as_str())))
                .unwrap_or(false);
            if has_type_conv && !explicit_fields {
                return Err(Diag::spec(
                    format!("/nodes/{}/fields", d_idx),
                    "field.required",
                    "Refactor type conversion cannot infer length/decimal; provide explicit fields",
                ));
            }
            let sel = operands.first().cloned().unwrap_or_default();
            let Some(f) = fields.iter_mut().find(|f| {
                f.get("name")
                    .and_then(Value::as_str)
                    .map(|n| field_eq(n, &sel))
                    .unwrap_or(false)
            }) else {
                return Ok(());
            };
            let Some(m) = f.as_object_mut() else {
                return Ok(());
            };
            let rename = s
                .pointer("/refactor/rename")
                .and_then(Value::as_str)
                .filter(|r| !r.is_empty())
                .map(str::to_string);
            if rename.is_some() {
                // editor metadata: capture the pre-rename identity
                let prior_dt = m
                    .get("dataType")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                step_hints[si] = Some(serde_json::json!({
                    "name": m.get("name").and_then(Value::as_str).unwrap_or(""),
                    "dataType": prior_dt,
                }));
            }
            if has_type_conv {
                if let Some(ro) = refactor {
                    for (k, v) in ro {
                        if k != "rename" {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            if let Some(rn) = rename {
                m.insert("name".into(), Value::String(rn));
            }
        }
        "AddField" => {
            let af = s.get("addField").cloned().unwrap_or(Value::Null);
            let name = af.get("name").and_then(Value::as_str).unwrap_or("");
            let dt = af.get("dataType").and_then(Value::as_str).unwrap_or("");
            let has_role = af
                .get("isDimension")
                .map(Value::is_boolean)
                .unwrap_or(false);
            let has_exp = af
                .get("exp")
                .and_then(Value::as_str)
                .map(|x| !x.trim().is_empty())
                .unwrap_or(false);
            if name.is_empty() || dt.is_empty() || !has_role || !has_exp {
                return Err(Diag::spec(
                    format!("/nodes/{}/steps/{}/addField", d_idx, si),
                    "field.required",
                    "AddField requires declared name, dataType, isDimension role and exp for derivation",
                ));
            }
            // the added field is born by computation: no direct lineage
            let mut m = af.as_object().cloned().unwrap_or_default();
            for k in ["originalNode", "originalField", "inputNode", "inputField"] {
                m.remove(k);
            }
            fields.push(Value::Object(m));
        }
        "TransformToCalculatedField" => {
            // Java edits the field's expression in place — names preserved
            // (evolve_names already confirmed); the field becomes computed
            // so its proven origin is shed
            let tfi = s.get("transformFieldInfo").cloned().unwrap_or(Value::Null);
            let exp = tfi
                .get("exp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if exp.is_empty() {
                return Err(Diag::spec(
                    format!("/nodes/{}/steps/{}/transformFieldInfo", d_idx, si),
                    "field.required",
                    "TransformToCalculatedField requires transformFieldInfo.exp for derivation",
                ));
            }
            let dt = tfi
                .get("dataType")
                .and_then(Value::as_str)
                .map(str::to_string);
            for f in fields.iter_mut() {
                let hit = f
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|n| operands.iter().any(|o| field_eq(o, n)))
                    .unwrap_or(false);
                if !hit {
                    continue;
                }
                if let Some(m) = f.as_object_mut() {
                    m.insert("exp".into(), Value::String(exp.clone()));
                    m.remove("originalNode");
                    m.remove("originalField");
                    if let Some(dtv) = &dt {
                        m.insert("dataType".into(), Value::String(dtv.clone()));
                    }
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Run the Select step simulation over wire-shaped fields. Hard errors
/// (name collisions, invalid required payloads) are `Err`; an unmodelable
/// step stops simulation and is reported via `SimResult::opaque_at`.
fn simulate_select(
    d: &NodeDesc,
    pred: &NodeDesc,
    pred_fields: &[Value],
    explicit_fields: bool,
) -> Result<SimResult, Diag> {
    let sn = d.spec.unwrap();
    let mut work: Vec<Value> = pred_fields
        .iter()
        .map(|f| clone_passthrough(f, pred))
        .collect();
    let steps = sn
        .get("steps")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut step_hints: Vec<Option<Value>> = vec![None; steps.len()];
    let mut opaque_at = None;
    for (si, s) in steps.iter().enumerate() {
        // names evolve first: collision detection + authoritative name set
        let mut names: Vec<String> = work
            .iter()
            .enumerate()
            .map(|(i, f)| field_name(f, i))
            .collect();
        match evolve_names(&mut names, s) {
            Ok(true) => {}
            Ok(false) => {
                opaque_at = Some(si);
                break;
            }
            Err(tag) => {
                return Err(Diag::spec(
                    format!("/nodes/{}/steps/{}/{}", d.idx, si, tag),
                    "field.duplicate",
                    format!(
                        "step at /nodes/{}/steps/{} creates a duplicate field name",
                        d.idx, si
                    ),
                ));
            }
        }
        apply_step_mutation(&mut work, s, si, d.idx, &mut step_hints, explicit_fields)?;
    }
    Ok(SimResult {
        fields: work,
        step_hints,
        opaque_at,
    })
}

/// Backwards suffix pass over the steps AFTER the last opaque step (or ALL
/// steps when the predecessor shape is unknown). `fields` are the FINAL
/// declarations (emit_computed/emit_unresolved applied): their length and
/// order never change. Walking the suffix in reverse recovers the
/// step-time metadata that is still provable — a Transform's exp/dataType,
/// an AddField's birth exp — and rejects declarations the suffix provably
/// contradicts. Nothing is manufactured: no lineage is synthesized through
/// the opaque step and no rename hint is guessed (the pre-rename
/// dataType is unknowable at this point).
fn apply_escape_steps(
    d: &NodeDesc,
    steps: &[Value],
    fields: &mut [Value],
    start: usize,
) -> Result<(), Diag> {
    let mismatch =
        |msg: String| Diag::spec(format!("/nodes/{}/fields", d.idx), "field.mismatch", msg);
    // slot.name = the name the field carried at the current reverse
    // frontier; alive=false once a reverse AddField consumed its birth
    let mut slots: Vec<(String, bool)> = fields
        .iter()
        .map(|f| {
            (
                f.get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_uppercase(),
                true,
            )
        })
        .collect();
    let mut killed: HashSet<String> = HashSet::new();
    let mut exp_bound: HashSet<usize> = HashSet::new();
    let mut born: HashMap<usize, Value> = HashMap::new();
    let mut transformed: HashMap<usize, Value> = HashMap::new();
    for si in (start..steps.len()).rev() {
        let s = &steps[si];
        let ty = s.get("type").and_then(Value::as_str).unwrap_or("");
        if s.get("rawStep").is_some() || matches!(ty, "SplitField" | "Text2Id") {
            break;
        }
        let operands: Vec<String> = s
            .get("fields")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        match ty {
            "RemoveField" => {
                for o in &operands {
                    let uo = o.to_uppercase();
                    // a declared field this step removes is a
                    // contradiction (re-births were consumed by the
                    // reverse AddField already)
                    if slots.iter().any(|(n, a)| *a && *n == uo) {
                        return Err(mismatch(format!(
                            "field {:?} is removed by step {} but still declared",
                            o, si
                        )));
                    }
                    killed.insert(uo);
                }
            }
            "Refactor" => {
                // only rename changes names; type conversions carry no
                // recoverable prior metadata
                if let Some(rn) = s
                    .pointer("/refactor/rename")
                    .and_then(Value::as_str)
                    .filter(|r| !r.is_empty())
                {
                    let urn = rn.to_uppercase();
                    if let Some(pos) = slots.iter().position(|(n, a)| *a && *n == urn) {
                        if let Some(sel) = operands.first() {
                            slots[pos].0 = sel.to_uppercase();
                        }
                    } else if killed.contains(&urn) {
                        // renamed then killed later: the pre-rename name
                        // is dead at this frontier too
                        if let Some(sel) = operands.first() {
                            killed.insert(sel.to_uppercase());
                        }
                    }
                }
            }
            "TransformToCalculatedField" => {
                let tfi = s.get("transformFieldInfo").cloned().unwrap_or(Value::Null);
                for o in &operands {
                    let uo = o.to_uppercase();
                    if let Some(idx) = slots.iter().position(|(n, a)| *a && *n == uo) {
                        // reverse order => first hit per field is the
                        // LATEST transform; earlier ones must not override
                        if !exp_bound.contains(&idx) {
                            transformed.insert(idx, tfi.clone());
                            exp_bound.insert(idx);
                        }
                    }
                }
            }
            "AddField" => {
                let af = s.get("addField").cloned().unwrap_or(Value::Null);
                let na = af
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_uppercase();
                if let Some(idx) = slots.iter().position(|(n, a)| *a && *n == na) {
                    // birth of a surviving declared field: bind the
                    // addField payload, consume the slot
                    born.insert(idx, af.clone());
                    slots[idx].1 = false;
                    exp_bound.insert(idx);
                } else if !killed.contains(&na) {
                    return Err(mismatch(format!(
                        "step {} adds field {:?} which the explicit fields omit",
                        si, na
                    )));
                }
                // na in killed: born then removed later — allowed
            }
            _ => {}
        }
    }
    for (idx, f) in fields.iter_mut().enumerate() {
        let Some(m) = f.as_object_mut() else {
            continue;
        };
        for k in ["originalNode", "originalField", "inputNode"] {
            m.remove(k);
        }
        if born.contains_key(&idx) {
            m.remove("inputField");
        }
        if let Some(tfi) = transformed.get(&idx) {
            if let Some(e) = tfi.get("exp").and_then(Value::as_str) {
                m.insert("exp".into(), Value::String(e.to_string()));
            }
            if let Some(dt) = tfi.get("dataType").and_then(Value::as_str) {
                m.insert("dataType".into(), Value::String(dt.to_string()));
            }
        }
        if let Some(af) = born.get(&idx) {
            if !transformed.contains_key(&idx) {
                if let Some(e) = af.get("exp").and_then(Value::as_str) {
                    m.insert("exp".into(), Value::String(e.to_string()));
                }
            }
        }
    }
    Ok(())
}

/// Multiset equality of field names under the platform's Unicode-aware
/// `field_eq` folding (order-insensitive).
fn name_multiset_eq(a: &[String], b: &[String]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut remaining = b.to_vec();
    'outer: for x in a {
        for (i, y) in remaining.iter().enumerate() {
            if field_eq(x, y) {
                remaining.remove(i);
                continue 'outer;
            }
        }
        return false;
    }
    remaining.is_empty()
}

/// Select field computation: the sequential simulation provides lineage,
/// expressions and rename hints. A provable result is checked against the
/// declared name multiset directly; explicit `fields` keep their order and
/// presentation while simulated input/exp/origin is enforced by name. An
/// unmodelable step falls back to the declarations (minus unproven
/// original*) with whatever later steps remain provable on those names.
fn compute_select_fields(
    d: &NodeDesc,
    descs: &[NodeDesc],
    key_to_idx: &HashMap<String, usize>,
) -> (Option<ComputedNode>, Option<Diag>) {
    let sn = d.spec.unwrap();
    if d.inputs.is_empty() {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs", d.idx),
                "err.dataflow.node.input",
                "Select requires 1 input",
            )),
        );
    }
    let Some(pred) = input_desc(d, descs, key_to_idx, 0) else {
        return (
            None,
            Some(Diag::spec(
                format!("/nodes/{}/inputs/{}", d.idx, 0),
                "graph.danglingInput",
                "unknown input key",
            )),
        );
    };
    let declared = sn.get("fields").and_then(Value::as_array);
    let Some(pred_fields) = &pred.out_fields else {
        // unknown predecessor shape (rawNode without fields): the explicit
        // declarations ARE the contract — recover provable step metadata by
        // walking every step backwards; absent declarations stay an error
        let Some(decl) = declared else {
            return (
                None,
                Some(Diag::spec(
                    format!("/nodes/{}/fields", d.idx),
                    "field.required",
                    "predecessor fields not computed",
                )),
            );
        };
        let steps = sn
            .get("steps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut fields: Vec<Value> = decl
            .iter()
            .map(|df| {
                if has_exp(df) {
                    emit_computed(df)
                } else {
                    emit_unresolved(df)
                }
            })
            .collect();
        if let Err(e) = apply_escape_steps(d, &steps, &mut fields, 0) {
            return (None, Some(e));
        }
        return (
            Some(ComputedNode {
                fields,
                step_hints: vec![None; steps.len()],
            }),
            None,
        );
    };
    let res = match simulate_select(d, pred, pred_fields, declared.is_some()) {
        Ok(r) => r,
        Err(e) => return (None, Some(e)),
    };
    match declared {
        Some(decl) => {
            if let Some(oi) = res.opaque_at {
                // unmodelable step: declarations are the escape path —
                // emit them minus unproven lineage (computed fields also
                // shed inputNode), keep the hints proven before the opaque
                // step, then recover provable suffix metadata by walking
                // the later steps backwards
                let mut fields: Vec<Value> = decl
                    .iter()
                    .map(|df| {
                        if has_exp(df) {
                            emit_computed(df)
                        } else {
                            emit_unresolved(df)
                        }
                    })
                    .collect();
                let hints = res.step_hints;
                let steps = sn
                    .get("steps")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if let Err(e) = apply_escape_steps(d, &steps, &mut fields, oi + 1) {
                    return (None, Some(e));
                }
                return (
                    Some(ComputedNode {
                        fields,
                        step_hints: hints,
                    }),
                    None,
                );
            }
            // provable: declared names must equal the simulated multiset —
            // a mismatch is a spec error, never an unresolved declaration
            let decl_names: Vec<String> = decl.iter().map(|f| field_name(f, 0)).collect();
            let sim_names: Vec<String> = res
                .fields
                .iter()
                .enumerate()
                .map(|(i, f)| field_name(f, i))
                .collect();
            if !name_multiset_eq(&decl_names, &sim_names) {
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/fields", d.idx),
                        "field.mismatch",
                        format!(
                            "explicit fields {:?} do not match derivable output {:?}",
                            decl_names, sim_names
                        ),
                    )),
                );
            }
            // project each declaration onto its simulated field: declared
            // order + presentation keys, simulated
            // inputField/exp/original* enforced by name
            let mut out = Vec::with_capacity(decl.len());
            for df in decl {
                let dn = df.get("name").and_then(Value::as_str).unwrap_or("");
                if let Some(sf) = res.fields.iter().find(|sf| {
                    sf.get("name")
                        .and_then(Value::as_str)
                        .map(|n| field_eq(n, dn))
                        .unwrap_or(false)
                }) {
                    let mut m = sf.as_object().cloned().unwrap_or_default();
                    if let Some(do_) = df.as_object() {
                        for (k, v) in do_ {
                            if ENFORCED_FIELD_KEYS.contains(&k.as_str()) {
                                continue;
                            }
                            m.insert(k.clone(), v.clone());
                        }
                    }
                    m.insert(
                        "name".into(),
                        df.get("name").cloned().unwrap_or(Value::Null),
                    );
                    out.push(Value::Object(m));
                } else {
                    out.push(emit_unresolved(df));
                }
            }
            (
                Some(ComputedNode {
                    fields: out,
                    step_hints: res.step_hints,
                }),
                None,
            )
        }
        None => {
            if let Some(si) = res.opaque_at {
                let steps = sn
                    .get("steps")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let ty = steps
                    .get(si)
                    .and_then(|s| s.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or("rawStep");
                return (
                    None,
                    Some(Diag::spec(
                        format!("/nodes/{}/fields", d.idx),
                        "field.required",
                        format!(
                            "step {} at /nodes/{}/steps/{} changes fields unpredictably; provide explicit fields",
                            ty, d.idx, si
                        ),
                    )),
                );
            }
            (
                Some(ComputedNode {
                    fields: res.fields,
                    step_hints: res.step_hints,
                }),
                None,
            )
        }
    }
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
    // fields partition: fields:[] is retained for known-empty outputs, but
    // empty dimensions/measures lists are omitted entirely
    if let Some(fields) = &d.out_fields {
        let (dims, meas) = partition_fields(fields);
        body.insert("fields".into(), Value::Array(fields.clone()));
        if !dims.is_empty() {
            body.insert("dimensions".into(), Value::Array(dims));
        }
        if !meas.is_empty() {
            body.insert("measures".into(), Value::Array(meas));
        }
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
        // steps: typed verbatim / rawStep verbatim; empty lists omitted.
        // Refactor rename hints (originFieldInfo) are regenerated from the
        // sequential simulation — never trusted from the spec.
        if let Some(steps) = sn.get("steps").and_then(Value::as_array) {
            if !steps.is_empty() {
                let mut out_steps = Vec::new();
                for (si, s) in steps.iter().enumerate() {
                    if let Some(rs) = s.get("rawStep") {
                        out_steps.push(rs.clone());
                    } else {
                        let mut step = s.clone();
                        // AddField's fields operand is the platform's own
                        // selection: a missing operand is synthesized as
                        // [addField.name]; an explicit operand — even a
                        // differing singleton — is preserved verbatim.
                        if s.get("type").and_then(Value::as_str) == Some("AddField")
                            && !s
                                .as_object()
                                .map(|o| o.contains_key("fields"))
                                .unwrap_or(false)
                        {
                            if let Some(nm) = s.pointer("/addField/name").and_then(Value::as_str) {
                                if !nm.is_empty() {
                                    if let Some(m) = step.as_object_mut() {
                                        m.insert(
                                            "fields".into(),
                                            Value::Array(vec![Value::String(nm.to_string())]),
                                        );
                                    }
                                }
                            }
                        }
                        if s.get("rawStep").is_none() {
                            if let Some(hint) = d.step_hints.get(si).and_then(|h| h.as_ref()) {
                                if let Some(m) = step.as_object_mut() {
                                    m.insert("originFieldInfo".into(), hint.clone());
                                }
                            }
                        }
                        out_steps.push(step);
                    }
                }
                body.insert("steps".into(), Value::Array(out_steps));
            }
        }
        // stepCount is only meaningful when it exceeds the real step count
        let len = sn
            .get("steps")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0) as u64;
        let cfg_sc = sn
            .pointer("/config/stepCount")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if cfg_sc > len {
            body.insert("stepCount".into(), Value::from(cfg_sc));
        }
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

/// The default Output may be typed or a rawNode; its fields drive the
/// top-level dimensions/measures partition. The loader routes by the
/// nodes-map key, so `key == "default"` + `type == "Output"` decides for
/// both forms — a raw body's own `id` is payload, not routing.
fn is_default_output(d: &NodeDesc) -> bool {
    d.wire_key == "default" && d.ty.as_deref() == Some("Output")
}

fn default_output_partition(descs: &[NodeDesc], diags: &mut Vec<Diag>) -> (Vec<Value>, Vec<Value>) {
    for d in descs {
        if is_default_output(d) {
            match &d.out_fields {
                Some(fields) => return partition_fields(fields),
                None => {
                    diags.push(Diag::spec(
                        if d.raw_body.is_some() {
                            format!("/nodes/{}/rawNode/fields", d.idx)
                        } else {
                            format!("/nodes/{}/fields", d.idx)
                        },
                        "field.required",
                        "default Output fields are required to build the top-level partition",
                    ));
                    return (vec![], vec![]);
                }
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
