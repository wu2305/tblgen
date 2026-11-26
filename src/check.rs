//! `check` — structural and coverage-rule validation of a `.tbl` wire
//! document's `dataFlow`. Returns diagnostics with RFC 6901 paths into the
//! wire document and product evidence citations in `source`.
//!
//! Severity convention: diagnostics whose `rule` starts with `warn.` are
//! warnings; everything else is an error. The CLI splits on that prefix.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::model::*;

mod lineage;
mod output;

const ERR_ARITY: &str = "err.dataflow.node.input";

/// Per-file check result for the `check` CLI report.
pub struct CheckReport {
    pub file: String,
    pub scope: String,
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
}

/// Read and validate one file. `.tbl` wire objects are checked via `check`;
/// spec-format documents (format:1 + nodes/archive/rawTable) via
/// `validate_spec`; anything else is reported `skipped` (nonflow models are
/// not assessed).
pub fn check_file(path: &Path) -> Result<CheckReport, (String, i32)> {
    let file = path.display().to_string();
    let text = fs::read_to_string(path).map_err(|e| (format!("{}: {}", file, e), 2))?;
    if file.ends_with(".action.ts") {
        let at = crate::project::locate(path).map(|l| l.platform_path).unwrap_or_else(|| file.clone());
        let errors = crate::script::check_script(&at, &text);
        return Ok(CheckReport { file, scope: "script".into(), errors, warnings: Vec::new() });
    }
    // tolerate an optional UTF-8 BOM on JSON reads
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let v: Value =
        serde_json::from_str(text).map_err(|e| (format!("{}: invalid JSON: {}", file, e), 1))?;

    // never skip a non-object root: it cannot be a valid .tbl document
    if !v.is_object() {
        let mut errors = Vec::new();
        let warnings = Vec::new();
        errors.push(Diag::wire(
            String::new(),
            "structure.root",
            "",
            "document root must be a JSON object",
        ));
        return Ok(CheckReport {
            file,
            scope: "invalid".into(),
            errors,
            warnings,
        });
    }

    let is_spec = v.get("format").and_then(Value::as_u64) == Some(1)
        && (v.get("nodes").is_some() || v.get("archive").is_some() || v.get("rawTable").is_some());
    // properties.modelDataType:"DataFlow" declares a flow document even when
    // the dataFlow member is absent/malformed — route it to check().
    let declares_flow = v
        .pointer("/properties/modelDataType")
        .and_then(Value::as_str)
        == Some("DataFlow");

    let (scope, diags) = if is_spec {
        ("spec", crate::gen::validate_spec(&v))
    } else if v.get("dataFlow").is_some() || declares_flow {
        let mut d = check(&v);
        if let Some(at) = crate::project::locate(path) {
            d.extend(check_references(&v, &at));
        }
        ("dataflow", d)
    } else {
        ("skipped", Vec::new())
    };

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for d in diags {
        if d.is_warning() {
            warnings.push(d);
        } else {
            errors.push(d);
        }
    }
    Ok(CheckReport {
        file,
        scope: scope.into(),
        errors,
        warnings,
    })
}

fn err(path: String, rule: &str, source: &str, msg: impl Into<String>) -> Diag {
    Diag::wire(path, rule, source, msg)
}
fn warn(path: String, rule: &str, source: &str, msg: impl Into<String>) -> Diag {
    Diag::wire(path, format!("warn.{}", rule), source, msg)
}

fn src(file: &str, line: &str, sym: &str) -> String {
    format!("{}:{} {}", file, line, sym)
}

/// Validate the `dataFlow` section of a parsed `.tbl` object.
/// Caller decides scope: objects without `dataFlow` are "skipped" (nonflow
/// models are not assessed).
struct NodeInfo {
    ty: String,
    alias: Option<String>,
    inputs: Vec<String>,
    field_names: Option<Vec<String>>,
}

pub fn check(tbl: &Value) -> Vec<Diag> {
    let mut diags = Vec::new();
    let declares_flow = tbl
        .pointer("/properties/modelDataType")
        .and_then(Value::as_str)
        == Some("DataFlow");
    let Some(flow) = tbl.get("dataFlow") else {
        if declares_flow {
            diags.push(err(
                "/dataFlow".into(),
                "structure.dataFlow",
                &src(cite::LOADER, "1242", "DwTableLoader#loadDataFlow"),
                "properties.modelDataType is \"DataFlow\" but dataFlow is missing",
            ));
        }
        return diags;
    };
    if !flow.is_object() {
        diags.push(err(
            "/dataFlow".into(),
            "structure.dataFlow",
            &src(cite::LOADER, "1242", "DwTableLoader#loadDataFlow"),
            "dataFlow must be an object",
        ));
        return diags;
    }
    let Some(nodes) = flow.get("nodes") else {
        diags.push(err(
            "/dataFlow".into(),
            "structure.dataFlow.nodes",
            &src(cite::LOADER, "1242", "DwTableLoader#loadDataFlow"),
            "dataFlow.nodes is required",
        ));
        return diags;
    };
    let Some(nodes_obj) = nodes.as_object() else {
        diags.push(err(
            "/dataFlow/nodes".into(),
            "structure.dataFlow.nodes",
            &src(cite::LOADER, "1242", "DwTableLoader#loadDataFlow"),
            "dataFlow.nodes must be an object keyed by node id",
        ));
        return diags;
    };

    // Pass 1: per-node shape + collect graph.
    let mut infos: HashMap<String, NodeInfo> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (key, node) in nodes_obj {
        let base = format!("/dataFlow/nodes/{}", ptr_escape(key));
        if !node.is_object() {
            diags.push(err(
                base,
                "structure.node",
                &src(cite::NODE_IMPL, "347", "DataFlowNodeImpl#loadNodeInfo"),
                "node must be an object",
            ));
            continue;
        }
        let ty = match node.get("type").and_then(Value::as_str) {
            Some(t) => t.to_string(),
            None => {
                diags.push(err(
                    format!("{}/type", base),
                    "structure.node.type",
                    &src(cite::NODE_IMPL, "352", "DataFlowNodeImpl#setType"),
                    "node.type must be a string",
                ));
                continue;
            }
        };
        if !NODE_TYPES.contains(&ty.as_str()) {
            diags.push(err(
                format!("{}/type", base),
                "enum.nodeType",
                &src(cite::NODE_IMPL, "338", "DataFlowNodeType.valueOf"),
                format!(
                    "unknown node type {:?} (not a DataFlowNodeType enum value)",
                    ty
                ),
            ));
            // still register so references resolve
            infos.insert(
                key.clone(),
                NodeInfo {
                    ty,
                    alias: None,
                    inputs: vec![],
                    field_names: None,
                },
            );
            order.push(key.clone());
            continue;
        }
        if !node_supported(&ty) {
            diags.push(warn(
                format!("{}/type", base),
                "coverage.node.unknown",
                &src(cite::DWUTIL, "331", "DwUtilDataFlow#getInstance"),
                format!(
                    "node type {} has no analyzed semantics (extend fallback); fidelity only",
                    ty
                ),
            ));
        }

        let mut inputs = Vec::new();
        match node.get("inputNodes") {
            None | Some(Value::Null) => {}
            Some(Value::Array(arr)) => {
                for (i, v) in arr.iter().enumerate() {
                    match v.as_str() {
                        Some(s) => inputs.push(s.to_string()),
                        None => diags.push(err(
                            format!("{}/inputNodes/{}", base, i),
                            "structure.inputNodes",
                            &src(cite::NODE_IMPL, "356", "DataFlowNodeImpl#loadNodeInfo"),
                            "inputNodes entries must be strings (node ids)",
                        )),
                    }
                }
            }
            Some(_) => diags.push(err(
                format!("{}/inputNodes", base),
                "structure.inputNodes",
                &src(cite::NODE_IMPL, "356", "DataFlowNodeImpl#loadNodeInfo"),
                "inputNodes must be an array of node ids",
            )),
        }
        // duplicate edges: editor-only invariant (canLink), the server
        // loader does not reject them.
        let mut seen = HashSet::new();
        for inp in &inputs {
            if !seen.insert(inp.clone()) {
                diags.push(warn(
                    format!("{}/inputNodes", base),
                    "editor.inputNodes.duplicate",
                    &src(cite::BUILDER_TS, "19874", "canLink"),
                    format!("duplicate input edge to {} (editor canLink invariant)", inp),
                ));
            }
        }

        check_fields(&mut diags, node, &base, "fields", true);
        check_fields(&mut diags, node, &base, "originalFields", false);
        check_node_condition_keys(&mut diags, node, &base);

        let field_names = node.get("fields").and_then(Value::as_array).map(|arr| {
            arr.iter()
                .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
        });

        infos.insert(
            key.clone(),
            NodeInfo {
                ty: ty.clone(),
                alias: node
                    .get("alias")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                inputs,
                field_names,
            },
        );
        order.push(key.clone());
    }

    // Pass 2: graph-level checks (dangling, cycles, arity, per-type rules).
    // dangling inputs
    for key in &order {
        let info = &infos[key];
        let base = format!("/dataFlow/nodes/{}", ptr_escape(key));
        for inp in &info.inputs {
            if !nodes_obj.contains_key(inp) {
                // Java: getInputExes puts a null executor into the list
                // (DwDataFlowContext.java:411-418) and ExecutorImpl#init
                // dereferences it (DwDataFlowNodeExecutorImpl.java:225-229)
                // -> hard error, not an editor-only policy.
                diags.push(err(
                    format!("{}/inputNodes", base),
                    "graph.danglingInput",
                    &format!(
                        "{}:411 DwDataFlowContext#getInputExes; {}:225 DwDataFlowNodeExecutorImpl#init",
                        cite::DATAFLOW_CTX, cite::NODE_EXECUTOR
                    ),
                    format!("inputNodes references unknown node id {:?}", inp),
                ));
            }
        }
    }
    check_acyclic(&mut diags, &infos, &order);
    check_arity_and_type_rules(&mut diags, tbl, nodes_obj, &infos, &order);
    let param_names_w: HashSet<String> = tbl
        .get("parameters")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| p.get("name").and_then(Value::as_str))
                .map(|n| n.to_uppercase())
                .collect()
        })
        .unwrap_or_default();
    check_select_steps(&mut diags, nodes_obj, &infos, &order, &param_names_w);

    // light top-level checks
    check_top_level(&mut diags, tbl, nodes_obj, &infos);
    lineage::check_lineage(&mut diags, nodes_obj);
    diags
}

