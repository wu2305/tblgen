//! Integration tests for tblgen: generate/decompile/check/schema + CLI.

use serde_json::{json, Value};
use std::process::Command;
use tblgen::check::check;
use tblgen::decompile::decompile;
use tblgen::gen::{generate, generate_with_diagnostics, validate_spec};
use tblgen::model::*;
use tblgen::schema::schema_document;

/// Spec diagnostics minus warnings: most tests target error rules, and the
/// run-mode warning fires for inline specs that leave extractDataEnabled unset.
fn spec_errors(spec: &Value) -> Vec<Diag> {
    validate_spec(spec).into_iter().filter(|d| !d.is_warning()).collect()
}

fn budget_spec() -> Value {
    serde_json::from_str(include_str!("../examples/budget.spec.json")).unwrap()
}

// ---------------------------------------------------------------------------
// generate: budget example end-to-end
// ---------------------------------------------------------------------------

#[test]
fn budget_generates_valid_wire() {
    let spec = budget_spec();
    assert!(spec_errors(&spec).is_empty(), "spec must validate");
    let tbl = generate(&spec).expect("generate");
    let nodes = tbl.pointer("/dataFlow/nodes").unwrap().as_object().unwrap();
    // ids: first Output -> "default", others -> key
    assert!(nodes.contains_key("default"));
    assert!(nodes.contains_key("src"));
    assert!(nodes.contains_key("flt"));
    assert!(nodes.contains_key("pivot"));
    assert!(nodes.contains_key("ren"));
    // topo order sanity: inputNodes reference resolved ids
    assert_eq!(nodes["flt"]["inputNodes"], json!(["src"]));
    assert_eq!(nodes["ren"]["inputNodes"], json!(["pivot"]));
    assert_eq!(nodes["default"]["inputNodes"], json!(["ren"]));
    // FileInput: originalFields defaults to fields copy
    assert_eq!(
        nodes["src"]["originalFields"], nodes["src"]["fields"],
        "FileInput copies source fields into originalFields"
    );
    // config keys flattened to node level
    assert_eq!(nodes["src"]["fileName"], json!("budget.xlsx#Budget"));
    // Select steps; stepCount is emitted only when it exceeds the real
    // step count, so a matching count is omitted entirely
    assert_eq!(nodes["flt"]["steps"][0]["type"], json!("Filter"));
    assert!(nodes["flt"].get("stepCount").is_none());
    // Refactor rename applied through derivation: ren output has budget_amount
    let ren_names: Vec<String> = nodes["ren"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    // pivot carries `property` through with inputField identity preserved
    assert_eq!(
        ren_names,
        vec![
            "month".to_string(),
            "budget_amount".to_string(),
            "property".to_string()
        ]
    );
    // Output derives same fields; top-level partition mirrors default Output
    assert_eq!(tbl["dimensions"].as_array().unwrap().len(), 2);
    assert_eq!(tbl["measures"].as_array().unwrap().len(), 1);
    assert_eq!(tbl["dimensions"][0]["name"], json!("month"));
    assert_eq!(tbl["dimensions"][1]["name"], json!("property"));
    assert_eq!(tbl["measures"][0]["name"], json!("budget_amount"));
    // lineage: budget_amount traces to pivot.amount via ren
    let ba = nodes["ren"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "budget_amount")
        .unwrap();
    // Select fields never carry inputNode; inputField names the predecessor
    // field. `amount` is pivot-born (ColToRow header column): no origin is
    // invented for it.
    assert!(ba.get("inputNode").is_none());
    assert_eq!(ba["inputField"], json!("amount"));
    assert!(ba.get("originalNode").is_none());
    assert!(ba.get("originalField").is_none());
    // DwDataFlowContext#getFileTempFields maps physical columns only via
    // nonblank inputField — FileInput fields without explicit inputField/exp
    // get the identity mapping filled from `name`.
    for f in nodes["src"]["fields"].as_array().unwrap() {
        assert_eq!(
            f["inputField"].as_str().unwrap(),
            f["name"].as_str().unwrap(),
            "FileInput field {:?} must carry identity inputField",
            f["name"]
        );
    }
    // no timestamps anywhere
    let s = serde_json::to_string(&tbl).unwrap();
    assert!(
        !s.contains("lastModifyTime"),
        "gen must not emit lastModifyTime"
    );
}

#[test]
fn generate_is_deterministic() {
    let spec = budget_spec();
    let a = serde_json::to_string(&generate(&spec).unwrap()).unwrap();
    let b = serde_json::to_string(&generate(&spec).unwrap()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn generated_parameters_and_subprocesses_preserve_raw_overrides() {
    let mut spec = budget_spec();
    spec["parameters"] = json!([
        {"name":"first","dataType":"C","defaultValue":"typed",
         "raw":{"defaultValue":"raw","extra":true}},
        {"name":"second","dataType":"C"}
    ]);
    spec["subProcesses"] = json!({
        "defaulted":{"rootNodeId":"src","nodeIds":["src"]},
        "explicit":{"id":"explicit-id","rootNodeId":"src","nodeIds":["src"]},
        "overridden":{"rootNodeId":"src","nodeIds":["src"],
                      "raw":{"id":"raw-id","nodeIds":["src","flt"],"extra":true}}
    });

    let tbl = generate(&spec).unwrap();
    assert_eq!(
        tbl["parameters"],
        json!([
            {"name":"first","dataType":"C","defaultValue":"raw","extra":true},
            {"name":"second","dataType":"C"}
        ])
    );
    assert_eq!(
        tbl["dataFlow"]["subProcesses"],
        json!({
            "defaulted":{"id":"defaulted","rootNodeId":"src","nodeIds":["src"]},
            "explicit":{"id":"explicit-id","rootNodeId":"src","nodeIds":["src"]},
            "overridden":{"id":"raw-id","rootNodeId":"src","nodeIds":["src","flt"],"extra":true}
        })
    );

    spec["parameters"] = json!([]);
    spec["subProcesses"] = json!({});
    let tbl = generate(&spec).unwrap();
    assert_eq!(tbl["parameters"], json!([]));
    assert_eq!(tbl["dataFlow"]["subProcesses"], json!({}));

    let obj = spec.as_object_mut().unwrap();
    obj.remove("parameters");
    obj.remove("subProcesses");
    let tbl = generate(&spec).unwrap();
    assert!(tbl.get("parameters").is_none());
    assert!(tbl.pointer("/dataFlow/subProcesses").is_none());
}

#[test]
fn generated_parameters_and_subprocesses_report_invalid_shapes() {
    let cases = [
        (
            json!({}),
            json!([]),
            vec![
                Diag::spec(
                    "/parameters",
                    "structure.parameters",
                    "parameters must be an array",
                ),
                Diag::spec(
                    "/subProcesses",
                    "structure.subprocesses",
                    "subProcesses must be an object",
                ),
            ],
        ),
        (
            json!([false, {"name":"valid","dataType":"C","raw":[]}]),
            json!({"a/b~":false,"raw":{"rootNodeId":"src","nodeIds":["src"],"raw":[]}}),
            vec![
                Diag::spec(
                    "/parameters/0",
                    "structure.parameter",
                    "parameter must be an object",
                ),
                Diag::spec(
                    "/parameters/1/raw",
                    "structure.parameter.raw",
                    "parameter.raw must be an object",
                ),
                Diag::spec(
                    "/subProcesses/a~1b~0",
                    "structure.subprocess",
                    "subProcesses entries must be objects",
                ),
                Diag::spec(
                    "/subProcesses/raw/raw",
                    "structure.subprocess.raw",
                    "subProcesses.raw must be an object",
                ),
            ],
        ),
    ];
    for (parameters, subprocesses, expected) in cases {
        let mut spec = budget_spec();
        spec["parameters"] = parameters;
        spec["subProcesses"] = subprocesses;
        let (wire, diags) = generate_with_diagnostics(&spec);
        assert!(wire.is_none());
        let errors: Vec<_> = diags
            .iter()
            .filter(|d| !d.is_warning())
            .map(Diag::to_json)
            .collect();
        assert_eq!(errors, expected.iter().map(Diag::to_json).collect::<Vec<_>>());
    }
}

#[test]
fn lineage_identity_clone_rules() {
    // FileInput -> Select(no steps) -> Output: fields cloned with lineage
    let spec = json!({
        "format": 1,
        "nodes": [
            {"key":"a","type":"FileInput","fields":[
                {"name":"x","dataType":"C","isDimension":true,"originalNode":"src0","originalField":"x0"}
            ]},
            {"key":"b","type":"Select","inputs":["a"]},
            {"key":"c","type":"Output","inputs":["b"]}
        ]
    });
    let tbl = generate(&spec).unwrap();
    let nodes = tbl.pointer("/dataFlow/nodes").unwrap();
    let bf = &nodes["b"]["fields"][0];
    // Select fields never carry inputNode (single-input node)
    assert!(bf.get("inputNode").is_none());
    assert_eq!(bf["inputField"], json!("x"));
    // a source field's origin is its OWN (alias, name): the stale explicit
    // src0/x0 pair is rewritten, not trusted
    assert_eq!(bf["originalNode"], json!("a"));
    assert_eq!(bf["originalField"], json!("x"));
    // exp/aggType dropped at hop
    let spec2 = json!({
        "format": 1,
        "nodes": [
            {"key":"a","type":"FileInput","fields":[
                {"name":"x","dataType":"C","isDimension":true,"exp":"[x]","aggType":"SUM"}
            ]},
            {"key":"b","type":"Select","inputs":["a"]},
            {"key":"c","type":"Output","inputs":["b"]}
        ]
    });
    let tbl2 = generate(&spec2).unwrap();
    let bf2 = &tbl2.pointer("/dataFlow/nodes/b/fields/0").unwrap();
    assert!(bf2.get("exp").is_none());
    assert!(bf2.get("aggType").is_none());
}

// ---------------------------------------------------------------------------
// Enum coverage: every node/step type is either typed or raw-only
// ---------------------------------------------------------------------------

fn minimal_spec_with_node(node: Value) -> Value {
    json!({"format":1,"nodes":[node]})
}

#[test]
fn node_type_matrix() {
    for ty in NODE_TYPES {
        let node = json!({
            "key":"n","type":ty,
            "fields":[{"name":"f","dataType":"C","isDimension":true}]
        });
        let spec = minimal_spec_with_node(node.clone());
        let diags = spec_errors(&spec);
        if node_supported(ty) {
            // supported types may still fail arity/etc but must not fail on enum/coverage
            assert!(
                !diags
                    .iter()
                    .any(|d| d.rule == "enum.nodeType" || d.rule == "coverage.node.unknown"),
                "{}: {:?}",
                ty,
                diags.iter().map(|d| &d.rule).collect::<Vec<_>>()
            );
        } else {
            // unsupported enums must be rejected as typed
            assert!(
                diags.iter().any(|d| d.rule == "coverage.node.unknown"),
                "{}",
                ty
            );
        }
    }
    // unknown type string rejected entirely
    let spec = minimal_spec_with_node(json!({"key":"n","type":"NoSuchType"}));
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "enum.nodeType"));
}

#[test]
fn step_type_matrix() {
    for ty in STEP_TYPES {
        let step = json!({"type":ty});
        let spec = json!({
            "format":1,
            "nodes":[
                {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
                {"key":"s","type":"Select","inputs":["a"],
                 "fields":[{"name":"f","dataType":"C","isDimension":true}],
                 "steps":[step.clone()]},
                {"key":"o","type":"Output","inputs":["s"],
                 "fields":[{"name":"f","dataType":"C","isDimension":true}]}
            ]
        });
        let diags = spec_errors(&spec);
        if step_supported(ty) {
            assert!(
                !diags
                    .iter()
                    .any(|d| d.rule == "enum.stepType" || d.rule == "coverage.step.unknown"),
                "{}: {:?}",
                ty,
                diags.iter().map(|d| &d.rule).collect::<Vec<_>>()
            );
        } else {
            assert!(
                diags.iter().any(|d| d.rule == "coverage.step.unknown"),
                "{}",
                ty
            );
        }
    }
    // rawStep escape works for unsupported step
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}],
             "steps":[{"rawStep":{"type":"ToNumber","fields":["f"]}}]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec).is_empty());
    let tbl = generate(&spec).unwrap();
    assert_eq!(
        tbl.pointer("/dataFlow/nodes/s/steps/0/type").unwrap(),
        "ToNumber"
    );
}

