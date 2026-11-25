//! `schema` — JSON Schema 2020-12 describing spec format v1, plus worked
//! examples. The schema is generated from the same closed registries that
//! drive `validate_spec`, so it describes the ACTUAL accepted shape: closed
//! field keys, per-type closed config, closed step payloads. The only
//! permissive (`true`) positions are the designated `raw`/`rawNode`/
//! `rawStep` escape hatches.

use serde_json::{json, Map, Value};

use crate::model::*;

// ---------------------------------------------------------------------------
// Schema helpers
// ---------------------------------------------------------------------------

/// Object schema with `additionalProperties: false` (closed by default).
fn obj(properties: Value, required: &[&str]) -> Value {
    let mut m = Map::new();
    m.insert("type".into(), json!("object"));
    m.insert("properties".into(), properties);
    m.insert("additionalProperties".into(), json!(false));
    if !required.is_empty() {
        m.insert(
            "required".into(),
            Value::Array(required.iter().map(|s| json!(s)).collect()),
        );
    }
    Value::Object(m)
}

fn props(list: &[(&str, Value)]) -> Value {
    let mut m = Map::new();
    for (k, v) in list {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

fn str_s() -> Value {
    json!({"type": "string"})
}
fn num_s() -> Value {
    json!({"type": "number"})
}
fn int_s() -> Value {
    json!({"type": "integer"})
}
fn bool_s() -> Value {
    json!({"type": "boolean"})
}
/// Any scalar (string/number/boolean/null) — used where the loader accepts a
/// scalar of unspecified kind. Never a bare `true`.
fn scalar_any() -> Value {
    json!({"type": ["string", "number", "boolean", "null"]})
}
fn strarr() -> Value {
    json!({"type": "array", "items": str_s()})
}
fn any_obj() -> Value {
    json!({"type": "object"})
}

fn field_ref_schema() -> Value {
    json!({"$ref": "#/$defs/field"})
}

// ---------------------------------------------------------------------------
// $defs
// ---------------------------------------------------------------------------

/// Field object: the 16 proved scalar keys, closed.
fn field_def() -> Value {
    let mut fp = Map::new();
    for k in FIELD_KEYS {
        let v = match *k {
            "isDimension" | "extractable" | "hidden" => bool_s(),
            "length" | "decimal" => num_s(),
            _ => str_s(),
        };
        fp.insert((*k).to_string(), v);
    }
    obj(Value::Object(fp), &["name", "dataType", "isDimension"])
}

/// ColToRow valuesArray cell that is a field reference (not a constant).
fn col_to_row_cell_def() -> Value {
    let mut fp = Map::new();
    fp.insert("name".into(), str_s());
    fp.insert("dataType".into(), str_s());
    for k in ["dbfield", "inputField"] {
        fp.insert(k.into(), str_s());
    }
    for k in ["length", "decimal"] {
        fp.insert(k.into(), num_s());
    }
    obj(Value::Object(fp), &["name", "dataType"])
}

/// Closed step-payload object schemas (mirror `validate_step_payload`).
fn step_payload_defs() -> Value {
    let mut d = Map::new();

    // addField = field object + required exp (name/dataType/isDimension are
    // already required by the field def).
    d.insert(
        "addField".into(),
        json!({"allOf": [{"$ref": "#/$defs/field"}, {"required": ["exp"]}]}),
    );

    // split
    d.insert(
        "split".into(),
        obj(
            props(&[
                ("splitString", str_s()),
                ("splitOff", json!({"enum": ["First", "Last", "All"]})),
                ("splitType", json!({"enum": ["Column", "Row"]})),
                ("resultOrderField", str_s()),
                ("splitFieldsCount", int_s()),
                ("childFields", strarr()),
                ("showSplitResultOrder", bool_s()),
            ]),
            &[],
        ),
    );

    // replace
    d.insert(
        "replace".into(),
        obj(
            props(&[("replaceValues", strarr()), ("replace", str_s())]),
            &[],
        ),
    );

    // group / groupNumber (rawStep-only types; kept for completeness)
    d.insert("group".into(), obj(props(&[("type", str_s())]), &[]));
    d.insert(
        "groupNumber".into(),
        obj(
            props(&[
                ("interval", json!({"type": "array", "items": int_s()})),
                ("intervalCaption", strarr()),
            ]),
            &[],
        ),
    );

    // refactor — rename is a STRING (single selected field), not a map
    d.insert(
        "refactor".into(),
        obj(
            props(&[
                ("rename", json!({"type": "string", "minLength": 1})),
                ("desc", str_s()),
                ("dataType", str_s()),
            ]),
            &[],
        ),
    );

    // text2id
    d.insert(
        "text2id".into(),
        obj(
            props(&[
                ("mapTablePath", str_s()),
                ("textField", str_s()),
                ("idField", str_s()),
                ("synonym", str_s()),
                ("sortField", str_s()),
                ("srcField", str_s()),
            ]),
            &[],
        ),
    );

    // trim
    d.insert(
        "trim".into(),
        obj(
            props(&[
                (
                    "trimPosition",
                    json!({"enum": ["Front", "End", "FrontAndEnd", "All"]}),
                ),
                ("trimAllBlank", bool_s()),
            ]),
            &[],
        ),
    );

    // subString
    d.insert(
        "subString".into(),
        obj(
            props(&[
                ("type", json!({"enum": ["Left", "Right", "Mid"]})),
                ("boundaryType", json!({"enum": ["Index", "Char"]})),
                ("count", int_s()),
                ("start", int_s()),
                ("end", int_s()),
                ("headChar", str_s()),
                ("footChar", str_s()),
                ("includeBoundary", bool_s()),
            ]),
            &[],
        ),
    );

    // transformFieldInfo
    d.insert(
        "transformFieldInfo".into(),
        obj(
            props(&[("exp", str_s()), ("dataType", str_s())]),
            &["exp", "dataType"],
        ),
    );

    Value::Object(d)
}

/// Closed config payload schemas (mirror `validate_closed_config`).
fn config_payload_defs() -> Value {
    let mut d = Map::new();

    // joinConditions[]: {leftTable,rightTable,joinType?,exp?,clauses?[...]}
    let clause = obj(
        props(&[
            ("leftExp", str_s()),
            ("rightExp", str_s()),
            ("operator", str_s()),
            ("exp", str_s()),
        ]),
        &["leftExp", "rightExp"],
    );
    let cond = obj(
        props(&[
            ("leftTable", str_s()),
            ("rightTable", str_s()),
            ("joinType", str_s()),
            ("exp", str_s()),
            ("clauses", json!({"type": "array", "items": clause})),
        ]),
        &["leftTable", "rightTable"],
    );
    d.insert(
        "joinConditions".into(),
        json!({"type": "array", "items": cond}),
    );

    // unionMapArray[]: {visible?,values:[string|null,...]}
    d.insert(
        "unionMapArray".into(),
        json!({"type": "array", "items": obj(
            props(&[
                ("visible", bool_s()),
                ("values", json!({"type": "array", "items": json!({"type": ["string", "null"]})})),
            ]),
            &[],
        )}),
    );

    // colToRowInfo: {headers:[string], valuesArray:[[string|null|fieldref]]}
    let cell = json!({"anyOf": [str_s(), json!({"type": "null"}), col_to_row_cell_def()]});
    d.insert(
        "colToRowInfo".into(),
        obj(
            props(&[
                ("headers", strarr()),
                (
                    "valuesArray",
                    json!({"type": "array", "items": json!({"type": "array", "items": cell})}),
                ),
            ]),
            &["headers"],
        ),
    );

    // groupFieldsMap: {string:string}
    d.insert(
        "groupFieldsMap".into(),
        json!({"type": "object", "additionalProperties": str_s()}),
    );

    // distinctSorts[]: {exp,type}
    d.insert(
        "distinctSorts".into(),
        json!({"type": "array", "items": obj(
            props(&[("exp", str_s()), ("type", str_s())]),
            &["exp", "type"],
        )}),
    );

    Value::Object(d)
}

fn build_defs() -> Value {
    let mut defs = Map::new();
    defs.insert("field".into(), field_def());
    let mut payloads = Map::new();
    payloads.insert("step".into(), step_payload_defs());
    payloads.insert("config".into(), config_payload_defs());
    defs.insert("payloads".into(), Value::Object(payloads));
    Value::Object(defs)
}

// ---------------------------------------------------------------------------
// Step schema — one closed object per supported step type + rawStep.
// ---------------------------------------------------------------------------

fn payload_ref(name: &str) -> Value {
    json!({"$ref": format!("#/$defs/payloads/step/{}", name)})
}

fn step_typed_schema(ty: &str) -> Value {
    let mut sp = Map::new();
    sp.insert("type".into(), json!({"const": ty}));

    let mut required: Vec<&str> = vec!["type"];

    // fields operand: Filter additionally requires a non-empty list
    // (Java skips a Filter with no fields even for constant expressions).
    if ty == "Filter" {
        sp.insert(
            "fields".into(),
            json!({"type": "array", "minItems": 1, "items": str_s()}),
        );
        required.push("fields");
    } else {
        sp.insert("fields".into(), json!({"type": "array", "items": str_s()}));
    }

    // DeleteSpechars carries spechars + isRegExp (constructor :96-170).
    if ty == "DeleteSpechars" {
        sp.insert("spechars".into(), str_s());
        sp.insert("isRegExp".into(), bool_s());
    }

    match step_payload_name(ty) {
        Some("filter") => {
            sp.insert("filter".into(), str_s());
            required.push("filter");
        }
        Some(p) => {
            sp.insert(p.into(), payload_ref(p));
            required.push(match p {
                "addField" => "addField",
                "split" => "split",
                "replace" => "replace",
                "refactor" => "refactor",
                "subString" => "subString",
                "trim" => "trim",
                "text2id" => "text2id",
                "transformFieldInfo" => "transformFieldInfo",
                _ => p,
            });
        }
        None => {}
    }

    obj(Value::Object(sp), &required)
}

fn step_schema() -> Value {
    let mut arms: Vec<Value> = SUPPORTED_STEP_TYPES
        .iter()
        .map(|t| step_typed_schema(t))
        .collect();
    arms.push(obj(props(&[("rawStep", any_obj())]), &["rawStep"]));
    json!({"oneOf": arms})
}

// ---------------------------------------------------------------------------
// Config schema — closed per node type.
// ---------------------------------------------------------------------------

fn config_scalar_schema(ty: &str, k: &str) -> Value {
    match config_scalar_kind(ty, k) {
        ScalarKind::Str => str_s(),
        ScalarKind::Num => num_s(),
        ScalarKind::Bool => bool_s(),
        ScalarKind::AnyScalar => scalar_any(),
    }
}

fn config_schema(ty: &str) -> Value {
    let mut cp = Map::new();
    // common keys
    cp.insert("comment".into(), str_s());
    cp.insert("devQueryFixedRows".into(), bool_s());
    cp.insert("devFixedRowCount".into(), num_s());
    // per-type scalar keys
    for k in config_scalar_keys(ty) {
        if *k == "unionType" {
            cp.insert((*k).into(), json!({"enum": UNION_TYPES}));
        } else {
            cp.insert((*k).into(), config_scalar_schema(ty, k));
        }
    }
    // string-array keys
    for k in config_strarray_keys(ty) {
        cp.insert((*k).into(), strarr());
    }
    // closed payload keys
    for (k, _) in config_closed_keys(ty) {
        cp.insert(
            (*k).into(),
            json!({"$ref": format!("#/$defs/payloads/config/{}", k)}),
        );
    }
    obj(Value::Object(cp), &[])
}

// ---------------------------------------------------------------------------
// Node schema — one closed object per supported node type + rawNode.
// ---------------------------------------------------------------------------

fn node_typed_schema(ty: &str) -> Value {
    let mut np = Map::new();
    np.insert("key".into(), str_s());
    np.insert("type".into(), json!({"const": ty}));
    np.insert("alias".into(), str_s());
    np.insert("id".into(), str_s());
    np.insert("inputs".into(), json!({"type": "array", "items": str_s()}));
    np.insert(
        "fields".into(),
        json!({"type": "array", "items": field_ref_schema()}),
    );
    np.insert(
        "originalFields".into(),
        json!({"type": "array", "items": field_ref_schema()}),
    );
    np.insert("config".into(), config_schema(ty));
    if ty == "Select" {
        np.insert(
            "steps".into(),
            json!({"type": "array", "items": step_schema()}),
        );
    }
    np.insert("raw".into(), any_obj());
    obj(Value::Object(np), &["type"])
}

fn node_schema() -> Value {
    let rawnode = obj(
        props(&[("key", str_s()), ("rawNode", any_obj())]),
        &["key", "rawNode"],
    );
    let mut arms = vec![rawnode];
    for ty in SUPPORTED_NODE_TYPES {
        arms.push(node_typed_schema(ty));
    }
    json!({"oneOf": arms})
}

// ---------------------------------------------------------------------------
// Top-level forms
// ---------------------------------------------------------------------------

fn generated_form_schema() -> Value {
    let mut gp = Map::new();
    gp.insert("format".into(), json!({"const": 1}));
    gp.insert("version".into(), str_s());
    let mut pp = Map::new();
    pp.insert("modelDataType".into(), json!({"const": "DataFlow"}));
    pp.insert("datasource".into(), str_s());
    pp.insert("dbSchema".into(), str_s());
    pp.insert("dbTableName".into(), str_s());
    pp.insert("extractDataEnabled".into(), bool_s());
    pp.insert("raw".into(), any_obj());
    gp.insert("properties".into(), obj(Value::Object(pp), &[]));
    let param = obj(
        props(&[
            ("name", str_s()),
            ("dataType", str_s()),
            ("defaultValue", scalar_any()),
            ("raw", any_obj()),
        ]),
        &["name", "dataType"],
    );
    gp.insert(
        "parameters".into(),
        json!({"type": "array", "items": param}),
    );
    let sub = obj(
        props(&[
            ("id", str_s()),
            ("caption", str_s()),
            ("rootNodeId", str_s()),
            ("nodeIds", json!({"type": "array", "items": str_s()})),
            ("raw", any_obj()),
        ]),
        &["rootNodeId", "nodeIds"],
    );
    gp.insert(
        "subProcesses".into(),
        json!({"type": "object", "additionalProperties": sub}),
    );
    gp.insert(
        "nodes".into(),
        json!({"type": "array", "items": node_schema()}),
    );
    gp.insert("raw".into(), any_obj());
    obj(Value::Object(gp), &["format", "nodes"])
}

fn archive_form_schema() -> Value {
    let node_entry = obj(
        props(&[("id", str_s()), ("body", any_obj())]),
        &["id", "body"],
    );
    let archive = obj(
        props(&[
            ("top", any_obj()),
            ("flow", any_obj()),
            ("nodes", json!({"type": "array", "items": node_entry})),
        ]),
        &["top", "flow", "nodes"],
    );
    obj(
        props(&[("format", json!({"const": 1})), ("archive", archive)]),
        &["format", "archive"],
    )
}

fn rawtable_form_schema() -> Value {
    obj(
        props(&[("format", json!({"const": 1})), ("rawTable", any_obj())]),
        &["format", "rawTable"],
    )
}

/// The document returned by `tblgen schema`: `{schema, examples}`.
/// `schema` is a valid JSON Schema 2020-12 document; `examples` carries the
/// typed/rawOnly samples plus per-enum nodeExamples/stepExamples, each a
/// complete spec.
pub fn schema_document() -> Value {
    json!({
        "schema": {
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "tblgen spec format v1",
            "description": "Closed typed spec for .tbl DataFlow models. Unknown node/step types and unrecognized payloads must use rawNode/rawStep/raw; archive and rawTable are verbatim snapshot forms. Config keys are per-node-type closed; the only permissive positions are the explicit raw escape hatches.",
            "oneOf": [
                generated_form_schema(),
                archive_form_schema(),
                rawtable_form_schema(),
            ],
            "$defs": build_defs(),
        },
        "examples": examples(),
    })
}

// ---------------------------------------------------------------------------
// Examples — every entry is a COMPLETE valid spec (format:1 envelope).
// ---------------------------------------------------------------------------

fn fld(n: &str, dt: &str, dim: bool) -> Value {
    json!({"name": n, "dataType": dt, "isDimension": dim})
}

/// Shared FileInput source fields: property(C,dim) + m01/m02(N,meas).
fn src_fields() -> Vec<Value> {
    vec![
        fld("property", "C", true),
        fld("m01", "N", false),
        fld("m02", "N", false),
    ]
}

fn fileinput(key: &str) -> Value {
    json!({
        "key": key, "type": "FileInput", "alias": key,
        "config": {"fileName": "data.xlsx#Sheet"},
        "fields": src_fields(),
    })
}

fn output_node(key: &str, inputs: &[&str]) -> Value {
    json!({"key": key, "type": "Output", "alias": key, "inputs": inputs})
}

fn select_node(key: &str, input: &str, steps: Value, fields: Option<Vec<Value>>) -> Value {
    let mut o = json!({"key": key, "type": "Select", "alias": key, "inputs": [input]});
    if let Some(m) = o.as_object_mut() {
        m.insert("steps".into(), steps);
        if let Some(f) = fields {
            m.insert("fields".into(), Value::Array(f));
        }
    }
    o
}

fn wrap(nodes: Vec<Value>) -> Value {
    json!({
        "format": 1,
        "properties": {"modelDataType": "DataFlow"},
        "nodes": nodes,
    })
}

fn wrap_sub(nodes: Vec<Value>, sub: Value) -> Value {
    let mut w = wrap(nodes);
    w.as_object_mut()
        .unwrap()
        .insert("subProcesses".into(), sub);
    w
}

/// Explicit fields for steps/nodes whose shape cannot be derived.
fn xfields() -> Vec<Value> {
    vec![fld("x_dim", "C", true), fld("x_meas", "N", false)]
}

fn step_spec(step: Value, explicit_fields: bool) -> Value {
    let fields = if explicit_fields {
        Some(xfields())
    } else {
        None
    };
    wrap(vec![
        fileinput("src"),
        select_node("sel", "src", json!([step]), fields),
        output_node("out", &["sel"]),
    ])
}

fn node_examples() -> Vec<Value> {
    let mut v = Vec::new();
    let mut add = |label: &str, ty: &str, spec: Value| {
        v.push(json!({"label": label, "type": ty, "spec": spec}));
    };

    // ---- typed node examples (17 supported) ----
    add(
        "typed",
        "FileInput",
        wrap(vec![fileinput("src"), output_node("out", &["src"])]),
    );
    add(
        "typed",
        "ModelTable",
        wrap(vec![
            json!({
                "key": "mt", "type": "ModelTable", "alias": "mt",
                "config": {"moduleTablePath": "../ods/ods_demo.tbl"},
                "fields": [fld("a", "C", true), fld("b", "N", false)],
            }),
            output_node("out", &["mt"]),
        ]),
    );
    add(
        "typed",
        "Select",
        wrap(vec![
            fileinput("src"),
            select_node(
                "sel",
                "src",
                json!([{"type": "Filter", "fields": ["property"], "filter": "[property] != NULL"}]),
                None,
            ),
            output_node("out", &["sel"]),
        ]),
    );
    // Join: two aliased FileInputs + one joinConditions clause matching aliases
    add(
        "typed",
        "Join",
        wrap(vec![
            json!({
                "key": "l", "type": "FileInput", "alias": "left",
                "config": {"fileName": "l.xlsx#S"},
                "fields": [fld("k", "C", true), fld("v", "N", false)],
            }),
            json!({
                "key": "r", "type": "FileInput", "alias": "right",
                "config": {"fileName": "r.xlsx#S"},
                "fields": [fld("k", "C", true), fld("w", "N", false)],
            }),
            json!({
                "key": "j", "type": "Join", "alias": "j", "inputs": ["l", "r"],
                "config": {"joinConditions": [{
                    "leftTable": "left", "rightTable": "right",
                    "joinType": "INNER JOIN",
                    "clauses": [{
                        "leftExp": "[left].[k]", "rightExp": "[right].[k]",
                        "operator": "=", "exp": "[left].[k] = [right].[k]",
                    }],
                }]},
                "fields": [fld("k", "C", true), fld("v", "N", false), fld("w", "N", false)],
            }),
            output_node("out", &["j"]),
        ]),
    );
    // Union: two inputs, unionType + unionMapArray closed payload
    add(
        "typed",
        "Union",
        wrap(vec![
            fileinput("a"),
            fileinput("b"),
            json!({
                "key": "u", "type": "Union", "alias": "u", "inputs": ["a", "b"],
                "config": {
                    "unionType": "UnionAll",
                    "unionMapArray": [{"visible": true, "values": ["property", "m01"]}],
                },
                "fields": [fld("property", "C", true), fld("m01", "N", false)],
            }),
            output_node("out", &["u"]),
        ]),
    );
    // Agg: groupFields closed payload + explicit result fields
    add(
        "typed",
        "Agg",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "agg", "type": "Agg", "alias": "agg", "inputs": ["src"],
                "config": {
                    "groupFields": ["property"],
                    "groupFieldsMap": {"property": "property"},
                },
                "fields": [fld("property", "C", true), fld("total", "N", false)],
            }),
            output_node("out", &["agg"]),
        ]),
    );
    // Distinct (node): passthrough fields
    add(
        "typed",
        "Distinct",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "d", "type": "Distinct", "alias": "d", "inputs": ["src"],
                "fields": src_fields(),
            }),
            output_node("out", &["d"]),
        ]),
    );
    // DbTable: Script predecessor + explicit fields
    add(
        "typed",
        "DbTable",
        wrap(vec![
            json!({
                "key": "sc", "type": "Script", "alias": "sc",
                "config": {"scriptES5": "// load sql"},
                "fields": [fld("f1", "C", true)],
            }),
            json!({
                "key": "db", "type": "DbTable", "alias": "db", "inputs": ["sc"],
                "config": {"datasource": "defdw", "dbSchema": "ods", "dbTableName": "ods_t"},
                "fields": [fld("f1", "C", true)],
            }),
            output_node("out", &["db"]),
        ]),
    );
    // Sql: standalone query node with explicit fields
    add(
        "typed",
        "Sql",
        wrap(vec![
            json!({
                "key": "sq", "type": "Sql", "alias": "sq",
                "config": {"sql": "select 1 as f1"},
                "fields": [fld("f1", "C", true)],
            }),
            output_node("out", &["sq"]),
        ]),
    );
    // Output: minimal FileInput -> Output
    add(
        "typed",
        "Output",
        wrap(vec![fileinput("src"), output_node("out", &["src"])]),
    );
    // DbOutput: terminal writer
    add(
        "typed",
        "DbOutput",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "dbo", "type": "DbOutput", "alias": "dbo", "inputs": ["src"],
                "config": {"datasource": "defdw", "dbSchema": "ads", "dbTableName": "ads_demo"},
                "fields": src_fields(),
            }),
        ]),
    );
    // RowToCol: rowToColInfo is NOT a closed typed config -> explicit raw
    add(
        "typed",
        "RowToCol",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "r2c", "type": "RowToCol", "alias": "r2c", "inputs": ["src"],
                "raw": {"rowToColInfo": {
                    "caseFields": ["property"],
                    "valueFields": ["m01", "m02"],
                    "retainFields": [],
                    "transformArray": [["k", "v", "t"]],
                }},
                "fields": [fld("property", "C", true), fld("val", "N", false)],
            }),
            output_node("out", &["r2c"]),
        ]),
    );
    // ColToRow: closed colToRowInfo config; pivot refs exist in predecessor;
    // property identity retained via inputField.
    add(
        "typed",
        "ColToRow",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "c2r", "type": "ColToRow", "alias": "c2r", "inputs": ["src"],
                "config": {"colToRowInfo": {
                    "headers": ["month", "amount"],
                    "valuesArray": [
                        ["01", {"name": "m01", "dataType": "N"}],
                        ["02", {"name": "m02", "dataType": "N"}],
                    ],
                }},
                "fields": [
                    fld("month", "C", true),
                    fld("amount", "N", false),
                    json!({"name": "property", "dataType": "C", "isDimension": true, "inputField": "property"}),
                ],
            }),
            output_node("out", &["c2r"]),
        ]),
    );
    // Snapshot: period fields + explicit fields
    add(
        "typed",
        "Snapshot",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "snap", "type": "Snapshot", "alias": "snap", "inputs": ["src"],
                "config": {"periodStartField": "property", "periodEndField": "m01"},
                "fields": src_fields(),
            }),
            output_node("out", &["snap"]),
        ]),
    );
    // Script: external script source, explicit fields
    add(
        "typed",
        "Script",
        wrap(vec![
            json!({
                "key": "sc", "type": "Script", "alias": "sc",
                "config": {"scriptES5": "// generated script"},
                "fields": [fld("r", "C", true)],
            }),
            output_node("out", &["sc"]),
        ]),
    );
    // Update: single input (no updateConditions needed)
    add(
        "typed",
        "Update",
        wrap(vec![
            fileinput("src"),
            json!({
                "key": "upd", "type": "Update", "alias": "upd", "inputs": ["src"],
                "fields": src_fields(),
            }),
            output_node("out", &["upd"]),
        ]),
    );
    // SubProcess: real root inside subProcesses + main-flow consumer
    add(
        "typed",
        "SubProcess",
        wrap_sub(
            vec![
                fileinput("fin"),
                output_node("fout", &["fin"]),
                json!({
                    "key": "sub", "type": "SubProcess", "alias": "sub",
                    "config": {"subProcessId": "sp1"},
                    "fields": src_fields(),
                }),
                output_node("out", &["sub"]),
            ],
            json!({"sp1": {"rootNodeId": "fout", "nodeIds": ["fin", "fout"]}}),
        ),
    );

    // ---- rawOnly node examples (3 unsupported types) ----
    for ty in ["SlowChanging", "GeoFindLocation", "Extend"] {
        add(
            "rawOnly",
            ty,
            wrap(vec![
                fileinput("src"),
                json!({
                    "key": "x",
                    "rawNode": {
                        "type": ty, "id": "x", "alias": "x",
                        "inputNodes": ["src"],
                        "fields": src_fields(),
                    },
                }),
                // rawNode shape is opaque to the deriver: Output needs
                // explicit fields here.
                json!({"key": "out", "type": "Output", "alias": "out",
                       "inputs": ["x"], "fields": src_fields()}),
            ]),
        );
    }

    v
}

