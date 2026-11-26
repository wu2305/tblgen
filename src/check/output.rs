use std::collections::HashSet;

use serde_json::Value;

use super::{err, src, warn};
use crate::model::{cite, ptr_escape, skipped_field, Diag};

/// Container keys that may nest field lists inside a partition entry
/// (folders / date hierarchies).
const PARTITION_CONTAINERS: [&str; 6] = [
    "children",
    "fields",
    "items",
    "dimensions",
    "measures",
    "subDimensions",
];

/// Collect every `name` inside a top-level partition array, flattening
/// folder/hierarchy containers. Extra names are tolerated by callers.
fn partition_names<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::Array(arr) => {
            for item in arr {
                partition_names(item, out);
            }
        }
        Value::Object(o) => {
            if let Some(n) = o.get("name").and_then(Value::as_str) {
                out.push(n);
            }
            for k in PARTITION_CONTAINERS {
                if let Some(child) = o.get(k) {
                    partition_names(child, out);
                }
            }
        }
        _ => {}
    }
}

/// True when the object looks like a leaf field rather than a pure folder:
/// it carries a name plus at least one field-attribute key.
fn looks_like_field(o: &serde_json::Map<String, Value>) -> bool {
    o.contains_key("name")
        && [
            "dataType",
            "isDimension",
            "inputField",
            "originalField",
            "dbfield",
            "length",
            "decimal",
            "exp",
        ]
        .iter()
        .any(|k| o.contains_key(*k))
}

#[derive(Default)]
struct PartitionNames<'a> {
    names: Vec<&'a str>,
    next: usize,
    folded: Option<HashSet<String>>,
}
impl PartitionNames<'_> {
    fn contains(&mut self, name: &str) -> bool {
        if self.folded.is_none() && self.names.get(self.next).copied() == Some(name) {
            self.next += 1;
            return true;
        }
        // Use the whole partition, not just the suffix: duplicate or reordered fields can refer back.
        self.folded
            .get_or_insert_with(|| self.names.iter().map(|n| n.to_uppercase()).collect())
            .contains(&name.to_uppercase())
    }
}

/// The default Output node's live field names must appear in the matching
/// top-level partition (isDimension -> dimensions, else measures). The
/// platform overrides node fields with the top-level lists, so extra top
/// fields and an absent/empty node copy are fine — only names the Output
/// declares but the partition drops are errors.
pub(super) fn check_output_partition(
    diags: &mut Vec<Diag>,
    tbl: &Value,
    nodes_obj: &serde_json::Map<String, Value>,
) {
    let Some(def) = nodes_obj.get("default") else {
        return;
    };
    // only an actual default Output partitions the top level — a non-Output
    // node that happens to use the "default" key does not
    if def
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "Output")
    {
        return;
    }
    let Some(farr) = def.get("fields").and_then(Value::as_array) else {
        return;
    };
    if farr.is_empty() {
        return;
    }
    let mut dim_names = PartitionNames::default();
    let mut meas_names = PartitionNames::default();
    if let Some(d) = tbl.get("dimensions") {
        partition_names(d, &mut dim_names.names);
    }
    if let Some(m) = tbl.get("measures") {
        partition_names(m, &mut meas_names.names);
    }
    for (i, f) in farr.iter().enumerate() {
        if f.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        if skipped_field(f) {
            continue;
        }
        let Some(nm) = f.get("name").and_then(Value::as_str) else {
            continue;
        };
        let is_dim = f.get("isDimension").and_then(Value::as_bool) == Some(true);
        let top = if is_dim {
            &mut dim_names
        } else {
            &mut meas_names
        };
        if !top.contains(nm) {
            diags.push(err(
                format!("/dataFlow/nodes/default/fields/{}/name", i),
                "flow.output.partition",
                &src(cite::LOADER, "1406", "DwTableLoader#toList"),
                format!(
                    "default Output field {:?} ({}) is missing from the top-level {} partition",
                    nm,
                    if is_dim { "dimension" } else { "measure" },
                    if is_dim { "dimensions" } else { "measures" }
                ),
            ));
        }
    }
}

/// extractDataEnabled models extract to a physical table: every top-level
/// output field needs a non-blank ASCII `dbfield`. Warn per field, flattening
/// folder/hierarchy containers like the partition check.
pub(super) fn check_extract_dbfields(diags: &mut Vec<Diag>, tbl: &Value) {
    let extract = tbl
        .pointer("/properties/extractDataEnabled")
        .and_then(Value::as_bool)
        == Some(true);
    if !extract {
        return;
    }
    for key in ["dimensions", "measures"] {
        let Some(arr) = tbl.get(key).and_then(Value::as_array) else {
            continue;
        };
        for (i, item) in arr.iter().enumerate() {
            check_dbfield_entry(diags, item, &format!("/{}", key), i);
        }
    }
}

fn check_dbfield_entry(diags: &mut Vec<Diag>, v: &Value, base: &str, idx: usize) {
    match v {
        Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                check_dbfield_entry(diags, item, base, i);
            }
        }
        Value::Object(o) => {
            if looks_like_field(o) {
                let ok = o
                    .get("dbfield")
                    .and_then(Value::as_str)
                    .map(|s| !s.trim().is_empty() && s.chars().all(|c| c.is_ascii()))
                    .unwrap_or(false);
                if !ok {
                    diags.push(warn(
                        format!("{}/{}/dbfield", base, idx),
                        "extract.dbfield",
                        &src(cite::LOADER, "1406", "DwTableLoader#toList"),
                        "extractDataEnabled is set but this output field has no ASCII dbfield",
                    ));
                }
            }
            for k in PARTITION_CONTAINERS {
                if let Some(Value::Array(arr)) = o.get(k) {
                    let child_base = format!("{}/{}/{}", base, idx, ptr_escape(k));
                    for (i, item) in arr.iter().enumerate() {
                        check_dbfield_entry(diags, item, &child_base, i);
                    }
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::check_output_partition;
    use serde_json::json;

    #[test]
    fn reordered_and_repeated_names_keep_membership_and_diagnostic_order() {
        let tbl = json!({"dimensions":[{"name":"a"},{"name":"grp","children":[{"name":"Straße"},{"name":"TAIL"}]}], "measures":[{"items":[{"name":"AMT"}]}]});
        let nodes = json!({"default":{"type":"Output","fields":[
            {"name":"a","isDimension":true}, {"name":"absent","hidden":true},
            {"name":"AMT"}, {"name":"STRASSE","isDimension":true},
            {"name":"a","isDimension":true}, {"name":"TAIL","isDimension":true},
            {"name":"AMT","isDimension":true}, {"name":"missing"},
            {"name":"ignored","invalid":true},
            {"name":"still-live","invalid":true,"isOriginalFieldInvalid":true}
        ]}});
        let mut ds = Vec::new();
        check_output_partition(&mut ds, &tbl, nodes.as_object().unwrap());
        assert!(ds.iter().all(|d| d.rule == "flow.output.partition"));
        let paths: Vec<_> = ds.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/dataFlow/nodes/default/fields/6/name",
                "/dataFlow/nodes/default/fields/7/name",
                "/dataFlow/nodes/default/fields/9/name"
            ]
        );
    }
}