// ---------------------------------------------------------------------------
// Rejections
// ---------------------------------------------------------------------------

#[test]
fn rejects_cycle_and_dangling() {
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"Select","inputs":["b"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"b","type":"Select","inputs":["a"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"o","type":"Output","inputs":["a"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec).iter().any(|d| d.rule == "graph.cycle"));

    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"o","type":"Output","inputs":["ghost"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "graph.danglingInput"));
}

#[test]
fn rejects_duplicate_ids_and_keys() {
    // two nodes resolving to same id
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","id":"same","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"b","type":"FileInput","id":"same","fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "structure.node.id.duplicate"));
    // duplicate keys
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "structure.node.key.duplicate"));
}

#[test]
fn rejects_arity_violations() {
    // Select with 0 inputs
    let spec = json!({
        "format":1,
        "nodes":[{"key":"s","type":"Select","inputs":[]}]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "err.dataflow.node.input"));
    // FileInput with input
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"b","type":"FileInput","inputs":["a"],"fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "err.dataflow.node.input"));
}

#[test]
fn rejects_nested_unknown_keys() {
    // unknown field key
    let spec = json!({
        "format":1,
        "nodes":[{"key":"a","type":"FileInput","fields":[
            {"name":"f","dataType":"C","isDimension":true,"bogus":1}
        ]}]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "structure.field.key"));
    // unknown config key (not FileInput)
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],
             "config":{"fileName":"x"}, // FileInput-only key on Select
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "structure.config.key"));
    // unknown top-level spec key
    let spec3 = json!({"format":1,"nodes":[],"surprise":{}});
    assert!(spec_errors(&spec3)
        .iter()
        .any(|d| d.rule == "structure.spec.key"));
    // unknown step payload key
    let spec4 = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}],
             "steps":[{"type":"Refactor","refactor":{"bogus":{}}}]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec4)
        .iter()
        .any(|d| d.rule == "structure.step.payload.key"));
}

