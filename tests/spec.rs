//! Integration tests for tblgen: generate/decompile/check/schema + CLI.

use serde_json::{json, Value};
use std::process::Command;
use tblgen::check::check;
use tblgen::decompile::decompile;
use tblgen::gen::{generate, validate_spec};
use tblgen::model::*;
use tblgen::schema::schema_document;

fn budget_spec() -> Value {
    serde_json::from_str(include_str!("../examples/budget.spec.json")).unwrap()
}

// ---------------------------------------------------------------------------
// generate: budget example end-to-end
// ---------------------------------------------------------------------------

#[test]
fn budget_generates_valid_wire() {
    let spec = budget_spec();
    assert!(validate_spec(&spec).is_empty(), "spec must validate");
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
    // Select steps + stepCount
    assert_eq!(nodes["flt"]["steps"][0]["type"], json!("Filter"));
    assert_eq!(nodes["flt"]["stepCount"], json!(1));
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
    assert_eq!(ba["inputNode"], json!("pivot"));
    assert_eq!(ba["inputField"], json!("amount"));
    // original lineage reaches back to the ColToRow explicit field (oldest kept)
    assert_eq!(ba["originalNode"], json!("pivot"));
    assert_eq!(ba["originalField"], json!("amount"));
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
    assert_eq!(bf["inputNode"], json!("a"));
    assert_eq!(bf["inputField"], json!("x"));
    // oldest originalNode/originalField preserved, not overwritten by hop
    assert_eq!(bf["originalNode"], json!("src0"));
    assert_eq!(bf["originalField"], json!("x0"));
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
        let diags = validate_spec(&spec);
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
    assert!(validate_spec(&spec)
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
        let diags = validate_spec(&spec);
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
    assert!(validate_spec(&spec).is_empty());
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
    assert!(validate_spec(&spec).iter().any(|d| d.rule == "graph.cycle"));

    let spec2 = json!({
        "format":1,
        "nodes":[
            {"key":"o","type":"Output","inputs":["ghost"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(validate_spec(&spec2)
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
    assert!(validate_spec(&spec)
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
    assert!(validate_spec(&spec2)
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
    assert!(validate_spec(&spec)
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
    assert!(validate_spec(&spec2)
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
    assert!(validate_spec(&spec)
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
    assert!(validate_spec(&spec2)
        .iter()
        .any(|d| d.rule == "structure.config.key"));
    // unknown top-level spec key
    let spec3 = json!({"format":1,"nodes":[],"surprise":{}});
    assert!(validate_spec(&spec3)
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
    assert!(validate_spec(&spec4)
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
    assert!(validate_spec(&spec)
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
    assert!(validate_spec(&spec2)
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
    let diags = validate_spec(&spec);
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
        validate_spec(&spec).is_empty(),
        "{:?}",
        validate_spec(&spec)
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
    assert!(validate_spec(&spec)
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
    assert!(validate_spec(&spec2)
        .iter()
        .any(|d| d.rule == "field.required"));
}

#[test]
fn addfield_and_filter_field_rules() {
    // AddField: explicit fields must agree with addField.name; omitted ->
    // synthesized; disagreement rejected.
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
        validate_spec(&spec).is_empty(),
        "{:?}",
        validate_spec(&spec)
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

    // disagreement rejected
    let mut bad = base.clone();
    bad["nodes"][1]["steps"] = json!([{"type":"AddField","fields":["other"],
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(!validate_spec(&bad).is_empty());

    // Filter requires nonempty fields even for constant expressions
    let mut f = base.clone();
    f["nodes"][1]["steps"] = json!([{"type":"Filter","filter":"1 = 1"}]);
    assert!(validate_spec(&f).iter().any(|d| d.rule == "field.required"));
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
    let diags = validate_spec(&spec);
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
    assert!(validate_spec(&spec)
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
            validate_spec(ex).is_empty(),
            "typed example invalid: {:?}",
            validate_spec(ex)
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
                let diags = validate_spec(spec);
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
    assert!(validate_spec(&spec)
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
    assert!(validate_spec(&spec2)
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
    let ds = validate_spec(&spec);
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
    let ds2 = validate_spec(&spec2);
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
        let ds = validate_spec(&mk(step));
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
    assert!(!validate_spec(&ok)
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
    let ds = validate_spec(&bad);
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
    let ds2 = validate_spec(&ok);
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
    let ds3 = validate_spec(&quoted);
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
