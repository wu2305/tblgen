//! `decompile` — `.tbl` wire object -> spec-format v1 document.
//!
//! Strategy: build a generated-form candidate (typed nodes/steps where the
//! wire fits the closed schema, `rawNode`/`rawStep` escapes otherwise), then
//! verify by recompiling and comparing against the normalized input
//! (`dataFlow.nodes.*.lastModifyTime` stripped, object order ignored).
//! Exact match -> emit generated form; anything else -> structured archive
//! (DataFlow) or `rawTable` (non-DataFlow). Archive bodies are verbatim,
//! timestamps included.

use serde_json::{Map, Value};

use crate::gen::generate;
use crate::model::*;

/// Decompile a parsed `.tbl` object into a spec document.
pub fn decompile(tbl: &Value) -> Result<Value, Diag> {
    if !tbl.is_object() {
        return Err(Diag::spec(
            "",
            "structure.tbl",
            ".tbl must be a JSON object",
        ));
    }
    let declares_flow = tbl
        .pointer("/properties/modelDataType")
        .and_then(Value::as_str)
        == Some("DataFlow");
    match tbl.get("dataFlow") {
        Some(flow) => {
            // a present-but-malformed dataFlow must be REJECTED, not
            // archived as rawTable — the result could never regenerate.
            let ok = flow.get("nodes").map(Value::is_object).unwrap_or(false);
            if !ok {
                return Err(Diag::wire(
                    "/dataFlow",
                    "structure.dataFlow",
                    "",
                    "dataFlow.nodes must be an object",
                ));
            }
        }
        None => {
            if declares_flow {
                return Err(Diag::wire(
                    "/dataFlow",
                    "structure.dataFlow",
                    "",
                    "properties.modelDataType is \"DataFlow\" but dataFlow is missing",
                ));
            }
            // nonflow: not assessed, verbatim snapshot
            return Ok(serde_json::json!({"format": 1, "rawTable": tbl}));
        }
    }

    // candidate generated form
    if let Some(candidate) = try_generated(tbl) {
        if let Ok(wire) = generate(&candidate) {
            if wire == normalize(tbl) {
                return Ok(candidate);
            }
        }
    }
    Ok(archive_form(tbl))
}

/// Strip `dataFlow.nodes.*.lastModifyTime` — the only normalized-away wire
/// key (wall-clock cache hint). Everything else compares verbatim.
fn normalize(tbl: &Value) -> Value {
    let mut out = tbl.clone();
    if let Some(nodes) = out
        .pointer_mut("/dataFlow/nodes")
        .and_then(Value::as_object_mut)
    {
        for (_, node) in nodes.iter_mut() {
            if let Some(nobj) = node.as_object_mut() {
                nobj.remove("lastModifyTime");
            }
        }
    }
    out
}