#[test]
fn rejects_col_to_row_width_and_missing_fields() {
    // width mismatch
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"m1","dataType":"N","isDimension":false}]},
            {"key":"p","type":"ColToRow","inputs":["a"],
             "config":{"colToRowInfo":{"headers":["h1","h2"],
               "valuesArray":[["x"]]}},
             "fields":[{"name":"h1","dataType":"C","isDimension":true},{"name":"h2","dataType":"N","isDimension":false}]},
            {"key":"o","type":"Output","inputs":["p"],
             "fields":[{"name":"h1","dataType":"C","isDimension":true},{"name":"h2","dataType":"N","isDimension":false}]}
        ]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "err.dataflow.colToRow.config"));
    // ColToRow without fields -> rejected (no shape inference for pivot)
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"m1","dataType":"N","isDimension":false}]},
            {"key":"p","type":"ColToRow","inputs":["a"]},
            {"key":"o","type":"Output","inputs":["p"],
             "fields":[{"name":"h1","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "field.required"));
}

#[test]
fn rejects_step_field_not_found() {
    // Refactor references field not produced by input (string rename form)
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}],
             "steps":[{"type":"Refactor","fields":["ghost"],
                       "refactor":{"rename":"g"}}]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    // gen-side name tracking now proves the operated field is missing
    let diags = spec_errors(&spec);
    assert!(
        diags
            .iter()
            .any(|d| d.rule == "err.dataflow.stepField.notfound"),
        "{:?}",
        diags.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
    // and the wire check flags it too when the spec is bypassed
    let tbl = json!({"dataFlow":{"nodes":{
        "a":{"id":"a","type":"FileInput","alias":"a","fields":[{"name":"f","dataType":"C","isDimension":true}]},
        "s":{"id":"s","type":"Select","alias":"s","inputNodes":["a"],
            "fields":[{"name":"f","dataType":"C","isDimension":true}],
            "steps":[{"type":"Refactor","fields":["ghost"],"refactor":{"rename":"g"}}]},
        "o":{"id":"o","type":"Output","alias":"o","inputNodes":["s"],"fields":[]}
    }}});
    let wdiags = check(&tbl);
    assert!(
        wdiags
            .iter()
            .any(|d| d.rule == "err.dataflow.stepField.notfound"),
        "{:?}",
        wdiags.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
}

// ---------------------------------------------------------------------------
// rework semantics: string rename, AddField fields, Filter fields, malformed
// ---------------------------------------------------------------------------

#[test]
fn refactor_string_rename_semantics() {
    // rename is a STRING applied to the single selected field; it also
    // renames a field added earlier in the same step list.
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[
                {"name":"amount","dataType":"N","isDimension":false},
                {"name":"qty","dataType":"N","isDimension":false}]},
            {"key":"s","type":"Select","inputs":["a"],"steps":[
                {"type":"AddField","addField":{"name":"tmp","dataType":"C","isDimension":true,"exp":"[qty]"}},
                {"type":"Refactor","fields":["tmp"],"refactor":{"rename":"tmp_renamed"}},
                {"type":"Refactor","fields":["amount"],"refactor":{"rename":"budget_amount"}}
            ]},
            {"key":"o","type":"Output","inputs":["s"]}
        ]
    });
    assert!(
        spec_errors(&spec).is_empty(),
        "{:?}",
        spec_errors(&spec)
    );
    let tbl = generate(&spec).unwrap();
    let names: Vec<String> = tbl
        .pointer("/dataFlow/nodes/s/fields")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["budget_amount", "qty", "tmp_renamed"]);
}

#[test]
fn refactor_rejects_object_map_and_multi_field() {
    // the invented object-map form must be rejected
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],"steps":[
                {"type":"Refactor","fields":["f"],"refactor":{"rename":{"f":"g"}}}]},
            {"key":"o","type":"Output","inputs":["s"]}
        ]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "structure.step.payload.type"));

    // rename requires exactly one selected field
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[
                {"name":"f","dataType":"C","isDimension":true},
                {"name":"g","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],"steps":[
                {"type":"Refactor","fields":["f","g"],"refactor":{"rename":"h"}}]},
            {"key":"o","type":"Output","inputs":["s"]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "field.required"));
}

#[test]
fn addfield_and_filter_field_rules() {
    // AddField: a missing fields operand is synthesized as [addField.name];
    // an explicit non-empty operand is the platform's own selection and is
    // preserved verbatim even when it differs; an explicitly empty operand
    // is rejected.
    let base = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"]},
            {"key":"o","type":"Output","inputs":["s"]}
        ]
    });
    let mut spec = base.clone();
    spec["nodes"][1]["steps"] = json!([{"type":"AddField",
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(
        spec_errors(&spec).is_empty(),
        "{:?}",
        spec_errors(&spec)
    );
    let tbl = generate(&spec).unwrap();
    let names: Vec<&str> = tbl
        .pointer("/dataFlow/nodes/s/fields")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["f", "n1"], "AddField must emit nonempty fields");
    // the synthesized step operand is exactly [addField.name]
    assert_eq!(
        tbl.pointer("/dataFlow/nodes/s/steps/0/fields"),
        Some(&json!(["n1"]))
    );

    // explicit operand differing from addField.name is preserved verbatim
    let mut other = base.clone();
    other["nodes"][1]["steps"] = json!([{"type":"AddField","fields":["other"],
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(spec_errors(&other).is_empty(), "{:?}", spec_errors(&other));
    let tbl2 = generate(&other).unwrap();
    assert_eq!(
        tbl2.pointer("/dataFlow/nodes/s/steps/0/fields"),
        Some(&json!(["other"]))
    );

    // explicitly empty operand rejected
    let mut bad = base.clone();
    bad["nodes"][1]["steps"] = json!([{"type":"AddField","fields":[],
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(!spec_errors(&bad).is_empty());

    // Filter requires nonempty fields even for constant expressions
    let mut f = base.clone();
    f["nodes"][1]["steps"] = json!([{"type":"Filter","filter":"1 = 1"}]);
    assert!(spec_errors(&f).iter().any(|d| d.rule == "field.required"));
}

#[test]
fn case_insensitive_field_lookup() {
    // QueryImpl#getField/renameField compare uppercased: step operands may
    // differ in case from declared field names.
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"Amount","dataType":"N","isDimension":false}]},
            {"key":"s","type":"Select","inputs":["a"],"steps":[
                {"type":"Refactor","fields":["amount"],"refactor":{"rename":"AMOUNT"}},
                {"type":"RemoveField","fields":["AMOUNT"]}]},
            {"key":"o","type":"Output","inputs":["s"]}
        ]
    });
    let diags = spec_errors(&spec);
    assert!(diags.is_empty(), "{:?}", diags);
}

#[test]
fn explicit_select_fields_mismatch_detected() {
    // provable derivation: FileInput[f] -> Select(no steps) explicit [g]
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"a","type":"FileInput","fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"s","type":"Select","inputs":["a"],
             "fields":[{"name":"g","dataType":"C","isDimension":true}]},
            {"key":"o","type":"Output","inputs":["s"]}
        ]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "field.mismatch"));
}

// ---------------------------------------------------------------------------
// decompile: generated form preferred, archive fallback, rawTable
// ---------------------------------------------------------------------------

#[test]
fn decompile_roundtrip_generated() {
    let spec = budget_spec();
    let tbl = generate(&spec).unwrap();
    let back = decompile(&tbl).expect("decompile");
    assert_eq!(back["format"], json!(1));
    assert!(
        back.get("nodes").is_some(),
        "expected generated form, got {}",
        back
    );
    // regenerating the decompiled spec must equal the normalized input
    let wire2 = generate(&back).unwrap();
    assert_eq!(wire2, tbl, "decompile->generate must round-trip");
}

#[test]
fn decompile_nonflow_is_rawtable() {
    let tbl = json!({"version":"4.0.0","properties":{"modelDataType":"Ods"},"dimensions":[]});
    let spec = decompile(&tbl).unwrap();
    assert_eq!(spec["rawTable"], tbl);
}

#[test]
fn decompile_unsupported_node_falls_back_to_archive() {
    // a wire tbl containing a SlowChanging node cannot be typed -> archive
    let tbl = json!({
        "version":"4.19.0",
        "properties":{"modelDataType":"DataFlow"},
        "dimensions":[],"measures":[],
        "dataFlow":{"nodes":{
            "s1":{"id":"s1","type":"SlowChanging","alias":"sc","extensionName":"SlowChanging","inputNodes":[]},
            "o1":{"id":"o1","type":"Output","alias":"out","inputNodes":["s1"],"fields":[]}
        }}
    });
    let spec = decompile(&tbl).unwrap();
    // either generated-with-rawNode that round-trips, or archive — both preserve
    if spec.get("archive").is_some() {
        let wire = generate(&spec).unwrap();
        assert_eq!(
            wire["dataFlow"]["nodes"]["s1"]["type"],
            json!("SlowChanging")
        );
    } else {
        assert!(spec["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.get("rawNode").is_some()));
        let wire = generate(&spec).unwrap();
        assert_eq!(wire, tbl);
    }
}

fn rawnode_modeltable_spec() -> Value {
    json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"r","rawNode":{"type":"ModelTable","id":"r","alias":"r","moduleTablePath":"m.tbl"}},
            {"key":"out","type":"Output","alias":"out","inputs":["r"],
             "fields":[{"name":"A","dataType":"C","isDimension":true}]}
        ]
    })
}