fn step_examples() -> Vec<Value> {
    let mut v = Vec::new();
    let mut add = |label: &str, ty: &str, spec: Value| {
        v.push(json!({"label": label, "type": ty, "spec": spec}));
    };

    // ---- typed step examples (18 supported) ----
    add(
        "typed",
        "Filter",
        step_spec(
            json!({"type": "Filter", "fields": ["property"], "filter": "[property] != NULL"}),
            false,
        ),
    );
    add("typed", "AddField", {
        // exp references a declared parameter (上传文件年月)
        let mut sp = step_spec(
            json!({"type": "AddField", "addField": {"name": "year", "dataType": "C", "isDimension": true, "exp": "[上传文件年月]"}}),
            false,
        );
        sp["parameters"] = json!([{"name": "上传文件年月", "dataType": "C"}]);
        sp
    });
    add(
        "typed",
        "RemoveField",
        step_spec(json!({"type": "RemoveField", "fields": ["m02"]}), false),
    );
    add(
        "typed",
        "SplitField",
        step_spec(
            json!({"type": "SplitField", "fields": ["property"], "split": {"splitString": ",", "splitType": "Column"}}),
            true,
        ),
    );
    add(
        "typed",
        "Refactor",
        step_spec(
            json!({"type": "Refactor", "fields": ["m01"], "refactor": {"rename": "amount"}}),
            false,
        ),
    );
    add(
        "typed",
        "SubString",
        step_spec(
            json!({"type": "SubString", "fields": ["property"], "subString": {"start": 1, "end": 3}}),
            false,
        ),
    );
    add(
        "typed",
        "Replace",
        step_spec(
            json!({"type": "Replace", "fields": ["property"], "replace": {"replace": "x", "replaceValues": ["a", "b"]}}),
            false,
        ),
    );
    add(
        "typed",
        "ToUppercase",
        step_spec(
            json!({"type": "ToUppercase", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "ToLowercase",
        step_spec(
            json!({"type": "ToLowercase", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "Trim",
        step_spec(
            json!({"type": "Trim", "fields": ["property"], "trim": {"trimPosition": "FrontAndEnd"}}),
            false,
        ),
    );
    add(
        "typed",
        "DeleteNumbers",
        step_spec(
            json!({"type": "DeleteNumbers", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "DeleteLetters",
        step_spec(
            json!({"type": "DeleteLetters", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "DeleteSpechars",
        step_spec(
            json!({"type": "DeleteSpechars", "fields": ["property"], "spechars": "-_"}),
            false,
        ),
    );
    add(
        "typed",
        "DeletePunctuations",
        step_spec(
            json!({"type": "DeletePunctuations", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "ExcludeEmpty",
        step_spec(
            json!({"type": "ExcludeEmpty", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "ExtractNumber",
        step_spec(
            json!({"type": "ExtractNumber", "fields": ["property"]}),
            false,
        ),
    );
    add(
        "typed",
        "Text2Id",
        step_spec(
            json!({"type": "Text2Id", "fields": ["property"], "text2id": {"mapTablePath": "maps/x.tbl", "textField": "property", "idField": "property_id"}}),
            true,
        ),
    );
    add(
        "typed",
        "TransformToCalculatedField",
        step_spec(
            json!({"type": "TransformToCalculatedField", "fields": ["m01"], "transformFieldInfo": {"exp": "[m01] * 2", "dataType": "N"}}),
            true,
        ),
    );

    // ---- rawOnly step examples (6 unsupported) ----
    for (ty, body) in [
        (
            "Distinct",
            json!({"type": "Distinct", "fields": ["property"]}),
        ),
        (
            "ReGroup",
            json!({"type": "ReGroup", "fields": ["property"], "group": {}}),
        ),
        (
            "GroupNumber",
            json!({"type": "GroupNumber", "fields": ["m01"], "groupNumber": {"interval": [1, 10, 100]}}),
        ),
        ("ToNumber", json!({"type": "ToNumber", "fields": ["m01"]})),
        ("ToDate", json!({"type": "ToDate", "fields": ["property"]})),
        (
            "ToSynonym",
            json!({"type": "ToSynonym", "fields": ["property"]}),
        ),
    ] {
        add("rawOnly", ty, step_spec(json!({"rawStep": body}), true));
    }

    v
}

fn examples() -> Value {
    // Minimal typed spec exercising Filter + Refactor string rename.
    let typed_spec = wrap(vec![
        fileinput("src"),
        select_node(
            "sel",
            "src",
            json!([
                {"type": "Filter", "fields": ["property"], "filter": "[property] != NULL"},
                {"type": "Refactor", "fields": ["m01"], "refactor": {"rename": "amount"}},
            ]),
            None,
        ),
        output_node("out", &["sel"]),
    ]);

    // rawNode is opaque to the deriver: declare its wire fields explicitly,
    // and give Output explicit fields (no upstream inference possible).
    let rawnode_spec = wrap(vec![
        fileinput("src"),
        json!({"key": "x", "rawNode": {"type": "Extend", "id": "x", "alias": "x",
                "inputNodes": ["src"], "fields": src_fields()}}),
        json!({"key": "out", "type": "Output", "alias": "out",
               "inputs": ["x"], "fields": src_fields()}),
    ]);
    let rawstep_spec = step_spec(
        json!({"rawStep": {"type": "ToNumber", "fields": ["m01"]}}),
        true,
    );
    // archive: top + flow(excl. nodes) + explicit node bodies; a real
    // FileInput -> Output chain, Output declares fields explicitly.
    let archive_spec = json!({
        "format": 1,
        "archive": {
            "top": {"properties": {"modelDataType": "DataFlow"}},
            "flow": {},
            "nodes": [
                {"id": "n1", "body": {
                    "type": "FileInput", "id": "n1", "alias": "src",
                    "fileName": "data.xlsx#Sheet", "fields": src_fields(),
                }},
                {"id": "default", "body": {
                    "type": "Output", "id": "default", "alias": "out",
                    "inputNodes": ["n1"], "fields": src_fields(),
                }},
            ],
        },
    });
    // rawTable: a genuinely non-DataFlow table snapshot (no dataFlow).
    let rawtable_spec = json!({
        "format": 1,
        "rawTable": {
            "properties": {"modelDataType": "Ods"},
            "fields": src_fields(),
        },
    });

    json!({
        "typed": [typed_spec],
        "rawOnly": [rawnode_spec, rawstep_spec, archive_spec, rawtable_spec],
        "nodeExamples": node_examples(),
        "stepExamples": step_examples(),
    })
}