fn check_fields(diags: &mut Vec<Diag>, node: &Value, base: &str, key: &str, filtered: bool) {
    let Some(v) = node.get(key) else { return };
    let Some(arr) = v.as_array() else {
        diags.push(err(
            format!("{base}/{key}"),
            "structure.fields",
            &src(cite::NODE_IMPL, "692", "DataFlowNodeImpl#loadField"),
            format!("{} must be an array of field objects", key),
        ));
        return;
    };
    for (i, f) in arr.iter().enumerate() {
        if !f.is_object() {
            diags.push(err(
                format!("{base}/{key}/{i}"),
                "structure.field",
                &src(cite::NODE_IMPL, "692", "DataFlowNodeImpl#loadField"),
                "field must be an object",
            ));
            continue;
        }
        let name_ok = f
            .get("name")
            .and_then(Value::as_str)
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !name_ok {
            diags.push(err(
                format!("{base}/{key}/{i}/name"),
                "field.name",
                &src(cite::NODE_IMPL, "692", "DataFlowNodeImpl#loadField"),
                "field.name must be a non-empty string",
            ));
        }
        // dataType: node loadField does charAt(0); empty/missing breaks unless
        // the field is filtered out of `fields` (invalid && !isOriginalFieldInvalid).
        // originalFields are not filtered, so the exception does not apply there.
        let skipped = filtered
            && f.get("invalid").and_then(Value::as_bool).unwrap_or(false)
            && !f
                .get("isOriginalFieldInvalid")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if !skipped {
            let dt_ok = f
                .get("dataType")
                .and_then(Value::as_str)
                .map(|s| !s.is_empty())
                .unwrap_or(false);
            if !dt_ok {
                diags.push(err(
                    format!("{base}/{key}/{i}/dataType"),
                    "field.dataType",
                    &src(cite::NODE_IMPL, "696", "DataFlowNodeImpl#loadField"),
                    "field.dataType must be a non-empty string (type charAt(0))",
                ));
            }
        }
    }
}