#[test]
fn decompile_typed_last_modify_time_still_generated() {
    let spec = budget_spec();
    let mut tbl = generate(&spec).unwrap();
    tbl.pointer_mut("/dataFlow/nodes/src")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("lastModifyTime".into(), json!("2024-01-01T00:00:00Z"));
    let back = decompile(&tbl).expect("decompile");
    assert!(
        back.get("nodes").is_some(),
        "typed nodes must not be forced to archive by lastModifyTime: {}",
        back
    );
    assert!(back.get("archive").is_none());
    let wire2 = generate(&back).unwrap();
    let mut expected = tbl.clone();
    expected
        .pointer_mut("/dataFlow/nodes/src")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("lastModifyTime");
    assert_eq!(wire2, expected, "typed round-trip ignores normalized timestamp");
}

#[test]
fn decompile_rawnode_without_last_modify_time_roundtrips_generated() {
    let spec = rawnode_modeltable_spec();
    let tbl = generate(&spec).unwrap();
    let back = decompile(&tbl).expect("decompile");
    assert!(
        back.get("nodes").is_some(),
        "rawNode without lastModifyTime may still use generated form: {}",
        back
    );
    assert!(back.get("archive").is_none());
    assert!(back["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.get("rawNode").is_some()));
    assert_eq!(generate(&back).unwrap(), tbl, "rawNode generated form round-trips");
}

#[test]
fn decompile_rawnode_last_modify_time_archives_verbatim() {
    let spec = rawnode_modeltable_spec();
    let mut tbl = generate(&spec).unwrap();
    tbl.pointer_mut("/dataFlow/nodes/r")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .insert("lastModifyTime".into(), json!("2024-01-01T00:00:00Z"));
    let back = decompile(&tbl).expect("decompile");
    assert!(
        back.get("archive").is_some(),
        "rawNode carrying lastModifyTime cannot round-trip typed/generated: {}",
        back
    );
    assert!(back.get("nodes").is_none());
    let nodes = back.pointer("/archive/nodes").unwrap().as_array().unwrap();
    let r = nodes.iter().find(|n| n["id"] == json!("r")).unwrap();
    assert_eq!(r["body"]["type"], json!("ModelTable"));
    assert_eq!(
        r["body"]["lastModifyTime"],
        json!("2024-01-01T00:00:00Z"),
        "archive keeps raw node timestamps verbatim"
    );
    let wire = generate(&back).unwrap();
    assert_eq!(
        wire.pointer("/dataFlow/nodes/r/lastModifyTime").unwrap(),
        &json!("2024-01-01T00:00:00Z")
    );
}

#[test]
fn archive_form_is_editable() {
    // build archive spec by hand, generate, verify structure
    let spec = json!({
        "format":1,
        "archive":{
            "top":{"version":"4.19.0","properties":{"modelDataType":"DataFlow","customKey":42}},
            "flow":{},
            "nodes":[
                {"id":"n1","body":{"id":"n1","type":"Output","alias":"o","inputNodes":[],"lastModifyTime":"2020-01-01T00:00:00Z"}}
            ]
        }
    });
    let tbl = generate(&spec).unwrap();
    assert_eq!(tbl["properties"]["customKey"], json!(42));
    assert_eq!(
        tbl["dataFlow"]["nodes"]["n1"]["lastModifyTime"],
        json!("2020-01-01T00:00:00Z"),
        "archive preserves timestamps verbatim"
    );
    // decompile of that tbl must yield archive again (lastModifyTime only normalized in compare)
    let back = decompile(&tbl).unwrap();
    let wire2 = generate(&back).unwrap();
    // normalized comparison strips lastModifyTime; archive keeps it verbatim
    let mut norm = tbl.clone();
    norm.pointer_mut("/dataFlow/nodes/n1")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("lastModifyTime");
    let mut norm2 = wire2.clone();
    norm2
        .pointer_mut("/dataFlow/nodes/n1")
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove("lastModifyTime");
    assert_eq!(norm, norm2);
}

// ---------------------------------------------------------------------------
// check(): wire diagnostics
// ---------------------------------------------------------------------------

#[test]
fn check_wire_violations() {
    let tbl = json!({
        "dataFlow":{"nodes":{
            "a":{"id":"a","type":"Select","alias":"a","inputNodes":["ghost"],"fields":[]},
            "b":{"id":"b","type":"Output","alias":"b","inputNodes":["a","a"],"fields":[]}
        }}
    });
    let diags = check(&tbl);
    // dangling input ids are hard errors (null executor deref in Java);
    // duplicate edges remain editor-only (warn.*)
    assert!(diags.iter().any(|d| d.rule == "graph.danglingInput"));
    assert!(diags
        .iter()
        .any(|d| d.rule == "warn.editor.inputNodes.duplicate"));
    assert!(diags.iter().any(|d| d.rule == "err.dataflow.node.input")); // Output 2 inputs
                                                                        // nonflow: no diagnostics (skipped)
    assert!(check(&json!({"properties":{}})).is_empty());
}

// ---------------------------------------------------------------------------
// schema + CLI
// ---------------------------------------------------------------------------

#[test]
fn schema_is_valid_2020_12() {
    let doc = schema_document();
    assert_eq!(
        doc["schema"]["$schema"],
        json!("https://json-schema.org/draft/2020-12/schema")
    );
    assert!(doc["schema"]["oneOf"].is_array());
    assert!(doc["examples"]["typed"].is_array());
    assert!(doc["examples"]["rawOnly"].is_array());
    assert_eq!(
        doc["examples"]["nodeExamples"].as_array().unwrap().len(),
        NODE_TYPES.len(),
        "one example per node enum"
    );
    assert_eq!(
        doc["examples"]["stepExamples"].as_array().unwrap().len(),
        STEP_TYPES.len(),
        "one example per step enum"
    );
    // typed example must itself be a valid spec
    for ex in doc["examples"]["typed"].as_array().unwrap() {
        assert!(
            spec_errors(ex).is_empty(),
            "typed example invalid: {:?}",
            spec_errors(ex)
        );
    }
}

/// Every nodeExamples/stepExamples entry is a COMPLETE spec: typed examples
/// must generate + pass wire check with zero errors; rawOnly examples must be
/// rejected as typed AND round-trip their raw payload verbatim.
#[test]
fn schema_examples_generate_and_check() {
    let doc = schema_document();
    for group in ["nodeExamples", "stepExamples"] {
        for ex in doc["examples"][group].as_array().unwrap() {
            let label = ex["label"].as_str().unwrap();
            let ty = ex["type"].as_str().unwrap_or("?");
            let spec = &ex["spec"];
            if label == "typed" {
                let diags = spec_errors(spec);
                assert!(
                    diags.is_empty(),
                    "{} {} spec invalid: {:?}",
                    group,
                    ty,
                    diags
                );
                let tbl = generate(spec).unwrap_or_else(|d| {
                    panic!("{} {} generate failed: {}", group, ty, d.to_json())
                });
                let wdiags = check(&tbl);
                let errs: Vec<_> = wdiags
                    .iter()
                    .filter(|d| !d.rule.starts_with("warn."))
                    .collect();
                assert!(
                    errs.is_empty(),
                    "{} {} wire check errors: {:?}",
                    group,
                    ty,
                    errs.iter().map(|d| &d.rule).collect::<Vec<_>>()
                );
            } else {
                // rawOnly: the same spec must NOT validate as typed — i.e.
                // the raw escape is required. Verify the raw payload is
                // emitted verbatim into the wire.
                let tbl = generate(spec).unwrap_or_else(|d| {
                    panic!("{} {} rawOnly generate failed: {}", group, ty, d.to_json())
                });
                let s = serde_json::to_string(&tbl).unwrap();
                // the raw payload body must appear verbatim somewhere
                if let Some(rawnode) = spec
                    .pointer("/nodes")
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().find(|n| n.get("rawNode").is_some()))
                    .and_then(|n| n.get("rawNode"))
                {
                    assert!(
                        s.contains(&serde_json::to_string(rawnode).unwrap()),
                        "{} {} rawNode payload not preserved verbatim",
                        group,
                        ty
                    );
                }
                if let Some(rawstep) = spec
                    .pointer("/nodes")
                    .and_then(Value::as_array)
                    .and_then(|a| a.iter().find_map(|n| n.pointer("/steps/0/rawStep")))
                {
                    assert!(
                        s.contains(&serde_json::to_string(rawstep).unwrap()),
                        "{} {} rawStep payload not preserved verbatim",
                        group,
                        ty
                    );
                }
                // rawOnly wire must still pass check with no hard errors
                // (coverage/unknown-type warnings are expected)
                let wdiags = check(&tbl);
                let errs: Vec<_> = wdiags
                    .iter()
                    .filter(|d| !d.rule.starts_with("warn."))
                    .collect();
                assert!(
                    errs.is_empty(),
                    "{} {} rawOnly wire check errors: {:?}",
                    group,
                    ty,
                    errs.iter().map(|d| &d.rule).collect::<Vec<_>>()
                );
            }
        }
    }
}

#[test]
fn cli_gen_check_schema() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let dir = std::env::temp_dir().join(format!("tblgen-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let spec_path = dir.join("budget.spec.json");
    let tbl_path = dir.join("budget.tbl.json");
    std::fs::write(&spec_path, include_str!("../examples/budget.spec.json")).unwrap();

    // gen -> file
    let out = Command::new(bin)
        .args([
            "gen",
            spec_path.to_str().unwrap(),
            "-o",
            tbl_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "gen failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(tbl_path.exists());

    // gen again to same -o must fail (create_new, no overwrite)
    let out2 = Command::new(bin)
        .args([
            "gen",
            spec_path.to_str().unwrap(),
            "-o",
            tbl_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(2));

    // check the generated wire
    let out3 = Command::new(bin)
        .args(["check", tbl_path.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        out3.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&out3.stderr)
    );
    let report: Value = serde_json::from_slice(&out3.stdout).unwrap();
    assert_eq!(report["files"][0]["scope"], json!("dataflow"));
    assert_eq!(report["files"][0]["errors"].as_array().unwrap().len(), 0);

    // decompile roundtrip
    let spec2_path = dir.join("back.spec.json");
    let out4 = Command::new(bin)
        .args([
            "decompile",
            tbl_path.to_str().unwrap(),
            "-o",
            spec2_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        out4.status.success(),
        "decompile failed: {}",
        String::from_utf8_lossy(&out4.stderr)
    );
    let back: Value = serde_json::from_str(&std::fs::read_to_string(&spec2_path).unwrap()).unwrap();
    assert!(back.get("nodes").is_some());

    // schema command
    let out5 = Command::new(bin).arg("schema").output().unwrap();
    assert!(out5.status.success());
    let s: Value = serde_json::from_slice(&out5.stdout).unwrap();
    assert!(s["schema"].is_object());

    // malformed JSON -> exit 1, no panic
    let bad = dir.join("bad.json");
    std::fs::write(&bad, "{not json").unwrap();
    let out6 = Command::new(bin)
        .args(["gen", bad.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(out6.status.code(), Some(1));

    // usage error -> exit 2
    let out7 = Command::new(bin).arg("bogus").output().unwrap();
    assert_eq!(out7.status.code(), Some(2));

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// final-round focused rules
// ---------------------------------------------------------------------------

#[test]
fn modeltable_blank_path_is_hard_error() {
    // wire: ModelTable without moduleTablePath -> init throws
    // (DwDataFlowNodeContributor_dwtable.java:47-52)
    let tbl = json!({
        "dataFlow":{"nodes":{
            "m":{"id":"m","type":"ModelTable","alias":"m","moduleTablePath":"","fields":[]},
            "o":{"id":"o","type":"Output","alias":"o","inputNodes":["m"],"fields":[]}
        }}
    });
    let diags = check(&tbl);
    assert!(diags
        .iter()
        .any(|d| d.rule == "config.modeltable.path" && !d.rule.starts_with("warn.")));
    // missing key entirely -> same error
    let tbl2 = json!({
        "dataFlow":{"nodes":{
            "m":{"id":"m","type":"ModelTable","alias":"m","fields":[]},
            "o":{"id":"o","type":"Output","alias":"o","inputNodes":["m"],"fields":[]}
        }}
    });
    assert!(check(&tbl2)
        .iter()
        .any(|d| d.rule == "config.modeltable.path"));
    // non-blank path -> no such error (external existence stays unknown)
    let tbl3 = json!({
        "dataFlow":{"nodes":{
            "m":{"id":"m","type":"ModelTable","alias":"m","moduleTablePath":"../ods/x.tbl","fields":[]},
            "o":{"id":"o","type":"Output","alias":"o","inputNodes":["m"],"fields":[]}
        }}
    });
    assert!(!check(&tbl3)
        .iter()
        .any(|d| d.rule == "config.modeltable.path"));
}

#[test]
fn dangling_input_is_hard_error() {
    let tbl = json!({
        "dataFlow":{"nodes":{
            "o":{"id":"o","type":"Output","alias":"o","inputNodes":["ghost"],"fields":[]}
        }}
    });
    let diags = check(&tbl);
    let d = diags
        .iter()
        .find(|d| d.rule == "graph.danglingInput")
        .expect("dangling input must be a hard error");
    assert!(d.source.contains("DwDataFlowContext#getInputExes"));
}

#[test]
fn field_scalar_types_are_enforced() {
    // optional scalar wrong type -> spec rejected
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"f","dataType":"N","isDimension":false,"length":"x"}
            ]},
            {"key":"o","type":"Output","inputs":["i"],
             "fields":[{"name":"f","dataType":"N","isDimension":false}]}
        ]
    });
    assert!(spec_errors(&spec)
        .iter()
        .any(|d| d.rule == "structure.field.type" && d.path.ends_with("/length")));
    // extractable must be boolean
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"f","dataType":"C","isDimension":true,"extractable":"yes"}
            ]},
            {"key":"o","type":"Output","inputs":["i"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "structure.field.type" && d.path.ends_with("/extractable")));
}

