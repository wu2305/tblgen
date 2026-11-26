//! Regression tests for the dataflow core change: Unicode-aware field-name
//! semantics, shared step name-evolution with collision detection, tightened
//! wire `check` rules, the unified compile path and emission defaults.
//!
//! Fixtures live in `tests/fixtures/` (see PROVENANCE.md); nothing here may
//! reference developer-local paths.

use serde_json::{json, Value};
use tblgen::check::check;
use tblgen::decompile::decompile;
use tblgen::gen::{generate, generate_with_diagnostics, validate_spec};
use tblgen::model::Diag;

fn fixture(rel: &str) -> Value {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), rel);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
        .unwrap_or_else(|e| panic!("fixture {}: {}", path, e))
}

fn cx(name: &str) -> Value {
    fixture(&format!("cx/{}", name))
}

fn spec_errors(spec: &Value) -> Vec<Diag> {
    validate_spec(spec)
        .into_iter()
        .filter(|d| !d.is_warning())
        .collect()
}

fn rules(diags: &[Diag]) -> Vec<String> {
    diags.iter().map(|d| d.rule.clone()).collect()
}

fn wire_errors(tbl: &Value) -> Vec<Diag> {
    check(tbl).into_iter().filter(|d| !d.is_warning()).collect()
}

fn assert_single_diag<'a>(diags: &'a [Diag], rule: &str, path: &str, warning: bool) -> &'a Diag {
    let matches: Vec<_> = diags
        .iter()
        .filter(|d| d.rule == rule && d.path == path)
        .collect();
    assert_eq!(matches.len(), 1, "{rule} @ {path}: {diags:?}");
    assert_eq!(matches[0].is_warning(), warning);
    matches[0]
}

fn node<'a>(tbl: &'a Value, key: &str) -> &'a Value {
    &tbl["dataFlow"]["nodes"][key]
}

fn field<'a>(tbl: &'a Value, node_key: &str, name: &str) -> &'a Value {
    node(tbl, node_key)["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("node {} has no field {}", node_key, name))
}

fn names_of(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn top_names(tbl: &Value, key: &str) -> Vec<String> {
    names_of(&tbl[key])
}

// ---------------------------------------------------------------------------
// cx fixtures: name semantics + id ownership
// ---------------------------------------------------------------------------

#[test]
fn removefield_matches_case_insensitively() {
    // Java QueryImpl#getField/renameField compare uppercased names: a
    // RemoveField operand "sku" must remove the declared "SKU".
    let spec = cx("removefield_case.spec.json");
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    assert_eq!(top_names(&tbl, "dimensions"), vec!["ID".to_string()]);
    // empty partitions are omitted entirely (emission default)
    assert!(tbl.get("measures").is_none());
    assert_eq!(names_of(&node(&tbl, "sel")["fields"]), vec!["ID"]);
}

#[test]
fn refactor_rename_collision_is_rejected() {
    // SKU -> ID collides with the existing ID field. The diagnostic must
    // point into the step payload on both APIs.
    let spec = cx("rename_collision.spec.json");
    let ds = spec_errors(&spec);
    assert!(
        ds.iter()
            .any(|d| d.path.starts_with("/nodes/1/steps/0") && d.rule == "field.duplicate"),
        "expected field.duplicate at /nodes/1/steps/0: {:?}",
        ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
    );
    assert!(generate(&spec).is_err());
}

#[test]
fn case_only_self_rename_is_allowed() {
    // Renaming a field to its own name in a different case is not a
    // collision (Java replaces the entry in place).
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"ID","dataType":"C","isDimension":true},
                       {"name":"SKU","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "steps":[{"type":"Refactor","fields":["sku"],"refactor":{"rename":"Sku"}}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).unwrap();
    assert_eq!(names_of(&node(&tbl, "sel")["fields"]), vec!["ID", "Sku"]);
}

#[test]
fn addfield_name_collision_is_rejected() {
    // AddField whose name already exists (case-insensitively) is a
    // collision — even when a later step would remove it again.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"ID","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "steps":[
                {"type":"AddField","addField":{"name":"id","dataType":"C","isDimension":true,"exp":"'x'"}},
                {"type":"RemoveField","fields":["id"]}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    let ds = spec_errors(&spec);
    assert!(
        ds.iter()
            .any(|d| d.rule == "field.duplicate" && d.path.contains("/steps/0/")),
        "{:?}",
        ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
    );
}

#[test]
fn unicode_names_fold_like_java() {
    // Java uppercases with the default locale-insensitive String.toUpperCase;
    // German ß -> SS must participate in comparisons and collisions.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"Straße","dataType":"C","isDimension":true},
                       {"name":"ID","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "steps":[{"type":"RemoveField","fields":["STRASSE"]}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    let tbl = generate(&spec).expect("generate");
    // "STRASSE" folds to Straße's uppercase: Straße is removed, ID remains.
    assert_eq!(
        names_of(&node(&tbl, "sel")["fields"]),
        vec!["ID".to_string()]
    );
}

#[test]
fn conflicting_node_ids_are_rejected() {
    // typed node id "shared" vs rawNode key/body.id "shared": one owner map
    // must reject the collision on both APIs.
    let spec = cx("id_collision.spec.json");
    let ds = spec_errors(&spec);
    assert!(
        ds.iter().any(|d| d.rule.contains("duplicate")),
        "expected a duplicate-id diagnostic: {:?}",
        ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
    );
    assert!(generate(&spec).is_err());
}

#[test]
fn raw_default_output_partitions_top_level() {
    // the default Output may be a rawNode: its declared fields still drive
    // the top-level dimensions/measures partition.
    let spec = cx("raw_default_output.spec.json");
    let tbl = generate(&spec).expect("generate");
    assert_eq!(top_names(&tbl, "dimensions"), vec!["ID".to_string()]);
    assert!(tbl.get("measures").is_none());
}

// ---------------------------------------------------------------------------
// join conditions: aliases, not keys; clause qualifiers
// ---------------------------------------------------------------------------

fn join_alias_spec(left_table: &str, right_table: &str, l_exp: &str, r_exp: &str) -> Value {
    json!({
        "format": 1,
        "version": "4.19.0",
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"l","type":"FileInput","alias":"left","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"ID","dataType":"C","isDimension":true},
                       {"name":"SKU","dataType":"C","isDimension":true}]},
            {"key":"r","type":"FileInput","alias":"right","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"ID","dataType":"C","isDimension":true},
                       {"name":"SKU","dataType":"C","isDimension":true}]},
            {"key":"j","type":"Join","alias":"j","inputs":["l","r"],
             "config":{"joinConditions":[{
                "joinType":"LeftJoin","leftTable":left_table,"rightTable":right_table,
                "clauses":[{"leftExp":l_exp,"operator":"=","rightExp":r_exp}]
             }]},
             "fields":[{"name":"ID","dataType":"C","isDimension":true,
                        "inputNode":"left","inputField":"ID"}]},
            {"key":"out","type":"Output","alias":"out","inputs":["j"]}
        ]
    })
}

#[test]
fn join_condition_must_name_input_aliases() {
    // leftTable/rightTable resolve against node ALIASES only — the spec key
    // "l" is not an alias of the node aliased "left".
    let spec = cx("join_key_alias.spec.json");
    let ds = spec_errors(&spec);
    assert!(
        ds.iter().any(|d| d.rule == "join.condition.alias"),
        "expected join.condition.alias: {:?}",
        ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
    );
    assert!(generate(&spec).is_err());
}

#[test]
fn join_clause_qualifier_must_be_an_alias() {
    // alias-correct tables but clause expressions qualified by spec KEYS are
    // rejected: Java resolves [x].[y] against node aliases.
    let bad = join_alias_spec("left", "right", "[l].[ID]", "[r].[ID]");
    let ds = spec_errors(&bad);
    assert!(
        ds.iter().any(|d| d.rule == "join.condition.qualifier"),
        "expected join.condition.qualifier: {:?}",
        ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
    );

    // fully alias-qualified expressions validate and generate.
    let ok = join_alias_spec("left", "right", "[left].[ID]", "[right].[ID]");
    assert!(spec_errors(&ok).is_empty(), "{:?}", spec_errors(&ok));
    let tbl = generate(&ok).expect("generate");
    assert_eq!(names_of(&node(&tbl, "j")["fields"]), vec!["ID"]);
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    let mut expression = ok;
    expression["nodes"][2]["config"]["joinConditions"][0]["clauses"] =
        json!([{"exp":"[left].[ID] = [right].[ID]"}]);
    assert!(generate(&expression).is_ok());

    // 日期部件等多段引用只按链首校验别名，点号两侧允许空白。
    for (part, bad_part) in [
        ("[left].[ID].[年]", "[l].[ID].[年]"),
        ("[left] . [ID].\t[年]", "[l] . [ID].\t[年]"),
    ] {
        let spec = join_alias_spec("left", "right", part, "[right].[ID]");
        assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
        let mut tbl = generate(&spec).expect("生成应成功");
        assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
        let bad_spec = join_alias_spec("left", "right", bad_part, "[right].[ID]");
        assert!(spec_errors(&bad_spec)
            .iter()
            .any(|d| d.rule == "join.condition.qualifier"));
        tbl["dataFlow"]["nodes"]["j"]["joinConditions"][0]["clauses"][0]["leftExp"] = json!(bad_part);
        assert!(wire_errors(&tbl)
            .iter()
            .any(|d| d.rule == "join.condition.qualifier"));
    }
}

#[test]
fn join_condition_diagnostics_follow_the_emitted_aliases_and_map_back_to_config() {
    let mut valid = join_alias_spec("LEFT", "right", "[left].[ID]", "[RIGHT].[ID]");
    valid["nodes"][2]["id"] = json!("j/~wire");
    valid["nodes"][2]["config"]["joinConditions"][0]["exp"] = json!("[LEFT].[ID] = [RIGHT].[ID]");
    let valid_wire = generate(&valid).unwrap();
    for (tail, value, rule) in [
        ("/0/leftTable", json!("l"), "join.condition.alias"),
        (
            "/0/clauses/0/leftExp",
            json!("[unknown].[ID]"),
            "join.condition.qualifier",
        ),
        (
            "/0/exp",
            json!("[l].[ID] = [right].[ID]"),
            "join.condition.qualifier",
        ),
        (
            "/0/exp",
            json!("[unknown].[ID] = [right].[ID]"),
            "join.condition.qualifier",
        ),
        ("", json!([]), "join.conditions.count"),
    ] {
        let spec_path = format!("/nodes/2/config/joinConditions{tail}");
        let wire_path = format!("/dataFlow/nodes/j~1~0wire/joinConditions{tail}");
        let (mut spec, mut wire) = (valid.clone(), valid_wire.clone());
        *spec.pointer_mut(&spec_path).unwrap() = value.clone();
        *wire.pointer_mut(&wire_path).unwrap() = value;
        let (generated, diags) = generate_with_diagnostics(&spec);
        assert!(generated.is_none());
        assert_eq!(
            diags.iter().filter(|d| d.rule == rule).count(),
            1,
            "{diags:?}"
        );
        let spec_diag = assert_single_diag(&diags, rule, &spec_path, false);
        let wire_diags = check(&wire);
        let wire_diag = assert_single_diag(&wire_diags, rule, &wire_path, false);
        assert_eq!(spec_diag.message, wire_diag.message);
        assert_eq!(spec_diag.source, wire_diag.source);
        assert!(!spec_diag.source.is_empty());
    }
}