fn try_generated(tbl: &Value) -> Option<Value> {
    let mut spec = Map::new();
    spec.insert("format".into(), Value::from(1));

    // version
    match tbl.get("version") {
        Some(Value::String(s)) => {
            spec.insert("version".into(), Value::String(s.clone()));
        }
        Some(_) => {
            // non-string version cannot be typed; carry raw
            if let Some(v) = tbl.get("version") {
                let raw = spec_raw_map(&mut spec);
                raw.insert("version".into(), v.clone());
            }
        }
        None => {}
    }

    // properties: typed keys extracted, rest -> properties.raw
    if let Some(p) = tbl.get("properties") {
        if let Some(pobj) = p.as_object() {
            let mut typed = Map::new();
            let mut rawp = Map::new();
            let mut mdt_ok = true;
            for (k, v) in pobj {
                match k.as_str() {
                    "modelDataType" => {
                        if v.as_str() == Some("DataFlow") {
                            typed.insert(k.clone(), v.clone());
                        } else {
                            mdt_ok = false;
                        }
                    }
                    // typed only when the value has the loader's shape;
                    // anything else stays verbatim in raw
                    _ if prop_kind(k).is_some_and(|kind| prop_value_fits(kind, v)) => {
                        typed.insert(k.clone(), v.clone());
                    }
                    _ => {
                        rawp.insert(k.clone(), v.clone());
                    }
                }
            }
            if !mdt_ok {
                // non-DataFlow modelDataType: whole properties goes raw
                let raw = spec_raw_map(&mut spec);
                raw.insert("properties".into(), p.clone());
            } else {
                if !rawp.is_empty() {
                    typed.insert("raw".into(), Value::Object(rawp));
                }
                spec.insert("properties".into(), Value::Object(typed));
            }
        } else {
            let raw = spec_raw_map(&mut spec);
            raw.insert("properties".into(), p.clone());
        }
    }

    // parameters: typed items (name/dataType/defaultValue + raw), else raw
    if let Some(ps) = tbl.get("parameters") {
        if let Some(arr) = ps.as_array() {
            let mut ok = true;
            let mut items = Vec::new();
            'outer: for item in arr {
                let Some(iobj) = item.as_object() else {
                    ok = false;
                    break;
                };
                let mut m = Map::new();
                let mut rawi = Map::new();
                for (k, v) in iobj {
                    match k.as_str() {
                        "name" | "dataType" | "defaultValue" => {
                            m.insert(k.clone(), v.clone());
                        }
                        _ => {
                            rawi.insert(k.clone(), v.clone());
                        }
                    }
                }
                if m.get("name")
                    .and_then(Value::as_str)
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
                    || m.get("dataType")
                        .and_then(Value::as_str)
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                {
                    ok = false;
                    break 'outer;
                }
                if !rawi.is_empty() {
                    m.insert("raw".into(), Value::Object(rawi));
                }
                items.push(Value::Object(m));
            }
            if ok {
                spec.insert("parameters".into(), Value::Array(items));
            } else {
                let raw = spec_raw_map(&mut spec);
                raw.insert("parameters".into(), ps.clone());
            }
        } else {
            let raw = spec_raw_map(&mut spec);
            raw.insert("parameters".into(), ps.clone());
        }
    }

    // hierarchies: no typed form -> raw
    if let Some(h) = tbl.get("hierarchies") {
        let raw = spec_raw_map(&mut spec);
        raw.insert("hierarchies".into(), h.clone());
    }

    // dataFlow -> nodes + subProcesses
    let flow = tbl.get("dataFlow")?;
    let nodes_obj = flow.get("nodes")?.as_object()?;
    let mut spec_nodes = Vec::new();
    // first typed Output in iteration order gets default-id treatment
    let mut first_output_seen = false;

    for (key, body) in nodes_obj {
        match node_to_spec(key, body, &mut first_output_seen) {
            Some(sn) => spec_nodes.push(sn),
            None => return None, // unconvertible node body -> archive
        }
    }
    spec.insert("nodes".into(), Value::Array(spec_nodes));

    // subProcesses
    if let Some(sp) = flow.get("subProcesses") {
        if let Some(spobj) = sp.as_object() {
            let mut items = Map::new();
            for (key, item) in spobj {
                match subprocess_to_spec(item) {
                    Some(v) => {
                        items.insert(key.clone(), v);
                    }
                    None => return None,
                }
            }
            spec.insert("subProcesses".into(), Value::Object(items));
        } else {
            return None; // malformed subProcesses -> archive
        }
    }
    // other dataFlow keys cannot be expressed -> archive
    for k in flow.as_object()?.keys() {
        if k != "nodes" && k != "subProcesses" {
            return None;
        }
    }

    // remaining top-level keys -> raw (version/properties/parameters/
    // hierarchies/dimensions/measures/dataFlow handled or derived)
    for k in tbl.as_object()?.keys() {
        match k.as_str() {
            "version" | "properties" | "parameters" | "hierarchies" | "dimensions" | "measures"
            | "dataFlow" => {}
            _ => {
                let raw = spec_raw_map(&mut spec);
                if let Some(v) = tbl.get(k) {
                    raw.insert(k.clone(), v.clone());
                }
            }
        }
    }

    Some(Value::Object(spec))
}

fn spec_raw_map<'a>(spec: &'a mut Map<String, Value>) -> &'a mut Map<String, Value> {
    if !spec.contains_key("raw") {
        spec.insert("raw".into(), Value::Object(Map::new()));
    }
    spec.get_mut("raw").and_then(Value::as_object_mut).unwrap()
}