#[test]
fn spechars_must_be_string_and_rename_path_exact() {
    // spechars as object -> rejected (getString reads a string)
    let spec = json!({
        "format":1,
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"f","dataType":"C","isDimension":true}
            ]},
            {"key":"s","type":"Select","inputs":["i"],"steps":[
                {"type":"DeleteSpechars","fields":["f"],"spechars":{"a":1}}
            ]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    let ds = spec_errors(&spec);
    assert!(
        ds.iter()
            .any(|d| d.rule == "structure.step.payload.type" && d.path.ends_with("/spechars")),
        "spechars object must be rejected: {:?}",
        ds
    );

    // scalar rename diagnostic path: /nodes/<idx>/steps/<si>/refactor/rename —
    // payload name appended exactly once (was previously doubled/lowercased)
    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"amount","dataType":"N","isDimension":false}
            ]},
            {"key":"s","type":"Select","inputs":["i"],"steps":[
                {"type":"Refactor","fields":["amount"],"refactor":{"rename":5}}
            ]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"amount","dataType":"N","isDimension":false}]}
        ]
    });
    let ds2 = spec_errors(&spec2);
    let rename_diag = ds2
        .iter()
        .find(|d| d.path.contains("refactor"))
        .expect("rename type error expected");
    assert_eq!(rename_diag.path, "/nodes/1/steps/0/refactor/rename");
}