fn check_acyclic(diags: &mut Vec<Diag>, infos: &HashMap<String, NodeInfo>, order: &[String]) {
    // iterative topo; leftover = cycle
    let mut done: HashSet<String> = HashSet::new();
    let mut progress = true;
    while progress {
        progress = false;
        for key in order {
            if done.contains(key) {
                continue;
            }
            let inputs = &infos[key].inputs;
            if inputs
                .iter()
                .all(|i| done.contains(i) || !infos.contains_key(i))
            {
                done.insert(key.clone());
                progress = true;
            }
        }
    }
    let cyclic: Vec<&String> = order.iter().filter(|k| !done.contains(*k)).collect();
    if !cyclic.is_empty() {
        diags.push(err(
            "/dataFlow/nodes".into(),
            "graph.cycle",
            // editor prevents cycles via willLoop (dwbuilder.ts:19878);
            // server compile would recurse forever -> hard structural error.
            &src(cite::BUILDER_TS, "19878", "willLoop"),
            format!(
                "input graph has a cycle involving: {}",
                cyclic
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
}

fn check_arity_and_type_rules(
    diags: &mut Vec<Diag>,
    tbl: &Value,
    nodes_obj: &serde_json::Map<String, Value>,
    infos: &HashMap<String, NodeInfo>,
    order: &[String],
) {
    for key in order {
        let info = &infos[key];
        let node = &nodes_obj[key];
        let base = format!("/dataFlow/nodes/{}", ptr_escape(key));
        let n = info.inputs.len();
        let ty = info.ty.as_str();

        let arity_src = match ty {
            "Select" => src(
                cite::SELECT_NODE,
                "58",
                "DwDataFlowNodeContributor_select#init",
            ),
            "Output" | "DbOutput" => src(
                cite::OUTPUT_NODE,
                "207",
                "DwDataFlowNodeContributor_output#getPreExe",
            ),
            "Join" => src(
                cite::JOIN_NODE,
                "77",
                "DwDataFlowNodeContributor_join#compileQuery",
            ),
            "Union" => src(
                cite::UNION_NODE,
                "155",
                "DwDataFlowNodeContributor_union#compileQuery",
            ),
            _ => src(
                cite::BASE_CONTRIB,
                "274",
                "DwDataFlowNodeContributorImpl#getPreNodeExe",
            ),
        };

        match ty {
            "Select" | "Output" | "DbOutput" => {
                if n != 1 {
                    diags.push(err(
                        format!("{}/inputNodes", base),
                        ERR_ARITY,
                        &arity_src,
                        format!("{} requires exactly 1 input, found {}", ty, n),
                    ));
                }
            }
            "Join" => {
                if n < 2 {
                    diags.push(err(
                        format!("{}/inputNodes", base),
                        ERR_ARITY,
                        &arity_src,
                        format!("Join requires at least 2 inputs (MustMore), found {}", n),
                    ));
                }
                check_join_conditions(diags, node, &base, infos, n);
            }
            "Union" => {
                // 0 inputs cannot fetch the first non-empty query branch.
                if n == 0 {
                    diags.push(err(
                        format!("{}/inputNodes", base),
                        ERR_ARITY,
                        &arity_src,
                        "Union requires at least 1 input branch",
                    ));
                }
                check_union(diags, node, &base, infos, n);
            }
            "Agg" | "Distinct" => {
                // contributors read the FIRST input only — extra inputs
                // would be silently ignored
                if n != 1 {
                    diags.push(err(
                        format!("{}/inputNodes", base),
                        ERR_ARITY,
                        &arity_src,
                        format!("{} requires exactly 1 input, found {}", ty, n),
                    ));
                }
            }
            "RowToCol" | "ColToRow" | "Snapshot" | "Update" => {
                if n == 0 {
                    diags.push(warn(
                        format!("{}/inputNodes", base),
                        "warn.node.noInput",
                        &arity_src,
                        format!("{} has no inputs; contributor returns null query", ty),
                    ));
                }
            }
            "DbTable" => {
                // editor canLink restricts DbTable predecessors to Sql/Script;
                // the server executes whatever inputs exist, so editor-only.
                for inp in &info.inputs {
                    if let Some(ii) = infos.get(inp) {
                        if ii.ty != "Sql" && ii.ty != "Script" {
                            diags.push(warn(
                                format!("{}/inputNodes", base),
                                "editor.dbtable.predecessor",
                                &format!(
                                    "{}:19831 canLink (editor-only); {}:107 DwDataFlowNodeContributor_dbtable#compileQuery",
                                    cite::BUILDER_TS, cite::DBTABLE_NODE
                                ),
                                format!(
                                    "DbTable predecessor {:?} is {} — editor canLink expects Sql/Script",
                                    inp, ii.ty
                                ),
                            ));
                        }
                    }
                }
            }
            "Sql" => {
                // Frontend canLink (dwbuilder.ts:19862) restricts Sql
                // predecessors, but Java sql#compileQuery executes EVERY prior
                // node — so this is an editor-only policy, not a wire error.
                for inp in &info.inputs {
                    if let Some(ii) = infos.get(inp) {
                        if ii.ty != "Sql" && ii.ty != "DbOutput" {
                            diags.push(warn(
                                format!("{}/inputNodes", base),
                                "editor.sql.predecessor",
                                &format!(
                                    "{}:19863 canLink (editor-only); {}:225 DwDataFlowNodeContributor_sql#compileQuery executes prior nodes",
                                    cite::BUILDER_TS, cite::SQL_NODE
                                ),
                                format!(
                                    "Sql predecessor {:?} is {} — editor canLink expects Sql/DbOutput only",
                                    inp, ii.ty
                                ),
                            ));
                        }
                    }
                }
            }
            "FileInput" | "ModelTable" => {
                if n > 0 {
                    diags.push(err(
                        format!("{}/inputNodes", base),
                        ERR_ARITY,
                        &arity_src,
                        format!("{} takes no graph inputs, found {}", ty, n),
                    ));
                }
            }
            "SubProcess" => {
                if n > 0 {
                    diags.push(warn(
                        format!("{}/inputNodes", base),
                        "warn.node.subprocess.inputs",
                        &src(
                            cite::SUBPROCESS_NODE,
                            "20",
                            "DwDataFlowNodeContributor_subProcess#compileQuery",
                        ),
                        "SubProcess resolves via subProcessId; graph inputs are not used",
                    ));
                }
                check_subprocess(diags, tbl, node, &base);
            }
            _ => {}
        }

        // type-specific payload checks
        match ty {
            "ModelTable" => {
                // unresolved external module table: warn, not proof of
                // executability
                if node
                    .get("moduleTablePath")
                    .and_then(Value::as_str)
                    .map(|x| x.trim().is_empty())
                    .unwrap_or(true)
                {
                    diags.push(err(
                        format!("{}/moduleTablePath", base),
                        "config.modeltable.path",
                        &src(
                            cite::DWTABLE_NODE,
                            "47",
                            "DwDataFlowNodeContributor_dwtable#init",
                        ),
                        "ModelTable.moduleTablePath must be non-blank (init throws err.dataflow.dwtable.blankpath)",
                    ));
                }
            }
            "FileInput" => {
                // unresolved external resource is a warning, not proof of
                // executability
                if node
                    .get("fileName")
                    .and_then(Value::as_str)
                    .map(|x| x.trim().is_empty())
                    .unwrap_or(true)
                {
                    diags.push(warn(
                        format!("{}/fileName", base),
                        "node.fileinput.file",
                        &src(
                            cite::FILEINPUT_INFO,
                            "297",
                            "FileInputInfoImpl#loadInputInfoWithMap",
                        ),
                        "FileInput.fileName missing/blank — external resource unresolved",
                    ));
                }
            }
            "ColToRow" => {
                let pred_names = info
                    .inputs
                    .first()
                    .and_then(|k| infos.get(k))
                    .and_then(|i| i.field_names.as_ref())
                    .filter(|n| !n.is_empty());
                check_col_to_row(diags, node, &base, pred_names);
            }
            "RowToCol" => check_row_to_col(diags, node, &base),
            "Update" => check_update(diags, node, &base, n),
            "Script" => {
                let has_script =
                    node.get("scriptES5").is_some() || node.get("scriptName").is_some();
                if !has_script {
                    diags.push(warn(
                        format!("{}/scriptES5", base),
                        "warn.node.script.empty",
                        &src(
                            cite::SCRIPT_NODE,
                            "148",
                            "DwDataFlowNodeContributor_script#compileQuery",
                        ),
                        "Script node has neither scriptES5 nor scriptName",
                    ));
                }
            }
            "DbOutput" => {
                if node
                    .get("dbTableName")
                    .and_then(Value::as_str)
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
                {
                    diags.push(warn(
                        format!("{}/dbTableName", base),
                        "warn.node.dboutput.table",
                        &src(
                            cite::DBOUTPUT_NODE,
                            "36",
                            "DwDataFlowNodeContributor_dboutput#init",
                        ),
                        "DbOutput.dbTableName missing; physical table cannot be resolved",
                    ));
                }
            }
            "Snapshot" => {
                for k in ["periodStartField", "periodEndField"] {
                    if node.get(k).is_none() {
                        diags.push(warn(
                            format!("{}/{}", base, k),
                            "warn.node.snapshot.period",
                            &src(cite::NODE_IMPL, "381", "DataFlowNodeImpl#loadNodeInfo"),
                            format!("Snapshot.{} not set; period join may be unconstrained", k),
                        ));
                    }
                }
            }
            "Select" => check_select_node_meta(diags, node, &base),
            _ => {}
        }
    }
}

fn check_join_conditions(
    diags: &mut Vec<Diag>,
    node: &Value,
    base: &str,
    infos: &HashMap<String, NodeInfo>,
    n_inputs: usize,
) {
    let Some(conds) = node.get("joinConditions") else {
        return;
    };
    let cpath = format!("{}/joinConditions", base);
    let Some(arr) = conds.as_array() else {
        diags.push(err(
            cpath,
            "structure.joinConditions",
            &src(cite::JOIN_REL, "362", "JoinRelationImpl#loadJsonMap"),
            "joinConditions must be an array",
        ));
        return;
    };
    // Join.BuilderContributor#checkConditions (editor nodeimpl.ts):
    // conditions == inputs - 1
    if n_inputs >= 2 && arr.len() != n_inputs - 1 {
        diags.push(err(
            cpath.clone(),
            "join.conditions.count",
            &src(
                cite::NODEIMPL_TS,
                "2290-2320",
                "Join.BuilderContributor#checkConditions",
            ),
            format!(
                "joinConditions count {} != inputs-1 ({})",
                arr.len(),
                n_inputs - 1
            ),
        ));
    }
    // JoinRelationImpl#loadJsonMap: indexOf(JOINTYPES); unknown -> -1, which
    // is dropped on save
    for (i, c) in arr.iter().enumerate() {
        if let Some(t) = c.get("joinType").and_then(Value::as_str) {
            if !JOIN_TYPES.contains(&t) {
                diags.push(err(
                    format!("{}/{}/joinType", cpath, i),
                    "join.condition.joinType",
                    &src(cite::JOIN_REL, "320", "JoinRelationImpl#loadJsonMap"),
                    format!("joinType {:?} is not one of {:?}", t, JOIN_TYPES),
                ));
            }
        }
    }
    // alias set of this node's inputs
    let input_keys: Vec<String> = node
        .get("inputNodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    // leftTable/rightTable name input node ALIASES (not node ids); a node
    // without an alias falls back to its id.
    let mut aliases: Vec<String> = Vec::new();
    for k in &input_keys {
        if let Some(info) = infos.get(k) {
            aliases.push(info.alias.clone().unwrap_or_else(|| k.clone()));
        }
    }
    for (i, c) in arr.iter().enumerate() {
        let ipath = format!("{}/{}", cpath, i);
        if !c.is_object() {
            diags.push(err(
                ipath,
                "structure.joinCondition",
                &src(cite::JOIN_REL, "362", "JoinRelationImpl#loadJsonMap"),
                "joinConditions entries must be objects",
            ));
            continue;
        }
        for side in ["leftTable", "rightTable"] {
            if let Some(t) = c.get(side).and_then(Value::as_str) {
                if !t.is_empty() && !aliases.is_empty() && !aliases.iter().any(|a| field_eq(a, t)) {
                    diags.push(err(
                        format!("{}/{}", ipath, side),
                        "join.condition.alias",
                        &src(cite::NODE_IMPL, "803", "DataFlowNodeImpl#loadJoinCondition"),
                        format!("{} {:?} does not match any input node alias", side, t),
                    ));
                }
            }
        }
        // clause expressions qualify fields as [alias].[field]; the
        // qualifier must be one of THIS join's input aliases — a node id or
        // spec key is not resolvable at run time.
        if !aliases.is_empty() {
            for (ci, clause) in c
                .get("clauses")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
            {
                for (ek, expv) in [
                    ("leftExp", clause.get("leftExp")),
                    ("rightExp", clause.get("rightExp")),
                    ("exp", clause.get("exp")),
                ] {
                    let Some(e) = expv.and_then(Value::as_str) else {
                        continue;
                    };
                    for q in exp_table_qualifiers(e) {
                        if !aliases.iter().any(|a| field_eq(a, &q)) {
                            diags.push(err(
                                format!("{}/clauses/{}/{}", ipath, ci, ek),
                                "join.condition.qualifier",
                                &src(cite::JOIN_REL, "320", "JoinRelationImpl#loadJsonMap"),
                                format!(
                                    "expression qualifier {:?} is not an input alias {:?}",
                                    q, aliases
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn check_union(
    diags: &mut Vec<Diag>,
    node: &Value,
    base: &str,
    infos: &HashMap<String, NodeInfo>,
    n_inputs: usize,
) {
    if let Some(ut) = node.get("unionType") {
        match ut {
            Value::String(s)
                if ["UnionAll", "Union", "Minus", "Intersect"].contains(&s.as_str()) => {}
            Value::String(s) => diags.push(err(
                format!("{}/unionType", base),
                "enum.unionType",
                &src(cite::NODE_IMPL, "397", "DataFlowNodeImpl#loadNodeInfo"),
                format!("unknown unionType {:?}", s),
            )),
            _ => diags.push(err(
                format!("{}/unionType", base),
                "structure.unionType",
                &src(cite::NODE_IMPL, "397", "DataFlowNodeImpl#loadNodeInfo"),
                "unionType must be a string",
            )),
        }
    }
    // absent or explicit null = legacy name resolution; a present array
    // (even empty or all-hidden rows) still gates runtime field lookups
    let Some(map) = node.get("unionMapArray").filter(|m| !m.is_null()) else {
        return;
    };
    let Some(arr) = map.as_array() else {
        diags.push(err(
            format!("{}/unionMapArray", base),
            "structure.unionMap",
            &src(
                cite::UNION_NODE,
                "56",
                "DwDataFlowNodeContributor_union#loadUnionMap",
            ),
            "unionMapArray must be an array of row objects",
        ));
        return;
    };
    // input aliases in inputNodes order: loadUnionMap keys each row on the
    // FIRST non-empty value, qualified by that input's alias.
    let input_keys: Vec<String> = node
        .get("inputNodes")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let aliases: Vec<String> = input_keys
        .iter()
        .map(|k| {
            infos
                .get(k)
                .and_then(|i| i.alias.clone())
                .unwrap_or_else(|| k.clone())
        })
        .collect();
    let mut keys: HashSet<String> = HashSet::new();
    for (i, item) in arr.iter().enumerate() {
        let ipath = format!("{}/unionMapArray/{}", base, i);
        let Some(row) = item.as_object() else {
            diags.push(err(
                ipath,
                "structure.unionMap.row",
                &src(
                    cite::UNION_NODE,
                    "56",
                    "DwDataFlowNodeContributor_union#loadUnionMap",
                ),
                "unionMapArray rows must be objects",
            ));
            continue;
        };
        // ignored rows are skipped before any value validation
        if row.get("visible").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let values = match row.get("values").and_then(Value::as_array) {
            Some(v) => v,
            None => {
                diags.push(err(
                    format!("{}/values", ipath),
                    "structure.unionMap.values",
                    &src(
                        cite::UNION_NODE,
                        "56",
                        "DwDataFlowNodeContributor_union#loadUnionMap",
                    ),
                    "unionMapArray row requires a values array",
                ));
                continue;
            }
        };
        if values.len() != n_inputs {
            diags.push(err(
                format!("{}/values", ipath),
                "union.mapkey",
                &src(
                    cite::UNION_NODE,
                    "56",
                    "DwDataFlowNodeContributor_union#loadUnionMap",
                ),
                format!(
                    "unionMapArray[{}].values width {} != input count {}",
                    i,
                    values.len(),
                    n_inputs
                ),
            ));
        }
        let mut anchor: Option<(usize, String)> = None;
        for (vi, v) in values.iter().enumerate() {
            match v {
                Value::String(s) => {
                    if !s.is_empty() && anchor.is_none() {
                        anchor = Some((vi, s.clone()));
                    }
                }
                Value::Null => {}
                _ => diags.push(err(
                    format!("{}/values/{}", ipath, vi),
                    "structure.unionMap.values",
                    &src(
                        cite::UNION_NODE,
                        "56",
                        "DwDataFlowNodeContributor_union#loadUnionMap",
                    ),
                    "unionMapArray values must be strings or null",
                )),
            }
        }
        if let Some((vi, val)) = anchor {
            let a = aliases.get(vi).cloned().unwrap_or_default();
            let key =
                format!("[{}].[{}]", a.replace(']', "]]"), val.replace(']', "]]")).to_uppercase();
            if !keys.insert(key.clone()) {
                diags.push(err(
                    format!("{}/unionMapArray/{}", base, i),
                    "union.mapkey",
                    &src(
                        cite::UNION_NODE,
                        "56",
                        "DwDataFlowNodeContributor_union#loadUnionMap",
                    ),
                    format!("duplicate union map key {:?}", key),
                ));
            }
        }
    }
    // every runtime-read field must resolve to a map key: computed fields
    // only when they carry a non-empty inputField (Java skips the rest);
    // hidden/invalid-declared fields are skipped like Select tracking.
    // Java blank semantics apply to inputField/inputNode/exp fallbacks.
    let nonblank = |v: Option<&Value>| {
        v.and_then(Value::as_str)
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    };
    let mut claimed: HashSet<String> = HashSet::new();
    if let Some(farr) = node.get("fields").and_then(Value::as_array) {
        for (fi, f) in farr.iter().enumerate() {
            if f.get("hidden").and_then(Value::as_bool) == Some(true) {
                continue;
            }
            if skipped_field(f) {
                continue;
            }
            let computed = nonblank(f.get("exp"));
            let input_field = f
                .get("inputField")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            if computed && input_field.is_none() {
                continue;
            }
            let eff = match input_field {
                Some(s) => s.to_string(),
                None => match f.get("name").and_then(Value::as_str) {
                    Some(n) if !n.trim().is_empty() => n.to_string(),
                    _ => continue,
                },
            };
            let candidate = match f.get("inputNode").and_then(Value::as_str) {
                Some(in_) if !in_.trim().is_empty() => {
                    format!("[{}].[{}]", in_.replace(']', "]]"), eff.replace(']', "]]"))
                        .to_uppercase()
                }
                _ => eff.to_uppercase(),
            };
            if !keys.contains(&candidate) {
                diags.push(err(
                    format!("{}/fields/{}/inputField", base, fi),
                    "union.mapkey",
                    &src(
                        cite::UNION_NODE,
                        "56",
                        "DwDataFlowNodeContributor_union#loadUnionMap",
                    ),
                    format!(
                        "union field {:?} inputFieldExp {:?} matches no unionMapArray key",
                        f.get("name").and_then(Value::as_str).unwrap_or(""),
                        candidate
                    ),
                ));
                continue;
            }
            // two visible noncomputed fields resolving to the same map key
            // would ambiguously project one runtime column
            if !computed && !claimed.insert(candidate.clone()) {
                diags.push(err(
                    format!("{}/fields/{}/inputField", base, fi),
                    "union.mapkey",
                    &src(
                        cite::UNION_NODE,
                        "56",
                        "DwDataFlowNodeContributor_union#loadUnionMap",
                    ),
                    format!(
                        "union fields share map key {:?} (ambiguous projection)",
                        candidate
                    ),
                ));
            }
        }
    }
}

fn check_subprocess(diags: &mut Vec<Diag>, tbl: &Value, node: &Value, base: &str) {
    let Some(spid) = node.get("subProcessId").and_then(Value::as_str) else {
        diags.push(warn(
            format!("{}/subProcessId", base),
            "warn.node.subprocess.noid",
            &src(
                cite::SUBPROCESS_NODE,
                "20",
                "DwDataFlowNodeContributor_subProcess#compileQuery",
            ),
            "SubProcess.subProcessId missing; node compiles to null",
        ));
        return;
    };
    if spid.is_empty() {
        return;
    }
    // subProcessId resolves through dataFlow.subProcesses; warn when the
    // referenced root cannot be found (node then compiles to null).
    let has_subs = tbl
        .get("dataFlow")
        .and_then(|f| f.get("subProcesses"))
        .and_then(Value::as_object)
        .map(|m| !m.is_empty())
        .unwrap_or(false);
    if !has_subs {
        diags.push(warn(
            format!("{}/subProcessId", base),
            "warn.node.subprocess.unresolved",
            &src(cite::SUBPROCESS_NODE, "20", "DwDataFlowNodeContributor_subProcess#compileQuery"),
            format!("subProcessId {:?} set but dataFlow.subProcesses is empty/missing; node compiles to null", spid),
        ));
    }
}

fn check_col_to_row(
    diags: &mut Vec<Diag>,
    node: &Value,
    base: &str,
    pred_names: Option<&Vec<String>>,
) {
    let Some(info) = node.get("colToRowInfo") else {
        return;
    };
    let ipath = format!("{}/colToRowInfo", base);
    let Some(obj) = info.as_object() else {
        diags.push(err(
            ipath,
            "structure.colToRowInfo",
            &src(cite::NODE_IMPL, "870", "DataFlowNodeImpl#loadColToRowInfo"),
            "colToRowInfo must be an object",
        ));
        return;
    };
    let headers = obj.get("headers").and_then(Value::as_array);
    let values = obj.get("valuesArray").and_then(Value::as_array);
    let (Some(h), Some(v)) = (headers, values) else {
        return;
    };
    if h.is_empty() || v.is_empty() {
        return; // passthrough compile
    }
    let hlen = h.len();
    for (i, hv) in h.iter().enumerate() {
        if !hv.is_string() {
            diags.push(err(
                format!("{}/headers/{}", ipath, i),
                "structure.colToRow.headers",
                &src(cite::NODE_IMPL, "870", "DataFlowNodeImpl#loadColToRowInfo"),
                "headers entries must be strings",
            ));
        }
    }
    for (r, row) in v.iter().enumerate() {
        let rpath = format!("{}/valuesArray/{}", ipath, r);
        let Some(rarr) = row.as_array() else {
            diags.push(err(
                rpath.clone(),
                "structure.colToRow.row",
                &src(cite::NODE_IMPL, "870", "DataFlowNodeImpl#loadColToRowInfo"),
                "valuesArray rows must be arrays",
            ));
            continue;
        };
        if rarr.len() != hlen {
            diags.push(err(
                rpath.clone(),
                "err.dataflow.colToRow.config",
                &src(
                    cite::COLTOROW_NODE,
                    "39",
                    "DwDataFlowNodeContributor_colToRow#compileQuery",
                ),
                format!(
                    "valuesArray[{}] width {} != headers width {}",
                    r,
                    rarr.len(),
                    hlen
                ),
            ));
        }
        for (c, cell) in rarr.iter().enumerate() {
            if let Some(co) = cell.as_object() {
                if let Some(nm) = co.get("name").and_then(Value::as_str) {
                    if nm.is_empty() {
                        diags.push(err(
                            format!("{}/{}/{}", rpath, c, "name"),
                            "field.name",
                            &src(cite::NODE_IMPL, "692", "DataFlowNodeImpl#loadField"),
                            "valuesArray field objects require a non-empty name",
                        ));
                    } else if let Some(names) = pred_names {
                        // pivot source refs must exist in the predecessor's
                        // declared field set when that set is known
                        // (case-insensitive per QueryImpl#getField).
                        if !names.is_empty() && !names.iter().any(|p| field_eq(p, nm)) {
                            diags.push(err(
                                format!("{}/{}/{}", rpath, c, "name"),
                                "field.notfound",
                                &src(cite::NODE_IMPL, "870", "DataFlowNodeImpl#loadColToRowInfo"),
                                format!(
                                    "ColToRow field reference {:?} not found in predecessor fields",
                                    nm
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn check_row_to_col(diags: &mut Vec<Diag>, node: &Value, base: &str) {
    let Some(info) = node.get("rowToColInfo") else {
        return;
    };
    let ipath = format!("{}/rowToColInfo", base);
    let Some(obj) = info.as_object() else {
        diags.push(err(
            ipath,
            "structure.rowToColInfo",
            &src(cite::NODE_IMPL, "895", "DataFlowNodeImpl#loadRowToColInfo"),
            "rowToColInfo must be an object",
        ));
        return;
    };
    let case_len = obj
        .get("caseFields")
        .and_then(Value::as_array)
        .map(|a| a.len());
    if let (Some(cl), Some(tarr)) = (
        case_len,
        obj.get("transformArray").and_then(Value::as_array),
    ) {
        for (i, t) in tarr.iter().enumerate() {
            if let Some(ta) = t.as_array() {
                if ta.len() != cl + 2 {
                    diags.push(err(
                        format!("{}/transformArray/{}", ipath, i),
                        "structure.rowToCol.transform",
                        &src(
                            cite::ROWTOROC_NODE,
                            "45",
                            "DwDataFlowNodeContributor_rowToCol#compileQuery",
                        ),
                        format!(
                            "transformArray[{}] width {} != caseFields({})+2",
                            i,
                            ta.len(),
                            cl
                        ),
                    ));
                }
            }
        }
    }
}

fn check_update(diags: &mut Vec<Diag>, node: &Value, base: &str, n_inputs: usize) {
    if n_inputs > 1 {
        let ok = node
            .get("updateConditions")
            .and_then(Value::as_array)
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if !ok {
            diags.push(err(
                format!("{}/updateConditions", base),
                "err.dataflow.update.conditions",
                &src(cite::NODEIMPL_TS, "1801", "UpdateNodeInfo#checkNodeInfo"),
                "Update with >1 input requires updateConditions",
            ));
        }
    }
    if let Some(conds) = node.get("updateConditions").and_then(Value::as_array) {
        for (i, c) in conds.iter().enumerate() {
            let clauses_empty = c
                .get("updateClauses")
                .and_then(Value::as_array)
                .map(|a| a.is_empty())
                .unwrap_or(true);
            if clauses_empty {
                diags.push(warn(
                    format!("{}/updateConditions/{}/updateClauses", base, i),
                    "warn.node.update.clauses",
                    &src(cite::NODEIMPL_TS, "1801", "UpdateNodeInfo#checkNodeInfo"),
                    "updateConditions entry has no updateClauses",
                ));
            }
        }
    }
}

fn check_select_node_meta(diags: &mut Vec<Diag>, node: &Value, base: &str) {
    if let Some(steps) = node.get("steps") {
        if !steps.is_array() {
            diags.push(err(
                format!("{}/steps", base),
                "structure.steps",
                &src(cite::STEP_IMPL, "96", "DataFlowSelectNodeStepImpl#<init>"),
                "steps must be an array",
            ));
        }
    }
}

/// `filter` and `incrementRange` are read on EVERY node type by the
/// loaders: `filter` is cast to Map unconditionally
/// (DataFlowNodeImpl#loadFilter) and `incrementRange` items are cast to
/// clause maps — wrong shapes are hard ClassCastExceptions.
fn check_node_condition_keys(diags: &mut Vec<Diag>, node: &Value, base: &str) {
    if let Some(fv) = node.get("filter") {
        if !fv.is_null() {
            match fv.as_object() {
                Some(o) => {
                    for k in o.keys() {
                        if !["matchAll", "enabled", "clauses"].contains(&k.as_str()) {
                            diags.push(warn(
                                format!("{}/filter/{}", base, ptr_escape(k)),
                                "structure.node.filter",
                                &src(cite::NODE_IMPL, "758", "DataFlowNodeImpl#loadFilter"),
                                format!("unexpected filter key {:?}", k),
                            ));
                        }
                    }
                    if let Some(cl) = o.get("clauses") {
                        if let Some(arr) = cl.as_array() {
                            for (i, item) in arr.iter().enumerate() {
                                if !item.is_object() {
                                    diags.push(err(
                                        format!("{}/filter/clauses/{}", base, i),
                                        "structure.node.filter",
                                        &src(
                                            cite::NODE_IMPL,
                                            "758",
                                            "DataFlowNodeImpl#loadFilter",
                                        ),
                                        "filter.clauses entries must be objects (loadJsonMap casts each)",
                                    ));
                                }
                            }
                        } else {
                            diags.push(err(
                                format!("{}/filter/clauses", base),
                                "structure.node.filter",
                                &src(cite::NODE_IMPL, "758", "DataFlowNodeImpl#loadFilter"),
                                "filter.clauses must be an array",
                            ));
                        }
                    }
                }
                None => diags.push(err(
                    format!("{}/filter", base),
                    "structure.node.filter",
                    &src(cite::NODE_IMPL, "758", "DataFlowNodeImpl#loadFilter"),
                    "filter must be an object (loader casts to Map)",
                )),
            }
        }
    }
    if let Some(iv) = node.get("incrementRange") {
        if !iv.is_null() {
            match iv.as_array() {
                Some(arr) => {
                    for (i, item) in arr.iter().enumerate() {
                        if !item.is_object() {
                            diags.push(err(
                                format!("{}/incrementRange/{}", base, i),
                                "structure.node.incrementRange",
                                &src(
                                    cite::NODE_IMPL,
                                    "485",
                                    "DataFlowNodeImpl#loadNodeInfo",
                                ),
                                "incrementRange entries must be clause objects (loadJsonMap casts each)",
                            ));
                        }
                    }
                }
                None => diags.push(err(
                    format!("{}/incrementRange", base),
                    "structure.node.incrementRange",
                    &src(cite::NODE_IMPL, "485", "DataFlowNodeImpl#loadNodeInfo"),
                    "incrementRange must be an array of clause objects",
                )),
            }
        }
    }
}

/// Ordered Select-step names plus a multiset index for existence lookups.
/// `evolve_names` remains the authority for list mutation; the index updates
/// only for the three step kinds that can change membership.
struct TrackedNames {
    ordered: Vec<String>,
    counts: HashMap<String, usize>,
}

impl TrackedNames {
    fn new(ordered: Vec<String>) -> Self {
        let mut counts = HashMap::new();
        for name in &ordered {
            *counts.entry(name.to_uppercase()).or_insert(0) += 1;
        }
        Self { ordered, counts }
    }

    fn contains(&self, name: &str) -> bool {
        self.counts.contains_key(&name.to_uppercase())
    }

    fn evolve(&mut self, step: &Value, step_fields: &[String]) -> Result<bool, &'static str> {
        let ty = step.get("type").and_then(Value::as_str).unwrap_or("");
        let mut remove_keys = Vec::new();
        if ty == "RemoveField" {
            for name in step_fields {
                let key = name.to_uppercase();
                if self.counts.contains_key(&key) && !remove_keys.contains(&key) {
                    remove_keys.push(key);
                }
            }
        }
        let rename_keys = if ty == "Refactor" {
            let rename = step
                .pointer("/refactor/rename")
                .and_then(Value::as_str)
                .filter(|rn| !rn.is_empty());
            match (step_fields.first(), rename) {
                (Some(old), Some(new)) if self.contains(old) => {
                    Some((old.to_uppercase(), new.to_uppercase()))
                }
                _ => None,
            }
        } else {
            None
        };
        let add_key = if ty == "AddField" {
            step.pointer("/addField/name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .map(str::to_uppercase)
        } else {
            None
        };

        let result = evolve_names(&mut self.ordered, step);
        if !matches!(result, Ok(true)) {
            return result;
        }
        for key in remove_keys {
            self.counts.remove(&key);
        }
        if let Some((old, new)) = rename_keys {
            if old != new {
                if let Some(left) = self.counts.get_mut(&old) {
                    *left -= 1;
                    if *left == 0 {
                        self.counts.remove(&old);
                    }
                }
                *self.counts.entry(new).or_insert(0) += 1;
            }
        }
        if let Some(key) = add_key {
            *self.counts.entry(key).or_insert(0) += 1;
        }
        result
    }
}

/// Select step field-existence tracking across earlier transformations.
/// The pre-step field set comes from the *input node's* declared `fields`
/// output — never from the Select's own final `fields`. When the set cannot
/// be established we skip checks rather than guess (per coverage rule).
fn check_select_steps(
    diags: &mut Vec<Diag>,
    nodes_obj: &serde_json::Map<String, Value>,
    infos: &HashMap<String, NodeInfo>,
    order: &[String],
    param_names: &HashSet<String>,
) {
    for key in order {
        let info = &infos[key];
        if info.ty != "Select" || info.inputs.len() != 1 {
            continue;
        }
        let node = &nodes_obj[key];
        let base = format!("/dataFlow/nodes/{}", ptr_escape(key));
        let Some(steps) = node.get("steps").and_then(Value::as_array) else {
            continue;
        };
        let pred_key = &info.inputs[0];
        // Field tracking is a PROOF problem: it runs only while the operated
        // field set is provable from predecessor `fields`. Structural step
        // validation (object/type/enum/payload/fields operand) always runs,
        // even when the predecessor exposes no fields.
        let mut current: Option<TrackedNames> = infos
            .get(pred_key)
            .and_then(|i| i.field_names.clone())
            .filter(|n| !n.is_empty())
            .map(TrackedNames::new);
        // names provably removed by a step and not reintroduced — used to
        // flag stale declared fields without requiring full projection
        // equality on real-world wires
        let mut removed: Vec<String> = Vec::new();

        for (si, step) in steps.iter().enumerate() {
            let spath = format!("{}/steps/{}", base, si);
            let Some(step_obj) = step.as_object() else {
                diags.push(err(
                    spath,
                    "structure.step",
                    &src(cite::STEP_IMPL, "96", "DataFlowSelectNodeStepImpl#<init>"),
                    "step must be an object",
                ));
                continue;
            };
            let Some(ty) = step_obj.get("type").and_then(Value::as_str) else {
                diags.push(err(
                    format!("{}/type", spath),
                    "structure.step.type",
                    &src(cite::STEP_IMPL, "97", "DataFlowSelectNodeStepImpl#<init>"),
                    "step.type must be a string",
                ));
                continue;
            };
            if !STEP_TYPES.contains(&ty) {
                diags.push(err(
                    format!("{}/type", spath),
                    "enum.stepType",
                    &src(cite::STEP_IMPL, "98", "DataFlowSelectNodeStepType.valueOf"),
                    format!("unknown step type {:?}", ty),
                ));
                continue;
            }
            if !step_supported(ty) {
                diags.push(warn(
                    format!("{}/type", spath),
                    "coverage.step.unknown",
                    &src(
                        cite::SELECT_NODE,
                        "120",
                        "DwDataFlowNodeContributor_select#doCompileQuery",
                    ),
                    format!(
                        "step type {} has no analyzed branch (missing/TODO); fidelity only",
                        ty
                    ),
                ));
            }

            // `fields` operand: array of non-empty strings, case-insensitive
            // duplicates rejected (QueryImpl#getField uppercases + dup-checks).
            if let Some(fl) = step_obj.get("fields") {
                match fl.as_array() {
                    Some(arr) => {
                        let mut seen: HashSet<String> = HashSet::new();
                        for (fi, fv) in arr.iter().enumerate() {
                            match fv.as_str() {
                                Some(nm) if !nm.is_empty() => {
                                    if !seen.insert(nm.to_uppercase()) {
                                        diags.push(err(
                                            format!("{}/fields/{}", spath, fi),
                                            "structure.step.fields",
                                            &src(cite::QUERY_IMPL, "740", "QueryImpl#getField"),
                                            format!("duplicate field {:?} in step.fields", nm),
                                        ));
                                    }
                                }
                                _ => diags.push(err(
                                    format!("{}/fields/{}", spath, fi),
                                    "structure.step.fields",
                                    &src(
                                        cite::STEP_IMPL,
                                        "96",
                                        "DataFlowSelectNodeStepImpl#<init>",
                                    ),
                                    "step.fields entries must be non-empty strings",
                                )),
                            }
                        }
                    }
                    None => diags.push(err(
                        format!("{}/fields", spath),
                        "structure.step.fields",
                        &src(cite::STEP_IMPL, "96", "DataFlowSelectNodeStepImpl#<init>"),
                        "step.fields must be an array of field names",
                    )),
                }
            }
            // AddField always reads its fields operand — an absent or empty
            // array means the step cannot name its target (the generator
            // synthesizes [addField.name]; the wire must carry it).
            if ty == "AddField" {
                let ok = step_obj
                    .get("fields")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                if !ok {
                    diags.push(err(
                        format!("{}/fields", spath),
                        "structure.step.fields",
                        &src(cite::STEP_IMPL, "96", "DataFlowSelectNodeStepImpl#<init>"),
                        "AddField step requires a non-empty fields operand",
                    ));
                }
            } else if step_obj
                .get("fields")
                .and_then(Value::as_array)
                .map_or(true, |a| a.is_empty())
            {
                // every other step runs once per operand: none = a no-op
                diags.push(warn(
                    format!("{}/fields", spath),
                    "step.fields.empty",
                    &src(cite::SELECT_NODE, "150", "DwDataFlowNodeContributor_select#doCompileQuery"),
                    format!("{} step has no fields operand; the platform skips it", ty),
                ));
            }
            // closed payload member keys (warn — wire may carry loader-tolerated extras)
            if let Some(pname) = step_payload_name(ty) {
                if pname != "filter" {
                    if let Some(pk) = step_payload_keys(pname) {
                        if let Some(pobj) = step_obj.get(pname).and_then(Value::as_object) {
                            for k in pobj.keys() {
                                if !pk.contains(&k.as_str()) {
                                    diags.push(warn(
                                        format!("{}/{}/{}", spath, pname, ptr_escape(k)),
                                        "step.payload.key",
                                        &src(
                                            cite::STEP_IMPL,
                                            "96",
                                            "DataFlowSelectNodeStepImpl#<init>",
                                        ),
                                        format!("unexpected {} payload key {:?}", pname, k),
                                    ));
                                }
                            }
                        }
                    }
                } else if step_obj
                    .get("filter")
                    .and_then(Value::as_str)
                    .map(|x| x.is_empty())
                    .unwrap_or(true)
                {
                    diags.push(warn(
                        format!("{}/filter", spath),
                        "step.filter.empty",
                        &src(
                            cite::SELECT_NODE,
                            "284",
                            "DwDataFlowNodeContributor_select#checkStep",
                        ),
                        "Filter step has no non-empty filter expression",
                    ));
                }
            }

            // enum-valued payload strings: Java Enum.valueOf throws on
            // invalid values (DataFlowSelectNodeStepImpl.java:115-116,152-153,
            // 160-161) -> hard error. Blank/missing -> null/default, allowed.
            let step_enums: &[(&str, &str, &[&str], bool)] = match ty {
                "Trim" => &[(
                    "trim",
                    "trimPosition",
                    &["Front", "End", "FrontAndEnd", "All"],
                    false,
                )],
                "SplitField" => &[
                    ("split", "splitOff", &["First", "Last", "All"], true),
                    ("split", "splitType", &["Column", "Row"], true),
                ],
                "SubString" => &[
                    ("subString", "type", &["Left", "Right", "Mid"], true),
                    ("subString", "boundaryType", &["Index", "Char"], true),
                ],
                _ => &[],
            };
            for (pk, ek, allowed, blank_ok) in step_enums {
                if let Some(v) = step_obj
                    .get(*pk)
                    .and_then(|p| p.get(*ek))
                    .and_then(Value::as_str)
                {
                    if v.is_empty() && *blank_ok {
                        continue;
                    }
                    if !allowed.contains(&v) {
                        diags.push(err(
                            format!("/{}/{}", spath.trim_end_matches('/'), pk),
                            "enum.stepPayload",
                            &src(cite::STEP_IMPL, "115", "Enum.valueOf"),
                            format!(
                                "{}.{} value {:?} not in {:?} (Java valueOf throws)",
                                pk, ek, v, allowed
                            ),
                        ));
                    }
                }
            }
            // DeleteSpechars.spechars is read via getString -> warn if not a
            // string (loader tolerates, semantics degrade)
            if ty == "DeleteSpechars" {
                if let Some(v) = step_obj.get("spechars") {
                    if !v.is_string() {
                        diags.push(warn(
                            format!("{}/spechars", spath),
                            "warn.step.spechars.type",
                            &src(cite::STEP_IMPL, "133", "MapUtils.getString"),
                            "DeleteSpechars.spechars is not a string; getString yields null",
                        ));
                    }
                }
            }

            // ---- field tracking (only while provable) ----
            let Some(cur) = current.as_mut() else {
                continue;
            };
            let step_fields: Vec<String> = step_obj
                .get("fields")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
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
                && step_obj
                    .get("text2id")
                    .and_then(|t| t.get("srcField"))
                    .and_then(Value::as_str)
                    .map(|x| x.is_empty())
                    .unwrap_or(true));
            if operated {
                for (fi, name) in step_fields.iter().enumerate() {
                    if ty != "RemoveField" && !cur.contains(name) {
                        diags.push(err(
                            format!("{}/fields/{}", spath, fi),
                            "err.dataflow.stepField.notfound",
                            &src(
                                cite::SELECT_NODE,
                                "284",
                                "DwDataFlowNodeContributor_select#checkStep",
                            ),
                            format!("step {} references unknown field {:?}", ty, name),
                        ));
                    }
                }
            }

            // conservative expression check: unqualified [ref]s in
            // filter/addField.exp/transformFieldInfo.exp must resolve to a
            // tracked field or a declared parameter. Qualified refs and
            // unparseable grammar are skipped (warn only — never reject).
            for (ekey, epath) in [
                ("filter", "filter"),
                ("addField", "addField/exp"),
                ("transformFieldInfo", "transformFieldInfo/exp"),
            ] {
                let exp = if ekey == "filter" {
                    step_obj.get("filter").and_then(Value::as_str)
                } else {
                    step_obj
                        .get(ekey)
                        .and_then(|m| m.get("exp"))
                        .and_then(Value::as_str)
                };
                let Some(exp) = exp else { continue };
                for (name, qualified) in exp_bracket_refs(exp) {
                    if qualified || name.is_empty() {
                        continue;
                    }
                    let upper = name.to_uppercase();
                    let known = cur.contains(&name) || param_names.contains(&upper);
                    if !known {
                        diags.push(warn(
                            format!("{}/{}", spath, epath),
                            "exp.ref.notfound",
                            &src(
                                cite::STEP_IMPL,
                                "284",
                                "DwDataFlowNodeContributor_select#checkStep",
                            ),
                            format!(
                                "expression references unknown field/parameter {:?} (unqualified refs checked against tracked fields + parameters; qualified/unknown grammar not validated)",
                                name
                            ),
                        ));
                    }
                }
            }

            // evolve the tracked set through the shared Unicode-aware name
            // semantics; `removed` remembers names a step took away so a
            // stale declared field can be flagged without requiring full
            // projection equality (opaque shapes drop all tracking).
            let removed_operands: Vec<&str> = if ty == "RemoveField" {
                step_fields
                    .iter()
                    .filter(|name| cur.contains(name))
                    .map(String::as_str)
                    .collect()
            } else {
                Vec::new()
            };
            let rename_old = if ty == "Refactor" {
                step_fields
                    .first()
                    .filter(|sel| cur.contains(sel))
                    .and_then(|sel| cur.ordered.iter().find(|c| field_eq(c, sel)))
                    .cloned()
            } else {
                None
            };
            match cur.evolve(step, &step_fields) {
                Ok(true) => match ty {
                    "RemoveField" => {
                        for name in removed_operands {
                            removed.push(name.to_string());
                        }
                    }
                    "Refactor" => {
                        if let Some(rn) = step_obj
                            .get("refactor")
                            .and_then(|r| r.get("rename"))
                            .and_then(Value::as_str)
                        {
                            if !rn.is_empty() {
                                if let Some(old) = rename_old {
                                    // case-only self rename keeps the
                                    // name alive; a real rename removes
                                    // the old name and reintroduces rn
                                    if !field_eq(&old, rn) {
                                        removed.push(old);
                                    }
                                    removed.retain(|r| !field_eq(r, rn));
                                }
                            }
                        }
                    }
                    "AddField" => {
                        if let Some(nm) = step_obj
                            .get("addField")
                            .and_then(|a| a.get("name"))
                            .and_then(Value::as_str)
                        {
                            if !nm.is_empty() {
                                removed.retain(|r| !field_eq(r, nm));
                            }
                        }
                    }
                    _ => {}
                },
                Ok(false) => {
                    current = None;
                    removed.clear();
                }
                Err("addField/name") => {
                    // Java's addField skips a name already in the query, so
                    // the step is a no-op and the earlier field survives —
                    // names are unchanged, exactly as evolve_names left them
                    diags.push(warn(
                        format!("{}/addField/name", spath),
                        "field.duplicate",
                        &src(
                            cite::QUERY_HELPER,
                            "234",
                            "DwDataFlowQueryHelper#addField",
                        ),
                        "step AddField names an existing field; the platform skips it and keeps the existing field",
                    ));
                }
                Err(tag) => {
                    diags.push(err(
                        format!("{}/{}", spath, tag),
                        "field.duplicate",
                        &src(cite::QUERY_IMPL, "740", "QueryImpl#getField"),
                        format!("step {} creates a duplicate field name", ty),
                    ));
                    current = None;
                    removed.clear();
                }
            }
        }

        // a declared live, ordinary field that a step provably removed and
        // nothing reintroduced is stale metadata — flag it. Fields the
        // loader skips (hidden/invalid) or computed fields (exp) are exempt.
        // Declared names absent from the provable set are tolerated (steps
        // can produce shapes the projection does not model); names PRESENT
        // in the set may not be declared more times than the steps produce.
        if let Some(tracked) = current {
            let mut budget = tracked.counts;
            if let Some(farr) = node.get("fields").and_then(Value::as_array) {
                for (fi, f) in farr.iter().enumerate() {
                    if f.get("hidden").and_then(Value::as_bool) == Some(true) {
                        continue;
                    }
                    if skipped_field(f) {
                        continue;
                    }
                    if f.get("exp")
                        .and_then(Value::as_str)
                        .map(|e| !e.is_empty())
                        .unwrap_or(false)
                    {
                        continue;
                    }
                    let Some(nm) = f.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    if removed.iter().any(|r| field_eq(r, nm)) {
                        diags.push(err(
                            format!("{}/fields/{}/name", base, fi),
                            "field.mismatch",
                            &src(cite::QUERY_IMPL, "740", "QueryImpl#getField"),
                            format!(
                                "field {:?} was removed by a step and never reintroduced",
                                nm
                            ),
                        ));
                        continue;
                    }
                    let key = nm.to_uppercase();
                    if let Some(left) = budget.get_mut(&key) {
                        if *left == 0 {
                            diags.push(err(
                                format!("{}/fields/{}/name", base, fi),
                                "field.mismatch",
                                &src(cite::QUERY_IMPL, "740", "QueryImpl#getField"),
                                format!(
                                    "field {:?} is declared more times than the steps produce",
                                    nm
                                ),
                            ));
                        } else {
                            *left -= 1;
                        }
                    }
                }
            }
        }
    }
}

fn check_top_level(
    diags: &mut Vec<Diag>,
    tbl: &Value,
    nodes_obj: &serde_json::Map<String, Value>,
    infos: &HashMap<String, NodeInfo>,
) {
    let props = tbl.get("properties");
    crate::gen::run_mode_warnings(
        props.and_then(|p| p.get("extractDataEnabled")),
        props.and_then(|p| p.get("dbTableName")),
        "/properties",
        diags,
    );
    // parameters
    if let Some(ps) = tbl.get("parameters") {
        match ps.as_array() {
            Some(arr) => {
                for (i, p) in arr.iter().enumerate() {
                    let ppath = format!("/parameters/{}", i);
                    if !p.is_object() {
                        diags.push(warn(
                            ppath,
                            "parameter.shape",
                            &src(cite::LOADER, "1165", "DwTableLoader#loadParameters"),
                            "parameter must be an object (loader skips non-objects)",
                        ));
                        continue;
                    }
                    // DwTableLoader: blank names are skipped and dataType
                    // defaults to VARCHAR — tolerated, never fatal.
                    for k in ["name", "dataType"] {
                        if p.get(k)
                            .and_then(Value::as_str)
                            .map(|s| s.is_empty())
                            .unwrap_or(true)
                        {
                            diags.push(warn(
                                format!("{}/{}", ppath, k),
                                "parameter.field",
                                &src(cite::LOADER, "1172", "DwTableLoader#loadParameters"),
                                format!(
                                    "parameter.{} missing/blank — loader skips or defaults VARCHAR",
                                    k
                                ),
                            ));
                        }
                    }
                }
            }
            None => diags.push(warn(
                "/parameters".into(),
                "parameter.shape",
                &src(cite::LOADER, "1165", "DwTableLoader#loadParameters"),
                "parameters is not an array — loader treats as empty",
            )),
        }
    }
    // dimensions / measures: DwTableLoader#toList returns EMPTY for any
    // non-List (incl. null) — tolerated on the wire, never fatal.
    for k in ["dimensions", "measures"] {
        if let Some(v) = tbl.get(k) {
            if !v.is_array() && !v.is_null() {
                diags.push(warn(
                    format!("/{}", k),
                    "structure.fields",
                    &src(cite::LOADER, "1406", "DwTableLoader#toList"),
                    format!("{} is not an array — loader treats as empty", k),
                ));
            }
        }
    }
    // subProcesses shape
    if let Some(sp) = tbl.pointer("/dataFlow/subProcesses") {
        if let Some(obj) = sp.as_object() {
            for (id, item) in obj {
                let ipath = format!("/dataFlow/subProcesses/{}", ptr_escape(id));
                if !item.is_object() {
                    diags.push(err(
                        ipath,
                        "structure.subprocess",
                        &src(cite::LOADER, "1267", "DwTableLoader#loadDataFlow"),
                        "subProcesses entries must be objects",
                    ));
                    continue;
                }
                if item
                    .get("rootNodeId")
                    .and_then(Value::as_str)
                    .map(|s| s.is_empty())
                    .unwrap_or(true)
                {
                    diags.push(err(
                        format!("{}/rootNodeId", ipath),
                        "structure.subprocess",
                        &src(cite::LOADER, "1267", "DwTableLoader#loadDataFlow"),
                        "subProcesses.rootNodeId must be a non-empty string",
                    ));
                }
                if !item.get("nodeIds").map(Value::is_array).unwrap_or(false) {
                    diags.push(err(
                        format!("{}/nodeIds", ipath),
                        "structure.subprocess",
                        &src(cite::LOADER, "1267", "DwTableLoader#loadDataFlow"),
                        "subProcesses.nodeIds must be an array",
                    ));
                }
            }
        } else {
            diags.push(err(
                "/dataFlow/subProcesses".into(),
                "structure.subprocess",
                &src(cite::LOADER, "1267", "DwTableLoader#loadDataFlow"),
                "dataFlow.subProcesses must be an object",
            ));
        }
    }
    // the runner resolves its output as nodes["default"] and fails without it
    if !nodes_obj.is_empty() && !nodes_obj.contains_key("default") {
        diags.push(err(
            "/dataFlow/nodes".into(),
            "flow.noDefaultOutput",
            &src(cite::DATAFLOW_CTX, "173", "DwDataFlowContext#initNodes"),
            "dataFlow has no node with id \"default\"; running the model fails with err.dataflow.nooutput",
        ));
    } else if nodes_obj.get("default").and_then(|n| n.get("type")).and_then(Value::as_str)
        .is_some_and(|t| t != "Output")
    {
        diags.push(warn(
            "/dataFlow/nodes/default/type".into(),
            "flow.defaultNotOutput",
            &src(cite::LOADER, "399", "DwTableLoader#getOutputNode"),
            "node \"default\" is not an Output node; the loader takes it as the model output",
        ));
    }
    output::check_output_partition(diags, tbl, nodes_obj);
    output::check_extract_dbfields(diags, tbl);
    let _ = infos;
}

/// Cross-file checks for a `.tbl` inside a project layout: ModelTable
/// references and Script node scripts must exist. A reference into a project
/// that is not on disk cannot be verified and is only a warning.
pub fn check_references(tbl: &Value, at: &crate::project::Located) -> Vec<Diag> {
    use crate::project::{project_name, resolve_reference, script_candidates, to_disk};
    let mut diags = Vec::new();
    let Some(nodes) = tbl.pointer("/dataFlow/nodes").and_then(Value::as_object) else {
        return diags;
    };
    let project_on_disk = |p: &str| at.projects_root.join(project_name(p)).is_dir();
    for (id, node) in nodes {
        let base = format!("/dataFlow/nodes/{}", ptr_escape(id));
        match node.get("type").and_then(Value::as_str) {
            Some("ModelTable") => {
                let Some(r) = node.get("moduleTablePath").and_then(Value::as_str) else {
                    continue;
                };
                let Some(target) = resolve_reference(&at.platform_path, r) else {
                    continue;
                };
                if to_disk(&at.projects_root, &target).is_file() {
                    continue;
                }
                let path = format!("{}/moduleTablePath", base);
                let source = src(cite::DWTABLE_NODE, "50", "DwDataFlowNodeContributor_dwtable#initNode");
                if project_on_disk(&target) {
                    diags.push(err(
                        path,
                        "ref.modelTable.missing",
                        &source,
                        format!("{:?} resolves to {} which does not exist (err.dataflow.dwtable.notfound)", r, target),
                    ));
                } else {
                    diags.push(warn(
                        path,
                        "ref.modelTable.unverifiable",
                        &source,
                        format!("{:?} resolves to {} in a project that is not on disk", r, target),
                    ));
                }
            }
            Some("Script") => {
                let Some(name) = node.get("scriptName").and_then(Value::as_str).filter(|s| !s.is_empty())
                else {
                    continue;
                };
                let project = project_name(&at.platform_path);
                let cands = script_candidates(&at.projects_root, project, name);
                // the runner takes field names from the script (onProcessFields);
                // a node that names a dbfield differently is out of sync
                let source = cands.iter().rev().find_map(|p| fs::read_to_string(p).ok());
                if let Some(text) = &source {
                    let declared = crate::script::declared_fields(text);
                    for (i, f) in node.get("fields").and_then(Value::as_array).into_iter().flatten().enumerate() {
                        let (Some(db), Some(fname)) = (
                            f.get("dbfield").and_then(Value::as_str),
                            f.get("name").and_then(Value::as_str),
                        ) else {
                            continue;
                        };
                        if let Some(sname) = declared.get(&db.to_uppercase()) {
                            if sname != fname {
                                diags.push(err(
                                    format!("{}/fields/{}/name", base, i),
                                    "ref.script.fieldName",
                                    &src(cite::SCRIPT_NODE, "448", "DwDataFlowNodeContributor_script#onProcessFields"),
                                    format!(
                                        "field {:?} (dbfield {}) is named {:?} in script {}; the script's name is used at run time",
                                        fname, db, sname, name
                                    ),
                                ));
                            }
                        }
                    }
                }
                if source.is_none() {
                    diags.push(err(
                        format!("{}/scriptName", base),
                        "ref.script.missing",
                        &src(cite::SCRIPT_NODE, "205", "DwDataFlowNodeContributor_script#getScript"),
                        format!("script /{}/data/script/{}.action(.ts) does not exist", project, name),
                    ));
                }
            }
            _ => {}
        }
    }
    diags
}