// ---------------------------------------------------------------------------
// AddField fields operand contract
// ---------------------------------------------------------------------------

#[test]
fn addfield_fields_operand_passthrough() {
    let base = || {
        json!({
            "format": 1,
            "properties": {"extractDataEnabled": false},
            "nodes": [
                {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
                 "fields":[{"name":"f","dataType":"C","isDimension":true}]},
                {"key":"sel","type":"Select","alias":"sel","inputs":["src"]},
                {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
            ]
        })
    };

    // omitted -> synthesized as [addField.name]
    let mut spec = base();
    spec["nodes"][1]["steps"] = json!([{"type":"AddField",
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).unwrap();
    assert_eq!(node(&tbl, "sel")["steps"][0]["fields"], json!(["n1"]));
    assert_eq!(names_of(&node(&tbl, "sel")["fields"]), vec!["f", "n1"]);

    // explicitly empty -> rejected on both APIs
    let mut empty = base();
    empty["nodes"][1]["steps"] = json!([{"type":"AddField","fields":[],
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(spec_errors(&empty)
        .iter()
        .any(|d| d.rule == "field.required"));

    // non-empty operand is the platform's own selection — preserved
    // verbatim even when it differs from addField.name
    let mut other = base();
    other["nodes"][1]["steps"] = json!([{"type":"AddField","fields":["other"],
        "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[f]"}}]);
    assert!(spec_errors(&other).is_empty(), "{:?}", spec_errors(&other));
    let tbl = generate(&other).unwrap();
    assert_eq!(node(&tbl, "sel")["steps"][0]["fields"], json!(["other"]));

    // wire side: AddField without a non-empty fields operand is an error
    let mut wire = tbl.clone();
    wire["dataFlow"]["nodes"]["sel"]["steps"][0]
        .as_object_mut()
        .unwrap()
        .remove("fields");
    assert!(
        wire_errors(&wire)
            .iter()
            .any(|d| d.rule == "structure.step.fields"),
        "{:?}",
        rules(&wire_errors(&wire))
    );
    let mut wire2 = tbl.clone();
    wire2["dataFlow"]["nodes"]["sel"]["steps"][0]["fields"] = json!([]);
    assert!(wire_errors(&wire2)
        .iter()
        .any(|d| d.rule == "structure.step.fields"));
}

// ---------------------------------------------------------------------------
// Select explicit fields: simulated projection
// ---------------------------------------------------------------------------

#[test]
fn select_explicit_fields_project_lineage() {
    // explicit fields keep declared order/presentation but lineage (input,
    // exp, origin) is enforced from the step simulation — never trusted.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true},
                       {"name":"g","dataType":"N","isDimension":false}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "steps":[{"type":"AddField","addField":{"name":"h","dataType":"C",
                        "isDimension":true,"exp":"[f]"}}],
             "fields":[
                {"name":"h","dataType":"C","isDimension":true,"desc":"declared",
                 "inputField":"bogus","inputNode":"bogus","originalNode":"bogus",
                 "originalField":"bogus"},
                {"name":"g","dataType":"N","isDimension":false,"desc":"second"},
                {"name":"f","dataType":"C","isDimension":true}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let fs = node(&tbl, "sel")["fields"].as_array().unwrap();
    // declared order preserved: h, g, f
    let got: Vec<&str> = fs.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(got, vec!["h", "g", "f"]);
    // AddField payload drives exp; declared lineage keys are overridden
    assert_eq!(fs[0]["exp"], json!("[f]"));
    assert_eq!(fs[0]["desc"], json!("declared"));
    assert!(
        fs[0].get("originalNode").is_none(),
        "computed field has no origin"
    );
    // passthrough fields keep simulated lineage
    assert_eq!(fs[1]["inputField"], json!("g"));
    assert!(
        fs[1].get("inputNode").is_none(),
        "Select never emits inputNode"
    );
    assert_eq!(fs[1]["originalNode"], json!("src"));
    assert_eq!(fs[1]["originalField"], json!("g"));
    assert_eq!(fs[1]["desc"], json!("second"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn select_explicit_fields_multiset_semantics() {
    let field = |name: &str| json!({"name": name, "dataType": "C", "isDimension": true});
    let mk = |src: &[&str], decl: &[&str]| {
        json!({
            "format": 1,
            "properties": {"extractDataEnabled": false},
            "nodes": [
                {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
                 "fields": src.iter().map(|n| field(n)).collect::<Vec<_>>()},
                {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
                 "fields": decl.iter().map(|n| field(n)).collect::<Vec<_>>()},
                {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
            ]
        })
    };

    for decl in [&["ID", "SKU", "Amount"][..], &["Amount", "SKU", "ID"][..]] {
        let spec = mk(&["ID", "SKU", "Amount"], decl);
        assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
        let tbl = generate(&spec).expect("generate");
        let got: Vec<&str> = node(&tbl, "sel")["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(got, decl);
        assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
    }

    // Folded duplicates are a multiset: replacing one SKU occurrence with a
    // second ID changes the counts even though every name still exists.
    let bad = mk(&["ID", "SKU", "sku"], &["ID", "SKU", "ID"]);
    let ds = spec_errors(&bad);
    assert!(
        ds.iter()
            .any(|d| d.rule == "field.mismatch" && d.path == "/nodes/1/fields"),
        "{:?}",
        ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
    );
    assert!(generate(&bad).is_err());

    // ß and SS collide only after Unicode uppercasing; both occurrences must
    // be counted, and the declared reverse order still wins.
    let uni = mk(&["Straße", "STRASSE", "ID"], &["id", "strasse", "STRASSE"]);
    assert!(spec_errors(&uni).is_empty(), "{:?}", spec_errors(&uni));
    let tbl = generate(&uni).expect("generate unicode multiset");
    let got: Vec<&str> = node(&tbl, "sel")["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["name"].as_str().unwrap())
        .collect();
    assert_eq!(got, vec!["id", "strasse", "STRASSE"]);
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn transform_is_derivable_and_rename_after_add_works() {
    // TransformToCalculatedField preserves names (Java edits the field's
    // expression in place); an AddField without a fields operand is still
    // synthesized when a Transform follows.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"m","dataType":"N","isDimension":false},
                       {"name":"d","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],"steps":[
                {"type":"AddField","addField":{"name":"t","dataType":"N",
                   "isDimension":false,"exp":"[m] * 2"}},
                {"type":"TransformToCalculatedField","fields":["m"],
                 "transformFieldInfo":{"exp":"[m] + 1","dataType":"N"}},
                {"type":"Refactor","fields":["t"],"refactor":{"rename":"t2"}}
            ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let fs = node(&tbl, "sel")["fields"].as_array().unwrap();
    let names: Vec<&str> = fs.iter().map(|f| f["name"].as_str().unwrap()).collect();
    // simulated order: input order preserved, AddField result appended,
    // Refactor rename applied in place
    assert_eq!(names, vec!["m", "d", "t2"]);
    let m = fs.iter().find(|f| f["name"] == "m").unwrap();
    assert_eq!(m["exp"], json!("[m] + 1"));
    assert!(m.get("originalNode").is_none(), "computed -> no origin");
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

// ---------------------------------------------------------------------------
// Union: spec-side map resolution + wire-side key checks
// ---------------------------------------------------------------------------

fn union_spec(map: Value, fields: Value) -> Value {
    json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"a","type":"FileInput","alias":"a","config":{"fileName":"x.xlsx#S"},
             "fields":[{"name":"P1","dataType":"C","isDimension":true},
                       {"name":"P2","dataType":"N","isDimension":false}]},
            {"key":"b","type":"FileInput","alias":"b","config":{"fileName":"y.xlsx#S"},
             "fields":[{"name":"Q1","dataType":"C","isDimension":true},
                       {"name":"Q2","dataType":"N","isDimension":false}]},
            {"key":"u","type":"Union","alias":"u","inputs":["a","b"],
             "config":{"unionType":"UnionAll","unionMapArray":map},
             "fields":fields},
            {"key":"out","type":"Output","alias":"out","inputs":["u"]}
        ]
    })
}

#[test]
fn union_map_first_nonempty_input_wins() {
    // values[0] empty -> the map key is anchored on input b (index 1); the
    // emitted inputNode/inputField must be exactly that canonical pair.
    let spec = union_spec(
        json!([{"values":["","Q1"],"visible":true},
               {"values":["","Q2"],"visible":true}]),
        json!([{"name":"X1","dataType":"C","isDimension":true,"inputField":"Q1","inputNode":"b"},
               {"name":"X2","dataType":"N","isDimension":false,"inputField":"Q2","inputNode":"b"}]),
    );
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let f1 = field(&tbl, "u", "X1");
    assert_eq!(f1["inputNode"], json!("b"));
    assert_eq!(f1["inputField"], json!("Q1"));
    assert_eq!(f1["originalNode"], json!("b"));
    assert_eq!(f1["originalField"], json!("Q1"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn union_map_renamed_output_via_inputfield() {
    // output name may differ from every input column: inputField selects the
    // mapped value; the canonical key still anchors on the first populated
    // input slot.
    let spec = union_spec(
        json!([{"values":["P1","Q1"],"visible":true}]),
        json!([{"name":"OUT","dataType":"C","isDimension":true,
                "inputField":"Q1","inputNode":"b"}]),
    );
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let f = field(&tbl, "u", "OUT");
    assert_eq!(f["inputNode"], json!("a"));
    assert_eq!(f["inputField"], json!("P1"));
    assert_eq!(f["originalNode"], json!("a"));
    assert_eq!(f["originalField"], json!("P1"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn union_map_hidden_rows_and_legacy_no_map() {
    // visible:false rows produce no map key; fields never reference them.
    let spec = union_spec(
        json!([{"values":["P1"],"visible":false},
               {"values":["P2","Q2"],"visible":true}]),
        json!([{"name":"F2","dataType":"N","isDimension":false,"inputField":"P2"}]),
    );
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    assert_eq!(field(&tbl, "u", "F2")["inputField"], json!("P2"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
    assert_eq!(node(&tbl, "u")["unionMapArray"][0]["values"], json!(["P1"]));
    let round_trip = decompile(&tbl).unwrap();
    assert!(round_trip.get("nodes").is_some(), "{round_trip:?}");
    assert_eq!(generate(&round_trip).unwrap(), tbl);

    // Hiding a row does not exempt it from the typed spec's JSON shape.
    let mut missing_values = spec.clone();
    missing_values["nodes"][2]["config"]["unionMapArray"][0] = json!({"visible":false});
    assert_single_diag(
        &spec_errors(&missing_values),
        "field.required",
        "/nodes/2/config/unionMapArray/0/values",
        false,
    );

    // The same short row is an error once visible. Width is diagnosed
    // before Union projection can emit a misleading field lookup error.
    let (mut visible_spec, mut visible_wire) = (spec, tbl);
    visible_spec["nodes"][2]["config"]["unionMapArray"][0]["visible"] = json!(true);
    visible_wire["dataFlow"]["nodes"]["u"]["unionMapArray"][0]["visible"] = json!(true);
    let errors = spec_errors(&visible_spec);
    assert_eq!(errors.len(), 1, "{errors:?}");
    let spec_diag = assert_single_diag(
        &errors,
        "union.mapkey",
        "/nodes/2/config/unionMapArray/0/values",
        false,
    );
    let wire_diags = wire_errors(&visible_wire);
    let wire_diag = assert_single_diag(
        &wire_diags,
        "union.mapkey",
        "/dataFlow/nodes/u/unionMapArray/0/values",
        false,
    );
    assert_eq!(spec_diag.message, wire_diag.message);
    assert!(spec_diag.source.is_empty());
    assert!(!wire_diag.source.is_empty());

    // no unionMapArray at all -> legacy per-input name resolution; no key
    // checks apply.
    let mut legacy = union_spec(
        json!([]),
        json!([{"name":"P1","dataType":"C","isDimension":true}]),
    );
    legacy["nodes"][2]["config"]
        .as_object_mut()
        .unwrap()
        .remove("unionMapArray");
    assert!(
        spec_errors(&legacy).is_empty(),
        "{:?}",
        spec_errors(&legacy)
    );
    let tbl = generate(&legacy).expect("generate");
    assert_eq!(field(&tbl, "u", "P1")["inputField"], json!("P1"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn union_map_errors_on_unmatched_and_duplicate_keys() {
    // a declared field that no visible row can produce -> error
    let spec = union_spec(
        json!([{"values":["P1","Q1"],"visible":true}]),
        json!([{"name":"ZZZ","dataType":"C","isDimension":true}]),
    );
    assert!(spec_errors(&spec).iter().any(|d| d.rule == "union.mapkey"));

    // two rows anchored on the same [alias].[field] key -> error
    let spec2 = union_spec(
        json!([{"values":["P1","Q1"],"visible":true},
               {"values":["P1","Q2"],"visible":true}]),
        json!([{"name":"F","dataType":"C","isDimension":true,"inputField":"P1"}]),
    );
    assert!(spec_errors(&spec2)
        .iter()
        .any(|d| d.rule == "union.mapkey" && d.path.contains("unionMapArray")));
}

#[test]
fn union_map_lookup_aliases_and_secondary_values() {
    // Node keys (x/y) differ from aliases (Alpha/Beta); the second row has an
    // empty first value so its canonical key is anchored on Beta; the third
    // row exercises renamed-value (secondary) resolution back to Alpha.A2.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"x","type":"FileInput","alias":"Alpha","config":{"fileName":"x.xlsx#S"},
             "fields":[{"name":"A1","dataType":"C","isDimension":true},
                       {"name":"A2","dataType":"C","isDimension":true}]},
            {"key":"y","type":"FileInput","alias":"Beta","config":{"fileName":"y.xlsx#S"},
             "fields":[{"name":"B1","dataType":"C","isDimension":true},
                       {"name":"B2","dataType":"C","isDimension":true}]},
            {"key":"u","type":"Union","alias":"u","inputs":["x","y"],
             "config":{"unionType":"UnionAll","unionMapArray":[
                 {"values":["A1",""],"visible":true},
                 {"values":["","B1"],"visible":true},
                 {"values":["A2","B2"],"visible":true}
             ]},
             "fields":[
                 {"name":"OUT_A","dataType":"C","isDimension":true,"inputField":"A1","inputNode":"Alpha"},
                 {"name":"OUT_B","dataType":"C","isDimension":true,"inputField":"B1","inputNode":"Beta"},
                 {"name":"OUT_SECONDARY","dataType":"C","isDimension":true,"inputField":"B2","inputNode":"Beta"}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["u"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    assert_eq!(field(&tbl, "u", "OUT_A")["inputNode"], json!("Alpha"));
    assert_eq!(field(&tbl, "u", "OUT_A")["inputField"], json!("A1"));
    assert_eq!(field(&tbl, "u", "OUT_B")["inputNode"], json!("Beta"));
    assert_eq!(field(&tbl, "u", "OUT_B")["inputField"], json!("B1"));
    assert_eq!(
        field(&tbl, "u", "OUT_SECONDARY")["inputNode"],
        json!("Alpha")
    );
    assert_eq!(field(&tbl, "u", "OUT_SECONDARY")["inputField"], json!("A2"));
    assert_eq!(
        field(&tbl, "u", "OUT_SECONDARY")["originalNode"],
        json!("Alpha")
    );
    assert_eq!(
        field(&tbl, "u", "OUT_SECONDARY")["originalField"],
        json!("A2")
    );
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // A visible passthrough field that no canonical or secondary value can
    // produce is still union.mapkey, in declared order.
    let mut bad = spec.clone();
    bad["nodes"][2]["fields"] = json!([
        {"name":"OK","dataType":"C","isDimension":true,"inputField":"A1","inputNode":"Alpha"},
        {"name":"MISSING","dataType":"C","isDimension":true,"inputField":"NOPE","inputNode":"Beta"}
    ]);
    let ds = spec_errors(&bad);
    assert!(
        ds.iter()
            .any(|d| d.rule == "union.mapkey" && d.path.ends_with("/fields/1")),
        "{:?}",
        rules(&ds)
    );
}

#[test]
fn union_wire_requires_map_key_for_linked_fields() {
    // hand-built wire: mutate the generated union so a field's
    // inputFieldExp misses every map key -> hard error (Java NPEs on
    // values.get).
    let spec = union_spec(
        json!([{"values":["P1","Q1"],"visible":true},
               {"values":["P2","Q2"],"visible":true}]),
        json!([{"name":"F1","dataType":"C","isDimension":true,"inputField":"P1"},
               {"name":"F2","dataType":"N","isDimension":false,"inputField":"P2"}]),
    );
    let mut tbl = generate(&spec).unwrap();
    assert!(wire_errors(&tbl).is_empty());

    // break the direct key: inputNode no longer matches the row anchor
    tbl["dataFlow"]["nodes"]["u"]["fields"][0]["inputNode"] = json!("bogus");
    let ds = wire_errors(&tbl);
    assert!(
        ds.iter().any(|d| d.rule == "union.mapkey"),
        "{:?}",
        rules(&ds)
    );

    // bare inputField (no inputNode) also misses "[a].[P1]"-style keys
    let mut tbl2 = generate(&spec).unwrap();
    tbl2["dataFlow"]["nodes"]["u"]["fields"][0]
        .as_object_mut()
        .unwrap()
        .remove("inputNode");
    assert!(wire_errors(&tbl2).iter().any(|d| d.rule == "union.mapkey"));

    // malformed map shapes are hard errors, not silent skips
    for bad_map in [
        json!({"not":"array"}),
        json!(["x"]),
        json!([{"values":"nope"}]),
        json!([{"values":["P1",5]}]),
        json!([{"values":["P1"]}]), // width < inputs
    ] {
        let mut t = generate(&spec).unwrap();
        t["dataFlow"]["nodes"]["u"]["unionMapArray"] = bad_map.clone();
        assert!(
            !wire_errors(&t).is_empty(),
            "malformed unionMapArray must error: {:?}",
            bad_map
        );
    }
}

// ---------------------------------------------------------------------------
// Wire check: step evolution collisions + final-set comparison
// ---------------------------------------------------------------------------

fn select_wire(steps: Value, fields: Value) -> Value {
    json!({"dataFlow":{"nodes":{
        "src":{"id":"src","type":"FileInput","alias":"src","fileName":"x.xlsx#S",
               "fields":[{"name":"ID","dataType":"C","isDimension":true},
                         {"name":"SKU","dataType":"C","isDimension":true}]},
        "sel":{"id":"sel","type":"Select","alias":"sel","inputNodes":["src"],
               "fields":fields,"steps":steps},
        "default":{"id":"default","type":"Output","alias":"out","inputNodes":["sel"],
                   "fields":[]}
    }}})
}

#[test]
fn wire_step_name_collisions_are_errors() {
    let f = json!([{"name":"ID","dataType":"C","isDimension":true},
                   {"name":"SKU","dataType":"C","isDimension":true}]);
    // rename SKU -> ID collides
    let w = select_wire(
        json!([{"type":"Refactor","fields":["SKU"],"refactor":{"rename":"ID"}}]),
        f.clone(),
    );
    assert!(wire_errors(&w).iter().any(|d| d.rule == "field.duplicate"));

    // AddField naming an existing field: Java skips the step (no throw),
    // so the wire check only warns
    let w2 = select_wire(
        json!([{"type":"AddField","fields":["n"],
                "addField":{"name":"id","dataType":"C","isDimension":true,"exp":"'x'"}}]),
        f.clone(),
    );
    assert!(wire_errors(&w2).is_empty(), "{:?}", wire_errors(&w2));
    assert!(check(&w2).iter().any(|d| d.rule == "warn.field.duplicate"));

    // case-only self rename stays legal
    let w3 = select_wire(
        json!([{"type":"Refactor","fields":["sku"],"refactor":{"rename":"Sku"}}]),
        json!([{"name":"ID","dataType":"C","isDimension":true},
               {"name":"Sku","dataType":"C","isDimension":true}]),
    );
    assert!(wire_errors(&w3).is_empty(), "{:?}", wire_errors(&w3));
}

#[test]
fn wire_select_duplicate_names_keep_multiset_membership() {
    let field = |name: &str| json!({"name":name,"dataType":"C","isDimension":true});
    let source = json!([field("ID"), field("SKU"), field("sku")]);
    let output = json!([field("ID"), field("ABC"), field("sku")]);
    let mk = |steps: Value, fields: Value| {
        json!({"dataFlow":{"nodes":{
            "src":{"id":"src","type":"FileInput","alias":"src","fileName":"x.xlsx#S",
                   "fields":source},
            "sel":{"id":"sel","type":"Select","alias":"sel","inputNodes":["src"],
                   "fields":fields,"steps":steps},
            "default":{"id":"default","type":"Output","alias":"out","inputNodes":["sel"],
                       "fields":[]}
        }}})
    };

    // Refactor renames only the first matching occurrence: the duplicate SKU
    // remains available to later steps.
    let renamed = mk(
        json!([
            {"type":"Refactor","fields":["SKU"],"refactor":{"rename":"ABC"}},
            {"type":"ToUppercase","fields":["sku"]}
        ]),
        output,
    );
    let ds = wire_errors(&renamed);
    assert!(
        !ds.iter().any(|d| d.rule == "err.dataflow.stepField.notfound"),
        "{:?}",
        rules(&ds)
    );

    // RemoveField still removes every occurrence of the folded name.
    let removed = mk(
        json!([
            {"type":"RemoveField","fields":["SKU"]},
            {"type":"ToUppercase","fields":["sku"]}
        ]),
        json!([field("ID")]),
    );
    let ds = wire_errors(&removed);
    assert!(
        ds.iter().any(|d| d.rule == "err.dataflow.stepField.notfound"),
        "{:?}",
        rules(&ds)
    );
}

#[test]
fn wire_select_final_set_must_match_steps() {
    // provable steps: RemoveField sku -> the declared output must be [ID];
    // a stale declaration keeping SKU is a mismatch.
    let steps = json!([{"type":"RemoveField","fields":["SKU"]}]);
    let good = select_wire(
        steps.clone(),
        json!([{"name":"ID","dataType":"C","isDimension":true}]),
    );
    assert!(wire_errors(&good).is_empty(), "{:?}", wire_errors(&good));

    let stale = select_wire(
        steps,
        json!([{"name":"ID","dataType":"C","isDimension":true},
               {"name":"SKU","dataType":"C","isDimension":true}]),
    );
    let ds = wire_errors(&stale);
    assert!(
        ds.iter().any(|d| d.rule == "field.mismatch"),
        "{:?}",
        rules(&ds)
    );

    // multiplicity matters: a duplicated declared name is a mismatch too
    let dup = select_wire(
        json!([]),
        json!([{"name":"ID","dataType":"C","isDimension":true},
               {"name":"ID","dataType":"C","isDimension":true},
               {"name":"SKU","dataType":"C","isDimension":true}]),
    );
    assert!(wire_errors(&dup).iter().any(|d| d.rule == "field.mismatch"));
}

#[test]
fn wire_top_partition_must_match_default_output() {
    // default Output fields -> top-level dimensions/measures must agree.
    let mk = |dims: Value, meas: Value| {
        json!({"version":"4.19.0",
        "properties":{"modelDataType":"DataFlow"},
        "dimensions":dims,"measures":meas,
        "dataFlow":{"nodes":{
            "src":{"id":"src","type":"FileInput","alias":"src","fileName":"x",
                   "fields":[{"name":"ID","dataType":"C","isDimension":true},
                             {"name":"AMT","dataType":"N","isDimension":false}]},
            "default":{"id":"default","type":"Output","alias":"out","inputNodes":["src"],
                       "fields":[{"name":"ID","dataType":"C","isDimension":true},
                                 {"name":"AMT","dataType":"N","isDimension":false}]}
        }}})
    };
    // matching -> clean
    let ok = mk(
        json!([{"name":"ID","dataType":"C","isDimension":true}]),
        json!([{"name":"AMT","dataType":"N","isDimension":false}]),
    );
    assert!(wire_errors(&ok).is_empty(), "{:?}", wire_errors(&ok));

    // swapped roles -> stale Output copy: warning (the loader replaces
    // Output fields with the top-level lists)
    let bad = mk(
        json!([{"name":"AMT","dataType":"N","isDimension":false}]),
        json!([{"name":"ID","dataType":"C","isDimension":true}]),
    );
    assert!(wire_errors(&bad).is_empty(), "{:?}", wire_errors(&bad));
    assert!(check(&bad)
        .iter()
        .any(|d| d.rule == "warn.flow.output.partition"));

    // both empty while the Output carries fields -> error
    let missing = mk(json!([]), json!([]));
    assert!(wire_errors(&missing)
        .iter()
        .any(|d| d.rule == "flow.output.partition"));
}

// ---------------------------------------------------------------------------
// filter / incrementRange closed kinds (spec + wire)
// ---------------------------------------------------------------------------

#[test]
fn filter_and_incrementrange_are_structured() {
    let base = || {
        json!({
            "format": 1,
            "properties": {"extractDataEnabled": false},
            "nodes": [
                {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
                 "fields":[{"name":"f","dataType":"C","isDimension":true}]},
                {"key":"sel","type":"Select","alias":"sel","inputs":["src"]},
                {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
            ]
        })
    };

    // typed spec admits `filter` only on Agg (the platform writes it there);
    // the wire checker still validates the shape on every node.
    let agg = |filter: Value| {
        json!({"key":"agg","type":"Agg","alias":"agg","inputs":["sel"],
               "fields":[{"name":"f","dataType":"C","isDimension":true}],
               "config":{"filter":filter}})
    };
    let with_agg = |filter: Value| {
        let mut s = base();
        s["nodes"].as_array_mut().unwrap().push(agg(filter));
        s["nodes"][2]["inputs"] = json!(["agg"]);
        s
    };

    // valid Condition object on Agg
    let ok = with_agg(json!({"matchAll": true, "enabled": true,
        "clauses": [{"field": "f", "operator": "!=", "value": "NULL"}]}));
    assert!(spec_errors(&ok).is_empty(), "{:?}", spec_errors(&ok));
    let tbl = generate(&ok).unwrap();
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // filter on a non-Agg typed node is not an admitted config key
    let mut sel_filter = base();
    sel_filter["nodes"][1]["config"] = json!({"filter": {"matchAll": true}});
    assert!(spec_errors(&sel_filter)
        .iter()
        .any(|d| d.rule.starts_with("structure.config")));

    // string / scalar filter -> rejected (Java casts to Map); null too on
    // the typed form (wire tolerates null as absence)
    for bad_filter in [json!("f != NULL"), json!(5), json!([{"a":1}]), json!(null)] {
        let s = with_agg(bad_filter.clone());
        assert!(
            spec_errors(&s)
                .iter()
                .any(|d| d.rule.starts_with("structure.config")),
            "filter {:?} must be rejected: {:?}",
            bad_filter,
            rules(&spec_errors(&s))
        );
    }
    // non-object clause items -> rejected
    let s2 = with_agg(json!({"clauses": ["x", null, 3]}));
    assert!(!spec_errors(&s2).is_empty());

    // incrementRange: array of clause objects; scalars rejected. DbOutput
    // needs explicit fields and the flow still needs its default Output.
    let dbo = |ir: Value| {
        json!({"key":"dbo","type":"DbOutput","alias":"dbo","inputs":["sel"],
               "fields":[{"name":"f","dataType":"C","isDimension":true}],
               "config":{"datasource":"d","dbSchema":"s","dbTableName":"t",
                         "incrementRange":ir}})
    };
    let mut ok2 = base();
    ok2["nodes"]
        .as_array_mut()
        .unwrap()
        .push(dbo(json!([{"field":"f","operator":">","value":"0"}])));
    assert!(spec_errors(&ok2).is_empty(), "{:?}", spec_errors(&ok2));
    let mut bad2 = base();
    bad2["nodes"]
        .as_array_mut()
        .unwrap()
        .push(dbo(json!("f > 0")));
    assert!(!spec_errors(&bad2).is_empty());
    let mut bad3 = base();
    bad3["nodes"]
        .as_array_mut()
        .unwrap()
        .push(dbo(json!([{"a":1},"x"])));
    assert!(!spec_errors(&bad3).is_empty());
    // null incrementRange is rejected on the typed form (wire tolerates it)
    let mut bad4 = base();
    bad4["nodes"].as_array_mut().unwrap().push(dbo(json!(null)));
    assert!(!spec_errors(&bad4).is_empty());

    // wire side: same shapes checked on every node
    let wire = json!({"dataFlow":{"nodes":{
        "a":{"id":"a","type":"FileInput","alias":"a","fileName":"x",
            "fields":[{"name":"f","dataType":"C","isDimension":true}],
            "filter":"not-an-object"},
        "default":{"id":"default","type":"Output","alias":"o","inputNodes":["a"],"fields":[]}
    }}});
    assert!(wire_errors(&wire)
        .iter()
        .any(|d| d.rule == "structure.node.filter"));

    let wire2 = json!({"dataFlow":{"nodes":{
        "a":{"id":"a","type":"FileInput","alias":"a","fileName":"x",
            "fields":[{"name":"f","dataType":"C","isDimension":true}],
            "incrementRange":[{"a":1},"x"]},
        "default":{"id":"default","type":"Output","alias":"o","inputNodes":["a"],"fields":[]}
    }}});
    assert!(wire_errors(&wire2)
        .iter()
        .any(|d| d.rule == "structure.node.incrementRange"));
}

// ---------------------------------------------------------------------------
// Join/Union field resolution on explicit declarations
// ---------------------------------------------------------------------------

#[test]
fn join_fields_resolve_with_optional_inputnode() {
    // name-only declarations resolve uniquely across inputs; ambiguous
    // names require inputNode; unresolved partials are rejected.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"l","type":"FileInput","alias":"left","config":{"fileName":"l.xlsx#S"},
             "fields":[{"name":"k","dataType":"C","isDimension":true,"length":20},
                       {"name":"v","dataType":"N","isDimension":false}]},
            {"key":"r","type":"FileInput","alias":"right","config":{"fileName":"r.xlsx#S"},
             "fields":[{"name":"k","dataType":"C","isDimension":true,"length":20},
                       {"name":"w","dataType":"N","isDimension":false}]},
            {"key":"j","type":"Join","alias":"j","inputs":["l","r"],
             "config":{"joinConditions":[{"leftTable":"left","rightTable":"right",
                "joinType":"InnerJoin",
                "clauses":[{"leftExp":"[left].[k]","operator":"=","rightExp":"[right].[k]"}]}]},
             "fields":[
                {"name":"k","inputNode":"left"},               // name-only + alias
                {"name":"v"},                                  // unique across inputs
                {"name":"renamed","inputField":"w","inputNode":"right"}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["j"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let fs = node(&tbl, "j")["fields"].as_array().unwrap();
    let k = fs.iter().find(|f| f["name"] == "k").unwrap();
    assert_eq!(k["inputNode"], json!("left"));
    assert_eq!(k["inputField"], json!("k"));
    assert_eq!(k["dataType"], json!("C")); // merged from predecessor
    assert_eq!(k["length"], json!(20));
    assert_eq!(k["originalNode"], json!("left")); // FileInput is a source
    let v = fs.iter().find(|f| f["name"] == "v").unwrap();
    assert_eq!(v["inputNode"], json!("left"));
    let rn = fs.iter().find(|f| f["name"] == "renamed").unwrap();
    assert_eq!(rn["inputNode"], json!("right"));
    assert_eq!(rn["inputField"], json!("w"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // ambiguous name without inputNode -> error
    let mut amb = spec.clone();
    amb["nodes"][2]["fields"] = json!([{"name":"k"}]);
    assert!(spec_errors(&amb)
        .iter()
        .any(|d| d.rule == "field.ambiguous"));

    // unknown field -> error
    let mut miss = spec.clone();
    miss["nodes"][2]["fields"] = json!([{"name":"ghost"}]);
    assert!(spec_errors(&miss)
        .iter()
        .any(|d| d.rule == "field.notfound"));
}

#[test]
fn join_projection_lookup_multi_input_aliases() {
    // Three inputs whose aliases differ from their keys; duplicate `id`/`shared`
    // columns require inputNode, while a unique column resolves across inputs.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"n1","type":"FileInput","alias":"Alpha","config":{"fileName":"n1.xlsx#S"},
             "fields":[{"name":"id","dataType":"C","isDimension":true},
                       {"name":"shared","dataType":"C","isDimension":true},
                       {"name":"a_only","dataType":"N","isDimension":false}]},
            {"key":"n2","type":"FileInput","alias":"Beta","config":{"fileName":"n2.xlsx#S"},
             "fields":[{"name":"id","dataType":"C","isDimension":true},
                       {"name":"shared","dataType":"C","isDimension":true},
                       {"name":"b_only","dataType":"N","isDimension":false}]},
            {"key":"n3","type":"FileInput","alias":"Gamma","config":{"fileName":"n3.xlsx#S"},
             "fields":[{"name":"id","dataType":"C","isDimension":true},
                       {"name":"shared","dataType":"C","isDimension":true},
                       {"name":"c_only","dataType":"N","isDimension":false}]},
            {"key":"j","type":"Join","alias":"j","inputs":["n1","n2","n3"],
             "config":{"joinConditions":[
                 {"leftTable":"Alpha","rightTable":"Beta","joinType":"InnerJoin",
                  "clauses":[{"leftExp":"[Alpha].[id]","operator":"=","rightExp":"[Beta].[id]"}]},
                 {"leftTable":"Beta","rightTable":"Gamma","joinType":"InnerJoin",
                  "clauses":[{"leftExp":"[Beta].[id]","operator":"=","rightExp":"[Gamma].[id]"}]}
             ]},
             "fields":[
                 {"name":"id","inputNode":"Beta"},
                 {"name":"a_renamed","inputField":"a_only","inputNode":"Alpha"},
                 {"name":"c_only"}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["j"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let id = field(&tbl, "j", "id");
    assert_eq!(id["inputNode"], json!("Beta"));
    assert_eq!(id["inputField"], json!("id"));
    assert_eq!(id["originalNode"], json!("Beta"));
    let renamed = field(&tbl, "j", "a_renamed");
    assert_eq!(renamed["inputNode"], json!("Alpha"));
    assert_eq!(renamed["inputField"], json!("a_only"));
    let c = field(&tbl, "j", "c_only");
    assert_eq!(c["inputNode"], json!("Gamma"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // The same shared column without inputNode stays ambiguous in declared order.
    let mut amb = spec.clone();
    amb["nodes"][3]["fields"] = json!([{"name":"shared"}]);
    let ds = spec_errors(&amb);
    assert!(ds.iter().any(|d| d.rule == "field.ambiguous" && d.path.ends_with("/fields/0")), "{:?}", rules(&ds));
}

// ---------------------------------------------------------------------------
// compile invariant: validate_spec sees wire-only errors
// ---------------------------------------------------------------------------

#[test]
fn validate_spec_covers_wire_only_errors() {
    // a rawNode whose body inputNodes dangle: only the wire check can see it —
    // validate_spec must surface it mapped back to the spec path.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"default","rawNode":{"type":"Output","id":"default","alias":"out",
                "inputNodes":["ghost"],"fields":[]}}
        ]
    });
    let ds = validate_spec(&spec);
    assert!(
        ds.iter().any(|d| d.rule == "graph.danglingInput"),
        "validate_spec must include wire-check errors: {:?}",
        rules(&ds)
    );
    assert!(generate(&spec).is_err());

    // an invalid raw step enum likewise surfaces through validate_spec
    let spec2 = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "fields":[{"name":"f","dataType":"C","isDimension":true}],
             "steps":[{"rawStep":{"type":"NoSuchStep","fields":["f"]}}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    let ds2 = validate_spec(&spec2);
    assert!(
        ds2.iter().any(|d| d.rule == "enum.stepType"),
        "validate_spec must include wire step enum errors: {:?}",
        rules(&ds2)
    );
    assert!(generate(&spec2).is_err());
}

// ---------------------------------------------------------------------------
// experiment.spec.json — full model end-to-end
// ---------------------------------------------------------------------------

#[test]
fn experiment_spec_end_to_end() {
    let spec = fixture("experiment.spec.json");
    let ds = validate_spec(&spec);
    let errs: Vec<_> = ds.iter().filter(|d| !d.is_warning()).collect();
    assert!(errs.is_empty(), "experiment spec must validate: {:?}", errs);

    let tbl = generate(&spec).expect("generate");
    let wire_errs = wire_errors(&tbl);
    assert!(
        wire_errs.is_empty(),
        "generated wire must check clean: {:?}",
        wire_errs
            .iter()
            .map(|d| (&d.path, &d.rule))
            .collect::<Vec<_>>()
    );

    // every AddField step carries fields == [addField.name] (8 total:
    // 7 route Selects + final_sel)
    let nodes = tbl["dataFlow"]["nodes"].as_object().unwrap();
    let mut addfield = 0;
    for (k, n) in nodes {
        for s in n["steps"].as_array().into_iter().flatten() {
            if s["type"] == "AddField" {
                addfield += 1;
                assert_eq!(
                    s["fields"],
                    json!([s["addField"]["name"].clone()]),
                    "node {} AddField fields operand",
                    k
                );
            }
        }
    }
    assert_eq!(addfield, 8, "expected 8 AddField steps");

    // union fields anchor on the first populated map input
    for f in node(&tbl, "union_routes")["fields"].as_array().unwrap() {
        assert_eq!(f["inputNode"], json!("route1_dist"), "{:?}", f);
        assert_eq!(f["inputField"], f["name"], "{:?}", f);
    }
    for f in node(&tbl, "union_person")["fields"].as_array().unwrap() {
        assert_eq!(f["inputNode"], json!("branch_direct"), "{:?}", f);
    }

    // root lineage: intermediate originalNode values are rewritten to the
    // oldest proven source; computed descendants carry no origin at all.
    let jl = field(&tbl, "join_labor", "维修工单号");
    assert_eq!(jl["inputNode"], json!("flt_main"));
    assert_eq!(jl["inputField"], json!("维修工单号"));
    assert_eq!(jl["originalNode"], json!("src_main"));
    assert_eq!(jl["originalField"], json!("维修工单号"));

    let jp = field(&tbl, "join_parts", "维修工单号");
    assert_eq!(jp["originalNode"], json!("src_main"));

    let jf = field(&tbl, "join_final", "维修工单号");
    assert_eq!(
        jf["originalNode"],
        json!("src_main"),
        "join_final rewrites dist_routes"
    );
    let jf_fan = field(&tbl, "join_final", "粉丝ID");
    assert_eq!(jf_fan["originalNode"], json!("src_person"));

    // 养修归类 is AddField-computed in every route Select: no origin may
    // survive any number of identity hops.
    for n in [
        "route1_dist",
        "union_routes",
        "dist_routes",
        "join_final",
        "final_sel",
        "default",
    ] {
        let f = node(&tbl, n)["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["name"] == "养修归类")
            .unwrap_or_else(|| panic!("{} lacks 养修归类", n));
        assert!(
            f.get("originalNode").is_none() && f.get("originalField").is_none(),
            "{} 养修归类 must not carry an origin: {:?}",
            n,
            f
        );
    }
    // UUID 编号 likewise: computed, no origin
    let fh = field(&tbl, "final_sel", "编号");
    assert_eq!(fh["exp"], json!("UUID()"));
    assert!(fh.get("originalNode").is_none());

    // final_sel explicit order + computed exps exactly as declared
    let fs = node(&tbl, "final_sel")["fields"].as_array().unwrap();
    let got: Vec<&str> = fs.iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(
        got,
        vec![
            "编号",
            "粉丝ID",
            "车辆VIN",
            "车牌号",
            "门店",
            "结算确认日期",
            "维修工单号",
            "养修归类",
            "应收金额",
            "实收金额"
        ]
    );
    let ss = node(&tbl, "final_sel")["fields"].as_array().unwrap();
    let _ = ss;
    let real = fs.iter().find(|f| f["name"] == "实收金额").unwrap();
    assert_eq!(
        real["exp"],
        json!("CASE WHEN [实收金额] = NULL THEN 0 ELSE [实收金额] END")
    );
    assert!(real.get("originalNode").is_none());
    // Select fields never carry inputNode
    for f in fs {
        assert!(
            f.get("inputNode").is_none(),
            "Select field carries inputNode: {:?}",
            f
        );
    }
}

// ---------------------------------------------------------------------------
// real server-saved .tbl fixtures: decompile -> generate must be exact
// ---------------------------------------------------------------------------

#[test]
fn real_tbl_fixtures_roundtrip_exactly() {
    for name in ["dataflow_pres.tbl", "dataflow_sql.tbl"] {
        let wire: Value = fixture(name);
        let spec = decompile(&wire)
            .unwrap_or_else(|d| panic!("{} decompile failed: {}", name, d.to_json()));
        let regen = generate(&spec)
            .unwrap_or_else(|d| panic!("{} regenerate failed: {}", name, d.to_json()));
        assert_eq!(
            regen, wire,
            "{} must round-trip as equal JSON (incl. version/lastModifyTime)",
            name
        );
    }
}

// ---------------------------------------------------------------------------
// emission defaults
// ---------------------------------------------------------------------------

#[test]
fn emission_defaults() {
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"out","type":"Output","alias":"out","inputs":["src"]}
        ]
    });
    let tbl = generate(&spec).expect("generate");
    // version defaults to the platform save version
    assert_eq!(tbl["version"], json!("4.19.0"));
    // empty partitions/lists are omitted, not emitted as []
    assert!(tbl.get("measures").is_none(), "empty measures omitted");
    assert_eq!(top_names(&tbl, "dimensions"), vec!["f".to_string()]);
    let out = node(&tbl, "default");
    assert!(
        out.get("measures").is_none(),
        "node measures omitted when empty"
    );
    assert!(out.get("steps").is_none() || out["steps"] == json!([]));
    // a Select with no steps emits no steps key and no stepCount
    let spec2 = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],"steps":[]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    let tbl2 = generate(&spec2).expect("generate");
    let sel = node(&tbl2, "sel");
    assert!(sel.get("steps").is_none(), "empty steps omitted: {:?}", sel);
    assert!(
        sel.get("stepCount").is_none(),
        "stepCount omitted when not exceeded"
    );

    // stepCount is emitted only when the declared count exceeds the real
    // step count (the platform's own bookkeeping)
    let mut spec2b = spec2.clone();
    spec2b["nodes"][1]["config"] = json!({"stepCount": 5});
    let tbl2b = generate(&spec2b).expect("generate");
    assert_eq!(node(&tbl2b, "sel")["stepCount"], json!(5));

    // explicit version is preserved
    let mut spec3 = spec.clone();
    spec3["version"] = json!("4.0.0");
    assert_eq!(generate(&spec3).unwrap()["version"], json!("4.0.0"));
}

// ---------------------------------------------------------------------------
// warn.extract.dbfield
// ---------------------------------------------------------------------------

#[test]
fn extract_dbfield_warns_on_top_fields() {
    // extractDataEnabled=true: every top-level dimension/measure needs an
    // ASCII dbfield; the warning routes to the default Output's field.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": true, "dbTableName": "ads_x"},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"项目","dataType":"C","isDimension":true},
                       {"name":"amt","dataType":"N","isDimension":false,"dbfield":"AMT"}]},
            {"key":"out","type":"Output","alias":"out","inputs":["src"]}
        ]
    });
    let ds = validate_spec(&spec);
    let w = ds.iter().find(|d| d.rule == "warn.extract.dbfield");
    assert!(
        w.is_some(),
        "expected warn.extract.dbfield: {:?}",
        rules(&ds)
    );
    assert!(
        w.unwrap().path.contains("/nodes/"),
        "warning routes to node fields: {}",
        w.unwrap().path
    );

    // wire side fires on the same condition
    let tbl = generate(&spec).unwrap();
    let wd = check(&tbl);
    assert!(wd
        .iter()
        .any(|d| d.rule == "warn.extract.dbfield" && d.path.starts_with("/dimensions")));

    // extractDataEnabled=false -> no warning
    let mut spec2 = spec.clone();
    spec2["properties"]["extractDataEnabled"] = json!(false);
    assert!(!validate_spec(&spec2)
        .iter()
        .any(|d| d.rule == "warn.extract.dbfield"));
}

// ---------------------------------------------------------------------------
// old broken wire outputs: targeted checker diagnostics
// ---------------------------------------------------------------------------

#[test]
fn old_wire_regressions_targeted_errors() {
    // each old cx/*.tbl was emitted by the previous generator with a known
    // defect; check must report the matching targeted diagnostic.
    let cases: &[(&str, &str, &str)] = &[
        (
            "removefield_case.tbl",
            "field.mismatch",
            "/dataFlow/nodes/sel/fields/1/name",
        ),
        (
            "join_key_alias.tbl",
            "join.condition.alias",
            "/dataFlow/nodes/j/joinConditions/0/leftTable",
        ),
        (
            "raw_default_output.tbl",
            "flow.output.partition",
            "/dataFlow/nodes/default/fields/0/name",
        ),
        (
            "rename_collision.tbl",
            "field.duplicate",
            "/dataFlow/nodes/sel/steps/0/refactor/rename",
        ),
    ];
    for (file, rule, path) in cases {
        let ds = check(&fixture(&format!("cx/{}", file)));
        assert!(
            ds.iter().any(|d| d.rule == *rule && d.path == *path),
            "{}: expected {} at {}: {:?}",
            file,
            rule,
            path,
            ds.iter().map(|d| (&d.path, &d.rule)).collect::<Vec<_>>()
        );
    }
    // id_collision: the key/id collision already destroyed a node at gen
    // time; the surviving flow is self-consistent, so the targeted signal
    // is the stale originalNode lineage warning.
    let ds = check(&fixture("cx/id_collision.tbl"));
    assert!(
        ds.iter().any(|d| d.rule == "warn.field.originalNode"),
        "{:?}",
        rules(&ds)
    );
    // the original experiment wire carries the same old defects
    let ds = check(&fixture("ads_owner_repair_history.tbl"));
    assert!(
        ds.iter().any(|d| d.rule == "union.mapkey"),
        "{:?}",
        rules(&ds)
    );
    assert!(
        ds.iter().any(|d| d.rule == "structure.step.fields"),
        "{:?}",
        rules(&ds)
    );
}

// ---------------------------------------------------------------------------
// Union map presence semantics (wire side)
// ---------------------------------------------------------------------------

fn union_wire(map: Value, fields: Value) -> Value {
    let mut u = json!({"id":"u","type":"Union","alias":"u","unionType":"UnionAll",
        "inputNodes":["a","b"],"fields":fields});
    if !map.is_null() {
        u["unionMapArray"] = map;
    }
    json!({"version":"4.19.0","properties":{"modelDataType":"DataFlow"},
    "dimensions":[{"name":"F","dataType":"C","isDimension":true}],
    "dataFlow":{"nodes":{
        "a":{"id":"a","type":"FileInput","alias":"a","fileName":"x",
            "fields":[{"name":"F","dataType":"C","isDimension":true}]},
        "b":{"id":"b","type":"FileInput","alias":"b","fileName":"y",
            "fields":[{"name":"G","dataType":"C","isDimension":true}]},
        "u":u,
        "default":{"id":"default","type":"Output","alias":"o","inputNodes":["u"],
            "fields":[{"name":"F","dataType":"C","isDimension":true}]}
    }}})
}

#[test]
fn union_map_presence_and_row_edge_cases() {
    let f = json!([{"name":"F","dataType":"C","isDimension":true,"inputNode":"a"}]);
    // absent map -> legacy resolution, no key checks
    assert!(wire_errors(&union_wire(Value::Null, f.clone())).is_empty());
    // explicit null -> same legacy path
    let mut w = union_wire(json!(null), f.clone());
    w["dataFlow"]["nodes"]["u"]["unionMapArray"] = json!(null);
    assert!(wire_errors(&w).is_empty());
    // present but EMPTY map still gates live reads -> error
    assert!(wire_errors(&union_wire(json!([]), f.clone()))
        .iter()
        .any(|d| d.rule == "union.mapkey"));
    // only hidden rows -> no keys -> live field still fails
    assert!(wire_errors(&union_wire(
        json!([{"values":["F",null],"visible":false}]),
        f.clone()
    ))
    .iter()
    .any(|d| d.rule == "union.mapkey"));
    // hidden row is skipped BEFORE its values are validated
    let w = union_wire(
        json!([{"values":["F",null]},
               {"values":"malformed","visible":false}]),
        f.clone(),
    );
    assert!(wire_errors(&w).is_empty(), "{:?}", wire_errors(&w));
    // whitespace inputField falls back to the field name for the union map
    // lookup (Java blank) — union.mapkey must NOT fire even though the
    // literal "   " operand separately fails field.inputField.missing
    let fw = json!([{"name":"F","dataType":"C","isDimension":true,
                     "inputNode":"a","inputField":"   "}]);
    let w = union_wire(json!([{"values":["F",null]}]), fw);
    assert!(
        !wire_errors(&w).iter().any(|d| d.rule == "union.mapkey"),
        "blank inputField must fall back to name: {:?}",
        rules(&wire_errors(&w))
    );
    // two visible noncomputed fields claiming the same map key -> error
    let dup = json!([{"name":"F","dataType":"C","isDimension":true,"inputNode":"a"},
                     {"name":"F2","dataType":"C","isDimension":true,
                      "inputNode":"a","inputField":"F"}]);
    let mut w = union_wire(json!([{"values":["F",null]}]), dup);
    w["dimensions"] = json!([{"name":"F","dataType":"C","isDimension":true},
                             {"name":"F2","dataType":"C","isDimension":true}]);
    w["dataFlow"]["nodes"]["default"]["fields"] = json!([
        {"name":"F","dataType":"C","isDimension":true},
        {"name":"F2","dataType":"C","isDimension":true}]);
    assert!(wire_errors(&w)
        .iter()
        .any(|d| d.rule == "union.mapkey" && d.message.contains("share map key")));
}

// ---------------------------------------------------------------------------
// Select simulation: exp/hint preservation, type conversion, raw preds
// ---------------------------------------------------------------------------

#[test]
fn select_simulation_exp_hint_and_type_conversion() {
    // AddField -> Transform -> Refactor rename: final field keeps the
    // transformed exp; the rename step emits an originFieldInfo hint with
    // the step-time name/dataType.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"m","dataType":"N","isDimension":false},
                       {"name":"d","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],"steps":[
                {"type":"AddField","addField":{"name":"t","dataType":"N",
                   "isDimension":false,"exp":"[m] * 2"}},
                {"type":"TransformToCalculatedField","fields":["t"],
                 "transformFieldInfo":{"exp":"[m] * 3","dataType":"N"}},
                {"type":"Refactor","fields":["t"],"refactor":{"rename":"t2"}}
            ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let t2 = field(&tbl, "sel", "t2");
    assert_eq!(
        t2["exp"],
        json!("[m] * 3"),
        "transform exp must survive rename"
    );
    assert!(t2.get("originalNode").is_none());
    // the Refactor step carries the editor hint captured before mutation
    let steps = node(&tbl, "sel")["steps"].as_array().unwrap();
    let hint = &steps[2]["originFieldInfo"];
    assert_eq!(hint["name"], json!("t"));
    assert_eq!(hint["dataType"], json!("N"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // type-converting Refactor requires explicit fields; with them the
    // simulation continues and a later AddField still derives.
    let spec2 = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"m","dataType":"N","isDimension":false},
                       {"name":"d","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "fields":[{"name":"m","dataType":"C","isDimension":false},
                       {"name":"d","dataType":"C","isDimension":true},
                       {"name":"n1","dataType":"N","isDimension":false}],
             "steps":[
                {"type":"Refactor","fields":["m"],
                 "refactor":{"dataType":"C"}},
                {"type":"AddField","fields":["n1"],
                 "addField":{"name":"n1","dataType":"N","isDimension":false,"exp":"[m]"}}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec2).is_empty(), "{:?}", spec_errors(&spec2));
    let tbl2 = generate(&spec2).expect("generate");
    let m = field(&tbl2, "sel", "m");
    assert_eq!(m["dataType"], json!("C"), "type conversion applied");
    assert!(wire_errors(&tbl2).is_empty(), "{:?}", wire_errors(&tbl2));

    // same type conversion WITHOUT explicit fields -> field.required
    let mut spec3 = spec2.clone();
    spec3["nodes"][1].as_object_mut().unwrap().remove("fields");
    assert!(spec_errors(&spec3)
        .iter()
        .any(|d| d.rule == "field.required"));
}

#[test]
fn raw_predecessor_explicit_disjoint_names_rejected() {
    // a rawNode predecessor with KNOWN body fields: a typed Select whose
    // explicit fields are disjoint from them must not silently pass.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"r","rawNode":{"type":"ModelTable","id":"r","alias":"r",
                "moduleTablePath":"m.tbl",
                "fields":[{"name":"A","dataType":"C","isDimension":true}]}},
            {"key":"sel","type":"Select","alias":"sel","inputs":["r"],
             "fields":[{"name":"B","dataType":"C","isDimension":true}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(!spec_errors(&spec).is_empty(), "disjoint names must fail");
}

// ---------------------------------------------------------------------------
// Agg/Distinct arity + lineage rules
// ---------------------------------------------------------------------------

#[test]
fn agg_and_distinct_require_exactly_one_input() {
    let two = |ty: &str| {
        json!({"format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"a","type":"FileInput","alias":"a","config":{"fileName":"x#S"},
             "fields":[{"name":"F","dataType":"C","isDimension":true}]},
            {"key":"b","type":"FileInput","alias":"b","config":{"fileName":"y#S"},
             "fields":[{"name":"F","dataType":"C","isDimension":true}]},
            {"key":"n","type":ty,"alias":"n","inputs":["a","b"]},
            {"key":"out","type":"Output","alias":"out","inputs":["n"]}
        ]})
    };
    for ty in ["Agg", "Distinct"] {
        let spec = two(ty);
        assert!(
            spec_errors(&spec)
                .iter()
                .any(|d| d.rule == "err.dataflow.node.input"),
            "{} with 2 inputs must fail: {:?}",
            ty,
            rules(&spec_errors(&spec))
        );
        // wire side: same arity rule on the emitted shape
        let tbl = json!({"version":"4.19.0","properties":{"modelDataType":"DataFlow"},
        "dimensions":[{"name":"F","dataType":"C","isDimension":true}],
        "dataFlow":{"nodes":{
            "a":{"id":"a","type":"FileInput","alias":"a","fileName":"x",
                "fields":[{"name":"F","dataType":"C","isDimension":true}]},
            "b":{"id":"b","type":"FileInput","alias":"b","fileName":"y",
                "fields":[{"name":"F","dataType":"C","isDimension":true}]},
            "n":{"id":"n","type":ty,"alias":"n","inputNodes":["a","b"],
                "fields":[{"name":"F","dataType":"C","isDimension":true}]},
            "default":{"id":"default","type":"Output","alias":"o","inputNodes":["n"],
                "fields":[{"name":"F","dataType":"C","isDimension":true}]}
        }}});
        assert!(
            wire_errors(&tbl)
                .iter()
                .any(|d| d.rule == "err.dataflow.node.input" && d.path.contains("inputNodes")),
            "{} wire arity: {:?}",
            ty,
            rules(&wire_errors(&tbl))
        );
    }
}

#[test]
fn lineage_source_exp_and_stale_origins() {
    // a source field carrying exp sheds original*; a stale explicit
    // original* on a passthrough is rewritten to the real source pair;
    // computed Select output never keeps inputNode even if declared.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"realias","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"f","dataType":"C","isDimension":true},
                       {"name":"calc","dataType":"C","isDimension":true,
                        "exp":"[f] || 'x'",
                        "originalNode":"bogus","originalField":"bogus"}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "fields":[{"name":"f","dataType":"C","isDimension":true,
                        "originalNode":"stale","originalField":"stale"},
                       {"name":"calc","dataType":"C","isDimension":true,
                        "inputNode":"stale","inputField":"calc"}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let sf = node(&tbl, "src")["fields"].as_array().unwrap();
    let calc = sf.iter().find(|f| f["name"] == "calc").unwrap();
    assert!(
        calc.get("originalNode").is_none() && calc.get("originalField").is_none(),
        "source exp field must shed origin: {:?}",
        calc
    );
    let ff = field(&tbl, "sel", "f");
    assert_eq!(
        ff["originalNode"],
        json!("realias"),
        "stale origin rewritten"
    );
    assert_eq!(ff["originalField"], json!("f"));
    let cf = field(&tbl, "sel", "calc");
    assert!(cf.get("originalNode").is_none(), "computed -> no origin");
    assert!(
        cf.get("inputNode").is_none(),
        "Select field drops inputNode"
    );
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

// ---------------------------------------------------------------------------
// union hidden fields + escape-path suffix recovery + unknown predecessors
// ---------------------------------------------------------------------------

#[test]
fn union_hidden_field_needs_no_map_row() {
    // hidden:true declared fields are never read at runtime: they bypass
    // unionMapArray matching entirely and carry no proven lineage.
    let spec = union_spec(
        json!([{"values":["P1","Q1"],"visible":true}]),
        json!([{"name":"F1","dataType":"C","isDimension":true,"inputField":"P1"},
               {"name":"H","dataType":"C","isDimension":true,"hidden":true}]),
    );
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let h = field(&tbl, "u", "H");
    assert!(
        h.get("originalNode").is_none() && h.get("inputField").is_none(),
        "hidden field carries no proven lineage: {:?}",
        h
    );
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn escape_suffix_recovers_provable_step_metadata() {
    // opaque step -> typed AddField+Transform: the declaration IS the final
    // shape; walking the suffix backwards binds the LATEST transform exp to
    // the AddField-born field.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"m","dataType":"N","isDimension":false}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "fields":[{"name":"x","dataType":"N","isDimension":false}],
             "steps":[
                {"rawStep":{"type":"SplitField","fields":["m"]}},
                {"type":"AddField","fields":["x"],
                 "addField":{"name":"x","dataType":"N","isDimension":false,"exp":"[m]"}},
                {"type":"TransformToCalculatedField","fields":["x"],
                 "transformFieldInfo":{"exp":"[m] * 2","dataType":"N"}}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    let x = field(&tbl, "sel", "x");
    assert_eq!(
        x["exp"],
        json!("[m] * 2"),
        "latest transform wins over addField exp"
    );
    assert!(x.get("originalNode").is_none());
    // no rename hint is guessed through the opaque step
    let steps = node(&tbl, "sel")["steps"].as_array().unwrap();
    assert!(steps.iter().all(|s| s.get("originFieldInfo").is_none()));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // a suffix that provably adds a surviving name the declaration omits
    // is a contradiction, not a silent projection change
    let mut missing = spec.clone();
    missing["nodes"][1]["fields"] = json!([]);
    let ds = spec_errors(&missing);
    assert!(
        ds.iter().any(|d| d.rule == "field.mismatch"),
        "{:?}",
        rules(&ds)
    );

    // a declared field the suffix provably removes is likewise rejected
    let mut removed = spec.clone();
    removed["nodes"][1]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"RemoveField","fields":["x"]}));
    let ds2 = spec_errors(&removed);
    assert!(
        ds2.iter().any(|d| d.rule == "field.mismatch"),
        "{:?}",
        rules(&ds2)
    );

    // Add-then-Remove is consistent with a declaration that omits the name
    let mut killed = spec.clone();
    killed["nodes"][1]["fields"] = json!([]);
    killed["nodes"][1]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"RemoveField","fields":["x"]}));
    assert!(
        spec_errors(&killed).is_empty(),
        "{:?}",
        spec_errors(&killed)
    );

    // A second opaque projection may discard x; only its suffix is provable.
    missing["nodes"][1]["steps"]
        .as_array_mut()
        .unwrap()
        .push(json!({"rawStep":{"type":"SplitField","fields":["x"]}}));
    assert!(generate(&missing).is_ok());
}

#[test]
fn escape_suffix_reverse_rename_binds_birth() {
    // opaque -> AddField x -> Refactor x=>y: the surviving declared name is
    // y; the reverse pass maps it back to x so the AddField birth binds.
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"m","dataType":"N","isDimension":false}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "fields":[{"name":"y","dataType":"N","isDimension":false}],
             "steps":[
                {"rawStep":{"type":"SplitField","fields":["m"]}},
                {"type":"AddField","fields":["x"],
                 "addField":{"name":"x","dataType":"N","isDimension":false,"exp":"[m]"}},
                {"type":"Refactor","fields":["x"],"refactor":{"rename":"y"}}
             ]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    assert_eq!(
        field(&tbl, "sel", "y")["exp"],
        json!("[m]"),
        "renamed AddField keeps its birth exp"
    );
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));
}

#[test]
fn raw_pred_without_fields_explicit_shape() {
    // rawNode predecessor WITHOUT a fields array: the typed node's own
    // declarations ARE the shape (previously field.required).
    let spec = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"r","rawNode":{"type":"ModelTable","id":"r","alias":"r",
                "moduleTablePath":"m.tbl"}},
            {"key":"out","type":"Output","alias":"out","inputs":["r"],
             "fields":[{"name":"A","dataType":"C","isDimension":true}]}
        ]
    });
    assert!(spec_errors(&spec).is_empty(), "{:?}", spec_errors(&spec));
    let tbl = generate(&spec).expect("generate");
    assert_eq!(field(&tbl, "default", "A")["name"], json!("A"));
    assert!(wire_errors(&tbl).is_empty(), "{:?}", wire_errors(&tbl));

    // absent declarations remain an error
    let mut bad = spec.clone();
    bad["nodes"][1].as_object_mut().unwrap().remove("fields");
    assert!(
        spec_errors(&bad).iter().any(|d| d.rule == "field.required"),
        "{:?}",
        rules(&spec_errors(&bad))
    );

    // Select over the same unknown shape: explicit fields plus the suffix
    // pass over ALL steps still binds the AddField birth expression
    let spec2 = json!({
        "format": 1,
        "properties": {"extractDataEnabled": false},
        "nodes": [
            {"key":"r","rawNode":{"type":"ModelTable","id":"r","alias":"r",
                "moduleTablePath":"m.tbl"}},
            {"key":"sel","type":"Select","alias":"sel","inputs":["r"],
             "fields":[{"name":"x","dataType":"N","isDimension":false}],
             "steps":[{"type":"AddField","fields":["x"],
                "addField":{"name":"x","dataType":"N","isDimension":false,"exp":"1+1"}}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]
    });
    assert!(spec_errors(&spec2).is_empty(), "{:?}", spec_errors(&spec2));
    let tbl2 = generate(&spec2).expect("generate");
    assert_eq!(field(&tbl2, "sel", "x")["exp"], json!("1+1"));
    assert!(wire_errors(&tbl2).is_empty(), "{:?}", wire_errors(&tbl2));
}

#[test]
fn original_node_may_name_any_node_and_empty_operands_skip_steps() {
    // the platform writes originalNode as the node a field was born in —
    // here a Select's AddField — so only an unknown alias warns
    let fields = json!([{"name":"ID","dataType":"C","isDimension":true}]);
    let mut w = select_wire(json!([]), fields.clone());
    w["dataFlow"]["nodes"]["sel"]["fields"][0]["originalNode"] = json!("sel");
    assert!(!rules(&check(&w)).contains(&"warn.field.originalNode".to_string()));
    w["dataFlow"]["nodes"]["sel"]["fields"][0]["originalNode"] = json!("ghost");
    assert!(rules(&check(&w)).contains(&"warn.field.originalNode".to_string()));

    // a per-field step without operands is skipped by Java: gen rejects it,
    // check warns
    let w = select_wire(json!([{"type":"ToUppercase","fields":[]}]), fields);
    assert!(rules(&check(&w)).contains(&"warn.step.fields.empty".to_string()));
    let spec = json!({"format":1,"version":"4.19.0","properties":{"extractDataEnabled":false},
        "nodes":[
            {"key":"src","type":"FileInput","alias":"src","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"ID","dataType":"C","isDimension":true}]},
            {"key":"sel","type":"Select","alias":"sel","inputs":["src"],
             "steps":[{"type":"ExcludeEmpty"}]},
            {"key":"out","type":"Output","alias":"out","inputs":["sel"]}
        ]});
    assert!(rules(&spec_errors(&spec)).contains(&"field.required".to_string()));
}

#[test]
fn col_to_row_uses_platform_reference_rules_and_keeps_typed_metadata() {
    let fields = json!([{"name":"header","dataType":"C","isDimension":true}]);
    let valid = json!({
        "format":1, "properties":{"extractDataEnabled":false},
        "nodes":[
            {"key":"src","type":"FileInput","config":{"fileName":"d.xlsx#S"},
             "fields":[{"name":"input","dataType":"C","isDimension":true}]},
            {"key":"pivot","type":"ColToRow","inputs":["src"],"fields":fields,
             "config":{"colToRowInfo":{"headers":["header"],
                                      "valuesArray":[[{"name":"INPUT","dataType":"C"}]]}}},
            {"key":"out","type":"Output","inputs":["pivot"]}
        ]
    });
    let valid_wire = generate(&valid).unwrap();
    for (row, tail, rule, has_platform_source) in [
        (json!([[]]), "/0", "err.dataflow.colToRow.config", true),
        (
            json!([[{"name":"","dataType":"C"}]]),
            "/0/0/name",
            "field.name",
            false,
        ),
        (
            json!([[{"name":"missing","dataType":"C"}]]),
            "/0/0/name",
            "field.notfound",
            true,
        ),
    ] {
        let (mut spec, mut wire) = (valid.clone(), valid_wire.clone());
        let spec_base = "/nodes/1/config/colToRowInfo/valuesArray";
        let wire_base = "/dataFlow/nodes/pivot/colToRowInfo/valuesArray";
        *spec.pointer_mut(spec_base).unwrap() = row.clone();
        *wire.pointer_mut(wire_base).unwrap() = row;
        let errors = spec_errors(&spec);
        assert_eq!(errors.len(), 1, "{errors:?}");
        let spec_diag = assert_single_diag(&errors, rule, &format!("{spec_base}{tail}"), false);
        let wire_diags = wire_errors(&wire);
        let wire_diag = assert_single_diag(&wire_diags, rule, &format!("{wire_base}{tail}"), false);
        assert_eq!(spec_diag.message, wire_diag.message);
        assert!(!wire_diag.source.is_empty());
        assert_eq!(!spec_diag.source.is_empty(), has_platform_source);
        if has_platform_source {
            assert_eq!(spec_diag.source, wire_diag.source);
        }
    }
    for cell in [
        json!({"name":"input"}),
        json!({"name":"input","dataType":""}),
    ] {
        let (mut spec, mut wire) = (valid.clone(), valid_wire.clone());
        spec["nodes"][1]["config"]["colToRowInfo"]["valuesArray"][0][0] = cell.clone();
        wire["dataFlow"]["nodes"]["pivot"]["colToRowInfo"]["valuesArray"][0][0] = cell;
        let errors = spec_errors(&spec);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_single_diag(
            &errors,
            "field.required",
            "/nodes/1/config/colToRowInfo/valuesArray/0/0/dataType",
            false,
        );
        assert!(wire_errors(&wire).is_empty());
    }
    let mut wrong_header = valid;
    wrong_header["nodes"][1]["config"]["colToRowInfo"]["headers"] = json!(["undeclared"]);
    assert_single_diag(
        &spec_errors(&wrong_header),
        "field.name",
        "/nodes/1/config/colToRowInfo/headers/0",
        false,
    );
}

#[test]
fn topology_keeps_hard_errors_and_remaps_platform_warnings_once() {
    let fields = json!([{"name":"ID","dataType":"C","isDimension":true}]);
    let spec = json!({
        "format":1, "properties":{"extractDataEnabled":false},
        "nodes":[
            {"key":"src","id":"source/~wire","type":"FileInput","fields":fields,
             "config":{"fileName":"d.xlsx#S"}},
            {"key":"row","type":"RowToCol","fields":fields},
            {"key":"col","type":"ColToRow","fields":fields},
            {"key":"snap","type":"Snapshot","fields":fields},
            {"key":"upd","type":"Update","fields":fields},
            {"key":"db","type":"DbTable","inputs":["src"],"fields":fields},
            {"key":"sql","type":"Sql","inputs":["src"],"fields":fields},
            {"key":"sel","type":"Select","inputs":["src"]},
            {"key":"out","type":"Output","inputs":["sel"]}
        ]
    });
    let (wire, diags) = generate_with_diagnostics(&spec);
    let wire = wire.unwrap_or_else(|| panic!("{diags:?}"));
    let wire_diags = check(&wire);
    for (index, id, rule) in [
        (1, "row", "warn.warn.node.noInput"),
        (2, "col", "warn.warn.node.noInput"),
        (3, "snap", "warn.warn.node.noInput"),
        (4, "upd", "warn.warn.node.noInput"),
        (5, "db", "warn.editor.dbtable.predecessor"),
        (6, "sql", "warn.editor.sql.predecessor"),
    ] {
        let spec_path = format!("/nodes/{index}/inputs");
        let wire_path = format!("/dataFlow/nodes/{id}/inputNodes");
        let spec_diag = assert_single_diag(&diags, rule, &spec_path, true);
        let wire_diag = assert_single_diag(&wire_diags, rule, &wire_path, true);
        assert_eq!(spec_diag.message, wire_diag.message);
        assert_eq!(spec_diag.source, wire_diag.source);
        assert!(!spec_diag.source.is_empty());
    }
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.rule == "warn.warn.node.noInput")
            .count(),
        4
    );
    assert_eq!(
        diags
            .iter()
            .filter(|d| d.rule.starts_with("warn.editor."))
            .count(),
        2
    );

    let (mut update_spec, mut update_wire) = (spec.clone(), wire.clone());
    update_spec["nodes"][4]["inputs"] = json!(["src", "db"]);
    update_spec["nodes"][4]["raw"] = json!({"updateConditions":[]});
    update_wire["dataFlow"]["nodes"]["upd"]["inputNodes"] = json!(["source/~wire", "db"]);
    update_wire["dataFlow"]["nodes"]["upd"]["updateConditions"] = json!([]);
    let rule = "err.dataflow.update.conditions";
    let update_spec_diags = spec_errors(&update_spec);
    let update_wire_diags = wire_errors(&update_wire);
    let spec_diag = assert_single_diag(
        &update_spec_diags,
        rule,
        "/nodes/4/raw/updateConditions",
        false,
    );
    let wire_diag = assert_single_diag(
        &update_wire_diags,
        rule,
        "/dataFlow/nodes/upd/updateConditions",
        false,
    );
    assert_eq!(spec_diag.source, wire_diag.source);

    let (mut bad_spec, mut bad_wire) = (spec, wire);
    bad_spec["nodes"][7]["inputs"] = json!([]);
    bad_wire["dataFlow"]["nodes"]["sel"]["inputNodes"] = json!([]);
    let bad_spec_diags = spec_errors(&bad_spec);
    let bad_wire_diags = wire_errors(&bad_wire);
    let rule = "err.dataflow.node.input";
    let spec_diag = assert_single_diag(&bad_spec_diags, rule, "/nodes/7/inputs", false);
    let wire_diag = assert_single_diag(
        &bad_wire_diags,
        rule,
        "/dataFlow/nodes/sel/inputNodes",
        false,
    );
    assert_eq!(spec_diag.message, wire_diag.message);
    assert!(spec_diag.source.is_empty());
    assert!(!wire_diag.source.is_empty());
}

#[test]
fn select_operand_checks_share_the_text2id_source_exception() {
    let fields = json!([{"name":"ID","dataType":"C","isDimension":true}]);
    let valid = json!({
        "format":1, "properties":{"extractDataEnabled":false},
        "nodes":[
            {"key":"src","type":"FileInput","config":{"fileName":"d.xlsx#S"},"fields":fields},
            {"key":"sel","type":"Select","inputs":["src"],"fields":fields,
             "steps":[{"type":"ToUppercase","fields":["id"]}]},
            {"key":"out","type":"Output","inputs":["sel"]}
        ]
    });
    let valid_wire = generate(&valid).unwrap();
    for (step, rejects) in [
        (json!({"type":"ToUppercase","fields":["ghost"]}), true),
        (json!({"type":"RemoveField","fields":["ghost"]}), false),
        (
            json!({"type":"Text2Id","fields":["ghost"],"text2id":{}}),
            true,
        ),
        (
            json!({"type":"Text2Id","fields":["ghost"],"text2id":{"srcField":""}}),
            true,
        ),
        (
            json!({"type":"Text2Id","fields":["ghost"],"text2id":{"srcField":"ID"}}),
            false,
        ),
    ] {
        let (mut spec, mut wire) = (valid.clone(), valid_wire.clone());
        spec["nodes"][1]["steps"][0] = step.clone();
        wire["dataFlow"]["nodes"]["sel"]["steps"][0] = step;
        let (generated, spec_diags) = generate_with_diagnostics(&spec);
        let wire_diags = check(&wire);
        let rule = "err.dataflow.stepField.notfound";
        if rejects {
            assert!(generated.is_none());
            let spec_diag =
                assert_single_diag(&spec_diags, rule, "/nodes/1/steps/0/fields/0", false);
            let wire_diag = assert_single_diag(
                &wire_diags,
                rule,
                "/dataFlow/nodes/sel/steps/0/fields/0",
                false,
            );
            assert_eq!(spec_diag.message, wire_diag.message);
            assert!(spec_diag.source.is_empty());
            assert!(!wire_diag.source.is_empty());
        } else {
            assert!(generated.is_some(), "{spec_diags:?}");
            assert!(wire_diags.iter().all(Diag::is_warning), "{wire_diags:?}");
        }
    }
}