#[test]
fn invalid_step_enums_rejected() {
    let mk = |payload: Value| {
        json!({
            "format":1,
            "nodes":[
                {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                    {"name":"f","dataType":"C","isDimension":true}
                ]},
                {"key":"s","type":"Select","inputs":["i"],"steps":[payload]},
                {"key":"o","type":"Output","inputs":["s"],
                 "fields":[{"name":"f","dataType":"C","isDimension":true}]}
            ]
        })
    };
    for (step, path_suffix) in [
        (
            json!({"type":"Trim","fields":["f"],"trim":{"trimPosition":"Middle"}}),
            "trimPosition",
        ),
        (
            json!({"type":"SplitField","fields":["f"],"split":{"splitString":",","splitOff":"Middle","splitType":"Column"}}),
            "splitOff",
        ),
        (
            json!({"type":"SubString","fields":["f"],"subString":{"type":"Center","count":1,"start":0,"end":1}}),
            "type",
        ),
    ] {
        let ds = spec_errors(&mk(step));
        assert!(
            ds.iter()
                .any(|d| d.rule == "structure.step.payload.enum" && d.path.ends_with(path_suffix)),
            "invalid enum must be rejected at {}: {:?}",
            path_suffix,
            ds
        );
    }
    // valid enum values pass
    let ok = mk(json!({"type":"Trim","fields":["f"],"trim":{"trimPosition":"FrontAndEnd"}}));
    assert!(!spec_errors(&ok)
        .iter()
        .any(|d| d.rule == "structure.step.payload.enum"));
}

