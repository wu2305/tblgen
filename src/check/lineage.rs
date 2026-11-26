use std::collections::{HashMap, HashSet};

use serde_json::Value;

use super::{err, src, warn};
use crate::model::{cite, field_eq, is_source, ptr_escape, Diag};

/// Field lineage: a field's `inputField` must name a field of the node it
/// reads from — resolved ONLY through this node's direct `inputNodes`
/// (by `inputNode` alias/key, or the sole input when unqualified). There is
/// no global by-alias shortcut: a lineage claim pointing outside the actual
/// predecessor is unverifiable and skipped rather than guessed. Node types
/// whose fields are pivot/remap-born or self-declared are exempt from the
/// inputField check. `originalNode` is checked on EVERY kind as a warning:
/// it must name a root source node (ModelTable/DbTable/FileInput/Sql/
/// Script), since computed values and remapped fields carry no origin.
pub(super) fn check_lineage(diags: &mut Vec<Diag>, nodes: &serde_json::Map<String, Value>) {
    // node types whose output fields are not name-inheriting graph reads
    const NOT_NAME_INHERITING: [&str; 10] = [
        "ColToRow",
        "RowToCol",
        "Agg",
        "SubProcess",
        "Script",
        "DbTable",
        "Sql",
        "Extend",
        "FileInput",
        "ModelTable",
    ];
    // effective node name consumers use: alias, falling back to the map key
    let name_of = |k: &str| -> String {
        nodes
            .get(k)
            .and_then(|n| n.get("alias"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| k.to_string())
    };
    // aliases (effective names) of root source nodes for originalNode
    let source_names: HashSet<String> = nodes
        .iter()
        .filter(|(_, n)| {
            n.get("type")
                .and_then(Value::as_str)
                .map(is_source)
                .unwrap_or(false)
        })
        .map(|(k, _)| name_of(k).to_uppercase())
        .collect();
    let names_of = |n: &Value| -> HashSet<String> {
        n.get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|f| f.get("name").and_then(Value::as_str).map(str::to_uppercase))
            .collect()
    };
    let mut names_by_pred: HashMap<&str, HashSet<String>> = HashMap::new();
    for (id, node) in nodes {
        let ty = node.get("type").and_then(Value::as_str).unwrap_or("");
        let inputs: Vec<&str> = node
            .get("inputNodes")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let check_inputs = !NOT_NAME_INHERITING.contains(&ty);
        for (i, f) in node
            .get("fields")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            // originalNode: every kind, warning only — must be a root source
            if let Some(on) = f
                .get("originalNode")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if !source_names.contains(&on.to_uppercase()) {
                    diags.push(warn(
                        format!(
                            "/dataFlow/nodes/{}/fields/{}/originalNode",
                            ptr_escape(id),
                            i
                        ),
                        "field.originalNode",
                        &src(
                            cite::DATAFLOW_CTX,
                            "207",
                            "DwDataFlowContext#initNodeFields",
                        ),
                        format!("originalNode {:?} is not a root source node alias", on),
                    ));
                }
            }
            if !check_inputs {
                continue;
            }
            let Some(input_field) = f
                .get("inputField")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if f.get("exp")
                .and_then(Value::as_str)
                .is_some_and(|e| !e.is_empty())
            {
                continue;
            }
            // the actual predecessor: inputNode names one of THIS node's
            // direct inputs (alias or key); unqualified fields need exactly
            // one input to be provable.
            let pred_key: Option<&str> = match f.get("inputNode").and_then(Value::as_str) {
                Some(input_node) if !input_node.is_empty() => inputs.iter().copied().find(|k| {
                    nodes.get(*k).map(|nn| {
                        let alias = nn
                            .get("alias")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or(*k);
                        field_eq(alias, input_node) || field_eq(*k, input_node)
                    }) == Some(true)
                }),
                _ if inputs.len() == 1 => Some(inputs[0]),
                _ => None,
            };
            let Some(key) = pred_key else {
                continue;
            };
            let Some(src_node) = nodes.get(key) else {
                continue;
            };
            let names = names_by_pred
                .entry(key)
                .or_insert_with(|| names_of(src_node));
            if !names.is_empty() && !names.contains(&input_field.to_uppercase()) {
                diags.push(err(
                    format!("/dataFlow/nodes/{}/fields/{}/inputField", ptr_escape(id), i),
                    "field.inputField.missing",
                    &src(
                        cite::DATAFLOW_CTX,
                        "207",
                        "DwDataFlowContext#initNodeFields",
                    ),
                    format!(
                        "field {:?} reads {:?}, which its predecessor {:?} does not have",
                        f.get("name").and_then(Value::as_str).unwrap_or(""),
                        input_field,
                        key
                    ),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::check_lineage;
    use serde_json::json;

    #[test]
    fn same_alias_on_distinct_predecessors_does_not_share_fields() {
        let nodes = json!({
            "a": {"type":"ModelTable","alias":"source","fields":[{"name":"Straße"}]},
            "b": {"type":"ModelTable","alias":"source","fields":[{"name":"B"}]},
            "first": {"type":"Select","inputNodes":["a"],"fields":[{"name":"x","inputField":"STRASSE"}]},
            "second": {"type":"Select","inputNodes":["b"],"fields":[
                {"name":"y","inputField":"B"}, {"name":"z","inputField":"STRASSE"}
            ]}
        });
        let mut ds = Vec::new();
        check_lineage(&mut ds, nodes.as_object().unwrap());
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].rule, "field.inputField.missing");
        assert_eq!(ds[0].path, "/dataFlow/nodes/second/fields/1/inputField");
    }
}