/// Convert one wire node to a spec node entry. Returns None when the node
/// cannot be expressed even via rawNode (non-object body).
fn node_to_spec(key: &str, body: &Value, first_output_seen: &mut bool) -> Option<Value> {
    let Some(bobj) = body.as_object() else {
        return None;
    };
    let ty = bobj.get("type").and_then(Value::as_str).unwrap_or("");
    let is_first_output = ty == "Output" && !*first_output_seen;
    if ty == "Output" {
        *first_output_seen = true;
    }

    if !node_supported(ty) {
        return rawnode_fallback(key, body);
    }

    // typed candidate
    let mut m = Map::new();
    m.insert("key".into(), Value::String(key.to_string()));
    m.insert("type".into(), Value::String(ty.to_string()));

    // alias
    if let Some(a) = bobj.get("alias") {
        m.insert("alias".into(), a.clone());
    }
    // id: keep unless it equals key and default resolution reproduces it
    if let Some(idv) = bobj.get("id") {
        let id_str = idv.as_str().unwrap_or("");
        let need_explicit = id_str != key || (is_first_output && key != "default");
        if need_explicit {
            m.insert("id".into(), idv.clone());
        }
        // if id==key and not first-output-special, omit -> gen resolves to key
    }
    // inputs
    if let Some(arr) = bobj.get("inputNodes").and_then(Value::as_array) {
        let inputs: Vec<Value> = arr.iter().filter(|v| v.is_string()).cloned().collect();
        if !inputs.is_empty() {
            m.insert("inputs".into(), Value::Array(inputs));
        }
    }

    // fields / originalFields: must fit closed field schema
    let fields_val = bobj.get("fields");
    let fields_ok = fields_val.map(fields_fit_schema).unwrap_or(false);
    let orig_ok = match bobj.get("originalFields") {
        Some(f) => fields_fit_schema(f),
        None => true, // absent is fine
    };
    if !orig_ok {
        return rawnode_fallback(key, body);
    }

    // dimensions/measures consistency with fields partition; the emitter
    // omits empty lists, so an absent key is only consistent with an empty
    // expected partition.
    if fields_ok {
        let expected = crate::gen::partition_fields(fields_val.unwrap().as_array().unwrap());
        let dims_ok = match bobj.get("dimensions") {
            Some(d) => *d == Value::Array(expected.0.clone()),
            None => expected.0.is_empty(),
        };
        let meas_ok = match bobj.get("measures") {
            Some(mm) => *mm == Value::Array(expected.1.clone()),
            None => expected.1.is_empty(),
        };
        if !dims_ok || !meas_ok {
            return rawnode_fallback(key, body);
        }
        m.insert("fields".into(), fields_val.unwrap().clone());
    } else {
        // no usable fields: only Select/Output may derive
        if ty != "Select" && ty != "Output" {
            return rawnode_fallback(key, body);
        }
        // Select/Output without fields -> spec omits fields (derive)
        // but if wire HAS fields that don't fit schema -> rawNode
        if bobj.contains_key("fields") {
            return rawnode_fallback(key, body);
        }
    }
    if let Some(of) = bobj.get("originalFields") {
        m.insert("originalFields".into(), of.clone());
    }

    // steps (Select): typed where possible else rawStep
    if let Some(steps) = bobj.get("steps").and_then(Value::as_array) {
        let mut out = Vec::new();
        for s in steps {
            out.push(step_to_spec(s));
        }
        m.insert("steps".into(), Value::Array(out));
    }

    // remaining keys -> config (whitelist) / raw
    let mut cfg = Map::new();
    let mut raw = Map::new();
    for (k, v) in bobj {
        match k.as_str() {
            "type" | "id" | "alias" | "inputNodes" | "fields" | "originalFields" | "dimensions"
            | "measures" | "steps" | "lastModifyTime" => {}
            "stepCount" => {
                cfg.insert(k.clone(), v.clone());
            }
            _ => {
                if config_key_allowed(ty, k) {
                    cfg.insert(k.clone(), v.clone());
                } else {
                    raw.insert(k.clone(), v.clone());
                }
            }
        }
    }
    if !cfg.is_empty() {
        m.insert("config".into(), Value::Object(cfg));
    }
    if !raw.is_empty() {
        m.insert("raw".into(), Value::Object(raw));
    }
    Some(Value::Object(m))
}

fn rawnode_fallback(key: &str, body: &Value) -> Option<Value> {
    // rawNode bodies are emitted verbatim, while the equality check strips
    // `dataFlow.nodes.*.lastModifyTime` from the input. A rawNode carrying
    // that key can therefore never regenerate the normalized wire.
    if body.get("lastModifyTime").is_some() {
        return None;
    }
    let mut m = Map::new();
    m.insert("key".into(), Value::String(key.to_string()));
    m.insert("rawNode".into(), body.clone());
    Some(Value::Object(m))
}