#[test]
fn expression_refs_validated_conservatively() {
    // [missing] != NULL -> proved-bad unqualified ref rejected
    let bad = json!({
        "format":1,
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"f","dataType":"C","isDimension":true}
            ]},
            {"key":"s","type":"Select","inputs":["i"],"steps":[
                {"type":"Filter","fields":["f"],"filter":"[missing] != NULL"}
            ]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    let ds = spec_errors(&bad);
    assert!(
        ds.iter()
            .any(|d| d.rule == "err.exp.ref.notfound" && d.path.ends_with("/filter")),
        "bad ref must fail: {:?}",
        ds
    );

    // parameter ref [上传文件年月] resolves via parameters -> valid
    let ok = json!({
        "format":1,
        "parameters":[{"name":"上传文件年月","dataType":"C"}],
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"f","dataType":"C","isDimension":true}
            ]},
            {"key":"s","type":"Select","inputs":["i"],"steps":[
                {"type":"AddField","fields":["f"],"addField":{
                    "name":"ym","dataType":"C","isDimension":true,
                    "exp":"[上传文件年月]"
                }}
            ]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[
                {"name":"f","dataType":"C","isDimension":true},
                {"name":"ym","dataType":"C","isDimension":true}
             ]}
        ]
    });
    let ds2 = spec_errors(&ok);
    assert!(
        !ds2.iter().any(|d| d.rule == "err.exp.ref.notfound"),
        "parameter ref must be valid: {:?}",
        ds2
    );

    // quoted '[missing]' inside a literal -> no false positive
    let quoted = json!({
        "format":1,
        "nodes":[
            {"key":"i","type":"FileInput","config":{"fileName":"x.xlsx"},"fields":[
                {"name":"f","dataType":"C","isDimension":true}
            ]},
            {"key":"s","type":"Select","inputs":["i"],"steps":[
                {"type":"Filter","fields":["f"],"filter":"[f] = '[missing]'"}
            ]},
            {"key":"o","type":"Output","inputs":["s"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    let ds3 = spec_errors(&quoted);
    assert!(
        !ds3.iter().any(|d| d.rule == "err.exp.ref.notfound"),
        "quoted literal must not be scanned: {:?}",
        ds3
    );

    // wire side: same bad ref -> warn.exp.ref.notfound (not hard error)
    let wire = json!({
        "dataFlow":{"nodes":{
            "i":{"id":"i","type":"FileInput","alias":"i","fileName":"x.xlsx","fields":[
                {"name":"f","dataType":"C","isDimension":true}
            ]},
            "s":{"id":"s","type":"Select","alias":"s","inputNodes":["i"],
                "fields":[{"name":"f","dataType":"C","isDimension":true}],
                "steps":[{"type":"Filter","fields":["f"],"filter":"[missing] != NULL"}]},
            "o":{"id":"o","type":"Output","alias":"o","inputNodes":["s"],
                "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        }}
    });
    let wd = check(&wire);
    assert!(
        wd.iter().any(|d| d.rule == "warn.exp.ref.notfound"),
        "wire bad ref must warn: {:?}",
        wd.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
    assert!(!wd.iter().any(|d| d.rule == "err.exp.ref.notfound"));
}

/// The general `examples.typed` / `examples.rawOnly` arrays: every entry must
/// validate, generate, and wire-check with zero hard errors; archive and
/// rawTable forms assert their distinct semantics.
#[test]
fn schema_general_examples_typed_and_rawonly() {
    let doc = schema_document();
    let ex = &doc["examples"];

    // typed[] — plain generated-form specs
    for (i, spec) in ex["typed"].as_array().unwrap().iter().enumerate() {
        let ds = spec_errors(spec);
        assert!(ds.is_empty(), "typed[{}] invalid: {:?}", i, ds);
        let tbl = generate(spec).unwrap_or_else(|d| panic!("typed[{}] gen: {}", i, d.to_json()));
        let w = check(&tbl);
        let errs: Vec<_> = w.iter().filter(|d| !d.rule.starts_with("warn.")).collect();
        assert!(
            errs.is_empty(),
            "typed[{}] wire errors: {:?}",
            i,
            errs.iter().map(|d| &d.rule).collect::<Vec<_>>()
        );
    }

    // rawOnly[] — first two are generated-form specs with raw escapes;
    // last two are the archive / rawTable snapshot forms.
    let raw = ex["rawOnly"].as_array().unwrap();
    assert_eq!(raw.len(), 4, "expected rawNode, rawStep, archive, rawTable");
    for (i, spec) in raw[..2].iter().enumerate() {
        let ds = spec_errors(spec);
        assert!(ds.is_empty(), "rawOnly[{}] spec invalid: {:?}", i, ds);
        let tbl = generate(spec).unwrap_or_else(|d| panic!("rawOnly[{}] gen: {}", i, d.to_json()));
        let w = check(&tbl);
        let errs: Vec<_> = w.iter().filter(|d| !d.rule.starts_with("warn.")).collect();
        assert!(
            errs.is_empty(),
            "rawOnly[{}] wire errors: {:?}",
            i,
            errs.iter().map(|d| &d.rule).collect::<Vec<_>>()
        );
    }

    // archive form: merges top+flow+node bodies into a wire DataFlow
    let archive = &raw[2];
    assert!(
        spec_errors(archive).is_empty(),
        "archive spec invalid: {:?}",
        spec_errors(archive)
    );
    let tbl = generate(archive).unwrap();
    assert_eq!(tbl["properties"]["modelDataType"], json!("DataFlow"));
    let nodes = &tbl["dataFlow"]["nodes"];
    assert_eq!(nodes["n1"]["type"], json!("FileInput"));
    assert_eq!(nodes["default"]["type"], json!("Output"));
    assert_eq!(nodes["default"]["inputNodes"], json!(["n1"]));
    let w = check(&tbl);
    let errs: Vec<_> = w.iter().filter(|d| !d.rule.starts_with("warn.")).collect();
    assert!(
        errs.is_empty(),
        "archive wire errors: {:?}",
        errs.iter().map(|d| &d.rule).collect::<Vec<_>>()
    );
    // decompile of the merged wire must yield a usable spec
    let back = decompile(&tbl).expect("archive wire decompile");
    assert_eq!(back["format"], json!(1));

    // rawTable form: verbatim non-DataFlow snapshot — no dataFlow, Ods model
    let rawtable = &raw[3];
    assert!(
        spec_errors(rawtable).is_empty(),
        "rawTable spec invalid: {:?}",
        spec_errors(rawtable)
    );
    let tbl = generate(rawtable).unwrap();
    assert_eq!(tbl["properties"]["modelDataType"], json!("Ods"));
    assert!(
        tbl.get("dataFlow").is_none(),
        "rawTable must not contain dataFlow"
    );
    // nonflow is not assessed for validity (skipped scope -> no diagnostics)
    assert!(
        check(&tbl).is_empty(),
        "nonflow rawTable must produce no diagnostics"
    );
    let back = decompile(&tbl).unwrap();
    assert_eq!(back["rawTable"], tbl, "rawTable must round-trip verbatim");
}

// ---------------------------------------------------------------------------
// properties: typed set from DwTableLoader#loadProperties, raw duplicates,
// run mode (extractDataEnabled)
// ---------------------------------------------------------------------------

fn props_spec(props: Value) -> Value {
    json!({
        "format": 1,
        "properties": props,
        "nodes": [
            {"key":"mt","type":"ModelTable","config":{"moduleTablePath":"../a.tbl"},
             "fields":[{"name":"m","dataType":"C","isDimension":true,"dbfield":"M"}]},
            {"key":"out","type":"Output","inputs":["mt"]}
        ]
    })
}

fn rules(diags: &[Diag]) -> Vec<String> {
    diags.iter().map(|d| d.rule.clone()).collect()
}

#[test]
fn properties_copied_block_in_raw_is_accepted() {
    // a real model's properties pasted whole into raw must not collide
    let spec = props_spec(json!({
        "extractDataEnabled": true,
        "dbTableName": "ads_x",
        "raw": {"extractDataEnabled": true, "dbTableName": "ads_x", "rollbackEnabled": false,
                "modifyDbTableEnabled": ["field", "index", "constraint"]}
    }));
    assert!(validate_spec(&spec).is_empty(), "{:?}", rules(&validate_spec(&spec)));
    let p = generate(&spec).unwrap()["properties"].clone();
    assert_eq!(p["extractDataEnabled"], json!(true));
    assert_eq!(p["rollbackEnabled"], json!(false));
}

#[test]
fn properties_raw_conflict_is_an_error() {
    let spec = props_spec(json!({
        "extractDataEnabled": true, "dbTableName": "t",
        "raw": {"extractDataEnabled": false}
    }));
    assert_eq!(rules(&validate_spec(&spec)), ["structure.properties.raw.conflict"]);
}

#[test]
fn properties_typed_values_are_type_checked() {
    let spec = props_spec(json!({
        "extractDataEnabled": false,
        "rollbackEnabled": "no",
        "modifyDbTableEnabled": ["field", "rows"]
    }));
    let d = validate_spec(&spec);
    assert_eq!(rules(&d), ["structure.properties", "structure.properties"]);
}

#[test]
fn run_mode_warnings_do_not_block_gen() {
    let unset = props_spec(json!({}));
    assert_eq!(rules(&validate_spec(&unset)), ["warn.properties.runMode.unset"]);
    assert!(generate(&unset).is_ok());

    let no_table = props_spec(json!({"extractDataEnabled": true}));
    assert_eq!(rules(&validate_spec(&no_table)), ["warn.properties.runMode.noTable"]);

    let tbl = generate(&no_table).unwrap();
    assert!(rules(&check(&tbl)).contains(&"warn.properties.runMode.noTable".to_string()));

    let query = props_spec(json!({"extractDataEnabled": false}));
    assert!(validate_spec(&query).is_empty());
}

#[test]
fn decompile_keeps_off_shape_typed_values_in_raw() {
    let spec = props_spec(json!({"extractDataEnabled": false, "raw": {"rollbackEnabled": "false"}}));
    let tbl = generate(&spec).unwrap();
    let back = decompile(&tbl).unwrap();
    assert_eq!(back["properties"]["raw"]["rollbackEnabled"], json!("false"));
    assert_eq!(generate(&back).unwrap(), tbl);
}

// ---------------------------------------------------------------------------
// CLI: new + project-aware check
// ---------------------------------------------------------------------------

fn cli(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tblgen")).args(args).output().unwrap()
}

fn report(out: &std::process::Output) -> Vec<(String, Vec<String>)> {
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    v["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            let rules = f["errors"].as_array().unwrap().iter().map(|e| e["rule"].as_str().unwrap().to_string());
            (f["file"].as_str().unwrap().rsplit('/').next().unwrap().to_string(), rules.collect())
        })
        .collect()
}

#[test]
fn new_models_pass_project_check() {
    let root = std::env::temp_dir().join(format!("tblgen-new-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let tables = root.join("P/data/tables");
    std::fs::create_dir_all(&tables).unwrap();
    let t = |p: &str| tables.join(p).display().to_string();

    let o = cli(&["new", "script", "--table", "ods_s", "--schema", "ods", "--extract", "true",
                  "--fields", "k:C:20,v:N:18.2:m", "-o", &t("ods/ods_s.tbl")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let script = root.join(format!("P/data/script/{}.action.ts", tblgen::new::md5_hex(b"ods_s")));
    assert!(script.is_file());

    let o = cli(&["new", "dsl", "--table", "dim_d", "--extract", "false", "--input", "../ods/ods_s.tbl",
                  "--fields", "k", "-o", &t("dim/dim_d.tbl")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let o = cli(&["new", "dsl", "--table", "dwd_j", "--extract", "true", "--input", "../ods/ods_s.tbl",
                  "--input", "$DATA:/dim/dim_d.tbl", "--join-on", "k", "--fields", "k,v",
                  "-o", &t("dwd/dwd_j.tbl")]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));

    // never overwrite; unknown fields and typed dsl fields are rejected
    assert_eq!(cli(&["new", "dsl", "--table", "dim_d", "--extract", "false", "--input",
                     "../ods/ods_s.tbl", "-o", &t("dim/dim_d.tbl")]).status.code(), Some(2));
    assert_eq!(cli(&["new", "dsl", "--table", "x", "--extract", "false", "--input",
                     "../ods/ods_s.tbl", "--fields", "nope", "-o", &t("dim/x.tbl")]).status.code(), Some(2));
    assert_eq!(cli(&["new", "script", "--table", "x", "--fields", "k:C", "-o", &t("x.tbl")]).status.code(), Some(2),
               "--extract is required");

    let o = cli(&["check", &root.display().to_string()]);
    assert!(o.status.success(), "{:?}", report(&o));
    assert_eq!(report(&o).len(), 4);

    // break the references: missing upstream model and missing script
    std::fs::remove_file(tables.join("dim/dim_d.tbl")).unwrap();
    std::fs::remove_file(&script).unwrap();
    let r = report(&cli(&["check", &root.display().to_string()]));
    assert!(r.contains(&("dwd_j.tbl".into(), vec!["ref.modelTable.missing".into()])), "{:?}", r);
    // --exclude drops everything under the path from the report
    let dwd = tables.join("dwd").display().to_string();
    let r = report(&cli(&["check", &root.display().to_string(), "--exclude", &dwd]));
    assert!(!r.iter().any(|(f, _)| f == "dwd_j.tbl"), "{:?}", r);
    assert_eq!(cli(&["check", &root.display().to_string(), "--exclude", "/no/such/dir"]).status.code(), Some(2));
    assert!(r.contains(&("ods_s.tbl".into(), vec!["ref.script.missing".into()])), "{:?}", r);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn renamed_script_fields_are_caught() {
    // Chinese display names on the model while the script still declares the
    // English names: both the script name rule and the lineage rule fire
    let root = std::env::temp_dir().join(format!("tblgen-rename-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let tbl = root.join("P/data/tables/ods/ods_r.tbl");
    std::fs::create_dir_all(tbl.parent().unwrap()).unwrap();
    let o = cli(&["new", "script", "--table", "ods_r", "--extract", "true",
                  "--fields", "stat_month:C:7,amt:N:18.2:m", "-o", &tbl.display().to_string()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let ok = report(&cli(&["check", &root.display().to_string()]));
    assert!(ok.iter().all(|(_, e)| e.is_empty()), "{:?}", ok);

    let mut d: Value = serde_json::from_str(&std::fs::read_to_string(&tbl).unwrap()).unwrap();
    fn rename(v: &mut Value) {
        match v {
            Value::Object(m) => {
                if let Some(Value::String(n)) = m.get_mut("name") {
                    if n == "stat_month" { *n = "统计年月".into(); }
                }
                m.values_mut().for_each(rename);
            }
            Value::Array(a) => a.iter_mut().for_each(rename),
            _ => {}
        }
    }
    rename(&mut d);
    std::fs::write(&tbl, d.to_string()).unwrap();
    let r = report(&cli(&["check", &root.display().to_string()]));
    let rules: Vec<&String> = r.iter().flat_map(|(_, e)| e).collect();
    assert!(rules.iter().any(|x| *x == "ref.script.fieldName"), "{:?}", r);
    assert!(rules.iter().any(|x| *x == "field.inputField.missing"), "{:?}", r);

    // the consistent way: display name with an explicit dbfield
    let t2 = root.join("P/data/tables/ods/ods_c.tbl");
    let o = cli(&["new", "script", "--table", "ods_c", "--extract", "true",
                  "--fields", "统计年月=stat_month:C:7", "-o", &t2.display().to_string()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let r = report(&cli(&["check", &t2.display().to_string()]));
    assert!(r.iter().all(|(_, e)| e.is_empty()), "{:?}", r);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn schema_config_refs_and_join_field_variant() {
    let doc = schema_document();
    let defs = &doc["schema"]["$defs"];
    let cfg = &defs["payloads"]["config"];
    // previously dangling $refs now resolve to real definitions
    assert_eq!(cfg["groupFields"]["type"], json!("array"));
    assert_eq!(cfg["groupFields"]["items"]["type"], json!("string"));
    assert_eq!(cfg["incrementRange"]["type"], json!("array"));
    assert_eq!(cfg["incrementRange"]["items"]["type"], json!("object"));
    assert_eq!(cfg["filter"]["type"], json!("object"));
    assert_eq!(cfg["filter"]["additionalProperties"], json!(false));
    assert_eq!(
        cfg["filter"]["properties"]["clauses"]["items"]["type"],
        json!("object")
    );
    // per-type config emits $refs into those defs
    let arms = doc["schema"]["oneOf"][0]["properties"]["nodes"]["items"]["oneOf"]
        .as_array()
        .unwrap();
    let arm = |ty: &str| {
        arms.iter()
            .find(|a| a["properties"]["type"]["const"] == json!(ty))
            .unwrap_or_else(|| panic!("no schema arm for {}", ty))
    };
    assert_eq!(
        arm("Agg")["properties"]["config"]["properties"]["filter"]["$ref"],
        json!("#/$defs/payloads/config/filter")
    );
    assert_eq!(
        arm("DbOutput")["properties"]["config"]["properties"]["incrementRange"]["$ref"],
        json!("#/$defs/payloads/config/incrementRange")
    );
    assert_eq!(
        arm("Join")["properties"]["config"]["properties"]["joinConditions"]["$ref"],
        json!("#/$defs/payloads/config/joinConditions")
    );
    // Join fields are name-only closed refs; other nodes keep the full def
    let jf = &arm("Join")["properties"]["fields"]["items"];
    assert_eq!(jf["required"], json!(["name"]));
    assert_eq!(jf["additionalProperties"], json!(false));
    assert!(jf["properties"]["dataType"].is_object());
    assert_eq!(
        defs["field"]["required"],
        json!(["name", "dataType", "isDimension"])
    );
    // originalFields keeps the full field def even on Join
    assert_eq!(
        arm("Join")["properties"]["originalFields"]["items"]["$ref"],
        json!("#/$defs/field")
    );
}

#[test]
fn cli_new_rejects_duplicate_and_existing_targets() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let dir = std::env::temp_dir().join(format!("tblgen-newdup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let d = dir.to_str().unwrap();
    let run = |extra: &[&str]| {
        Command::new(bin)
            .args([
                "new",
                "script",
                "--table",
                "t",
                "--extract",
                "false",
                "--fields",
                "f:C",
                "-o",
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    // every output pair collides under a different spelling -> exit 2,
    // and no output file is created
    for extra in [
        vec![
            &format!("{}/a.tbl", d),
            "--spec-out",
            &format!("{}/./a.tbl", d),
            "--no-script",
        ],
        vec![
            &format!("{}/a.tbl", d),
            "--script-out",
            &format!("{}/sub/../a.tbl", d),
        ],
        vec![
            &format!("{}/a.tbl", d),
            "--spec-out",
            &format!("{}/s.spec.json", d),
            "--script-out",
            &format!("{}/./s.spec.json", d),
        ],
    ] {
        let out = run(&extra);
        assert_eq!(out.status.code(), Some(2), "args {:?}", extra);
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "no files may be written on preflight failure"
        );
    }
    // an existing target (incl. a dangling symlink) is rejected untouched
    let sentinel = dir.join("sentinel.tbl");
    std::fs::write(&sentinel, "keep me").unwrap();
    let out = run(&[
        sentinel.to_str().unwrap(),
        "--spec-out",
        &format!("{}/s.json", d),
        "--no-script",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "keep me");
    #[cfg(unix)]
    {
        let dangling = dir.join("dangling.tbl");
        std::os::unix::fs::symlink(dir.join("missing-target"), &dangling).unwrap();
        let out = run(&[
            dangling.to_str().unwrap(),
            "--spec-out",
            &format!("{}/s2.json", d),
            "--no-script",
        ]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "dangling symlink target must be rejected"
        );
        assert!(dangling.symlink_metadata().is_ok());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// pass-3 regressions: raw/config collisions, lone non-default Output
// ---------------------------------------------------------------------------

#[test]
fn raw_config_key_collision_rejected() {
    // a raw object key duplicating a generated node key or a config key is
    // rejected whether or not the values agree — both land on the same
    // wire object.
    let base = |raw: Value| {
        json!({
            "format": 1,
            "properties": {"extractDataEnabled": false},
            "nodes": [
                {"key":"src","type":"FileInput","alias":"src",
                 "config":{"fileName":"d.xlsx#S"},
                 "raw": raw,
                 "fields":[{"name":"f","dataType":"C","isDimension":true}]},
                {"key":"out","type":"Output","alias":"out","inputs":["src"]}
            ]
        })
    };
    // same value as the generated fields array
    let same = base(json!({"fields":[{"name":"f","dataType":"C"}]}));
    assert!(
        spec_errors(&same)
            .iter()
            .any(|d| d.rule == "structure.node.raw.collision"),
        "{:?}",
        rules(&spec_errors(&same))
    );

    // different value — still a collision
    let diff = base(json!({"fields":"bogus"}));
    assert!(
        spec_errors(&diff)
            .iter()
            .any(|d| d.rule == "structure.node.raw.collision"),
        "{:?}",
        rules(&spec_errors(&diff))
    );

    // raw key colliding with a config key
    let cfgcol = base(json!({"fileName":"other.xlsx"}));
    assert!(
        spec_errors(&cfgcol)
            .iter()
            .any(|d| d.rule == "structure.node.raw.collision"),
        "{:?}",
        rules(&spec_errors(&cfgcol))
    );
}

#[test]
fn lone_nondefault_output_rejected() {
    // a single Output carrying an explicit non-"default" id leaves the flow
    // without a default Output -> flow.noDefaultOutput
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"out","type":"Output","id":"myout","alias":"out","inputs":["src"]}
        ]
    });
    assert!(
        spec_errors(&spec)
            .iter()
            .any(|d| d.rule == "flow.noDefaultOutput"),
        "{:?}",
        rules(&spec_errors(&spec))
    );
}

#[test]
fn cli_new_duplicate_under_nonexistent_dirs() {
    let bin = env!("CARGO_BIN_EXE_tblgen");
    let dir = std::env::temp_dir().join(format!("tblgen-newsub-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // every target lives under a directory that does not exist yet:
    // preflight must reject the duplicate spellings BEFORE any mkdir/write
    // — even the parent directory itself must not appear.
    let sub = dir.join("no/such/dir");
    let a = format!("{}/a.tbl", sub.display());
    let b = format!("{}/./a.tbl", sub.display());
    let out = Command::new(bin)
        .args([
            "new",
            "script",
            "--table",
            "t",
            "--extract",
            "false",
            "--fields",
            "f:C",
            "-o",
            &a,
            "--spec-out",
            &b,
            "--no-script",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{:?}", out);
    assert!(
        !dir.join("no").exists(),
        "preflight must not create directories: {:?}",
        std::fs::read_dir(&dir).unwrap().collect::<Vec<_>>()
    );
    let _ = std::fs::remove_dir_all(&dir);
}