fn fields_fit_schema(f: &Value) -> bool {
    let Some(arr) = f.as_array() else {
        return false;
    };
    arr.iter().all(|field| {
        field
            .as_object()
            .map(|o| {
                o.keys().all(|k| field_key_allowed(k))
                    && o.get("name")
                        .and_then(Value::as_str)
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                    && o.get("dataType")
                        .and_then(Value::as_str)
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                    // the saver omits a false role; absent reads as a measure
                    && o.get("isDimension").map(Value::is_boolean).unwrap_or(true)
            })
            .unwrap_or(false)
    })
}

fn step_to_spec(s: &Value) -> Value {
    let ty = s.get("type").and_then(Value::as_str).unwrap_or("");
    // `originFieldInfo` is editor metadata tblgen itself emits on typed
    // Refactor renames; it is regenerated on recompile, so strip it before
    // the fit check to keep our own wire typed. A step that still does not
    // fit keeps the hint verbatim inside rawStep.
    let stripped;
    let candidate = if ty == "Refactor"
        && s.as_object()
            .map(|o| o.contains_key("originFieldInfo"))
            .unwrap_or(false)
    {
        let mut m = s.as_object().cloned().unwrap_or_default();
        m.remove("originFieldInfo");
        stripped = Value::Object(m);
        &stripped
    } else {
        s
    };
    let fits = step_supported(ty)
        && candidate
            .as_object()
            .map(|o| {
                o.keys()
                    .all(|k| step_allowed_keys(ty).contains(&k.as_str()))
                    && step_payloads_fit(o, ty)
            })
            .unwrap_or(false);
    if fits {
        return candidate.clone();
    }
    // rawStep escape
    let mut m = Map::new();
    m.insert("rawStep".into(), s.clone());
    Value::Object(m)
}

fn step_payloads_fit(o: &serde_json::Map<String, Value>, ty: &str) -> bool {
    for (pname, keys) in STEP_PAYLOAD_KEYS {
        if pname == "group" {
            continue;
        }
        if let Some(pv) = o.get(pname) {
            // only check payload keys relevant to this step type
            if !step_allowed_keys(ty).contains(&pname) {
                continue;
            }
            let Some(pobj) = pv.as_object() else {
                return false;
            };
            if !pobj.keys().all(|k| keys.contains(&k.as_str())) {
                return false;
            }
        }
    }
    true
}

fn subprocess_to_spec(item: &Value) -> Option<Value> {
    let Some(iobj) = item.as_object() else {
        return None;
    };
    let mut m = Map::new();
    let mut raw = Map::new();
    for (k, v) in iobj {
        match k.as_str() {
            "id" | "caption" | "rootNodeId" | "nodeIds" => {
                m.insert(k.clone(), v.clone());
            }
            _ => {
                raw.insert(k.clone(), v.clone());
            }
        }
    }
    if !m.contains_key("rootNodeId") || !m.contains_key("nodeIds") {
        return None;
    }
    if !raw.is_empty() {
        m.insert("raw".into(), Value::Object(raw));
    }
    Some(Value::Object(m))
}

fn archive_form(tbl: &Value) -> Value {
    let mut top = tbl.as_object().cloned().unwrap_or_default();
    let flow_val = top.remove("dataFlow").unwrap_or(Value::Null);
    let mut flow = flow_val.as_object().cloned().unwrap_or_default();
    let nodes_val = flow.remove("nodes").unwrap_or(Value::Null);
    let mut node_entries = Vec::new();
    if let Some(nobj) = nodes_val.as_object() {
        for (id, body) in nobj {
            let mut e = Map::new();
            e.insert("id".into(), Value::String(id.clone()));
            e.insert("body".into(), body.clone());
            node_entries.push(Value::Object(e));
        }
    }
    let mut archive = Map::new();
    archive.insert("top".into(), Value::Object(top));
    archive.insert("flow".into(), Value::Object(flow));
    archive.insert("nodes".into(), Value::Array(node_entries));
    let mut out = Map::new();
    out.insert("format".into(), Value::from(1));
    out.insert("archive".into(), Value::Object(archive));
    Value::Object(out)
}
