//! Shared model: diagnostics, enums and allowed-key registries.
//!
//! Registries here are the single source of truth for both the JSON Schema
//! output (`schema` command) and the validators (`check`/`gen`). Every typed
//! object is closed; the only escape hatches are the explicit `raw` /
//! `rawNode` / `rawStep` channels. Field objects carry only the small set of
//! scalar keys proved to be consumed by the loaders — no arbitrary JSON
//! payloads, no `children`/hierarchy trees.

use serde_json::Value;

/// A single diagnostic. `path` is an RFC 6901 JSON pointer into the document
/// being validated (spec path for `gen`, wire `.tbl` path for `check`).
/// `source` carries the product evidence citation (`file:line symbol`) for
/// check rules; it is empty for generator diagnostics.
#[derive(Debug, Clone)]
pub struct Diag {
    pub path: String,
    pub rule: String,
    pub source: String,
    pub message: String,
}

impl Diag {
    pub fn new(
        path: impl Into<String>,
        rule: impl Into<String>,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Diag {
            path: path.into(),
            rule: rule.into(),
            source: source.into(),
            message: message.into(),
        }
    }

    /// Spec/gen diagnostic: path inside the spec document.
    pub fn spec(
        path: impl Into<String>,
        rule: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Diag::new(path, rule, "", message)
    }

    /// Check diagnostic: path inside the `.tbl` wire document plus evidence.
    pub fn wire(
        path: impl Into<String>,
        rule: impl Into<String>,
        source: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Diag::new(path, rule, source, message)
    }

    /// Warnings carry a `warn.` rule prefix and never fail a command.
    pub fn is_warning(&self) -> bool {
        self.rule.starts_with("warn.")
    }

    pub fn to_json(&self) -> Value {
        serde_json::json!({
            "path": self.path,
            "rule": self.rule,
            "source": if self.source.is_empty() { Value::Null } else { Value::String(self.source.clone()) },
            "message": self.message,
        })
    }
}

/// Escape a JSON pointer segment (RFC 6901).
pub fn ptr_escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

/// Build a JSON pointer from segments.
pub fn ptr(segments: &[&str]) -> String {
    let mut out = String::new();
    for seg in segments {
        out.push('/');
        out.push_str(&ptr_escape(seg));
    }
    out
}

/// Append one segment to an existing pointer.
pub fn ptr_push(base: &str, seg: &str) -> String {
    format!("{}/{}", base, ptr_escape(seg))
}

/// Platform save version stamped on generated `.tbl` documents when the
/// spec does not declare `version` (`DwTableService` writes this on save).
pub const SAVE_VERSION: &str = "4.19.0";

/// Field-name equality as the platform sees it: `QueryImpl#getField` and
/// friends compare `toUpperCase()`d names, which is Unicode-aware
/// (`ß` folds to `SS`), not ASCII-only case folding.
pub(crate) fn field_eq(a: &str, b: &str) -> bool {
    a.to_uppercase() == b.to_uppercase()
}

/// Node types whose fields are self-declared roots of lineage: they have no
/// graph predecessor whose fields could be inherited.
pub(crate) fn is_source(ty: &str) -> bool {
    matches!(
        ty,
        "ModelTable" | "DbTable" | "FileInput" | "Sql" | "Script"
    )
}

/// Whether the loader drops this field before reading it:
/// `invalid == true` unless `isOriginalFieldInvalid == true` resurrects it
/// (`DataFlowNodeImpl` field filtering). Hidden fields are filtered by
/// callers separately.
pub(crate) fn skipped_field(f: &Value) -> bool {
    f.get("invalid").and_then(Value::as_bool) == Some(true)
        && f.get("isOriginalFieldInvalid").and_then(Value::as_bool) != Some(true)
}

/// Evolve a provable field-name list through one Select step, mirroring the
/// name effects of `QueryImpl` step execution (`removeField`, `renameField`,
/// `addField`). Shared by the generator projection and the wire checker.
///
/// Returns `Ok(true)` when the step's name effect is known and `names` was
/// updated in place, `Ok(false)` when the step reshapes fields in a way the
/// projection cannot model (rawStep, SplitField, Text2Id, unknown types) and
/// `Err(tag)` when the step would create a duplicate field name — the tag is
/// the payload path tail used for diagnostics (`"addField/name"` or
/// `"refactor/rename"`).
pub(crate) fn evolve_names(names: &mut Vec<String>, step: &Value) -> Result<bool, &'static str> {
    if step.get("rawStep").is_some() {
        return Ok(false);
    }
    let ty = step.get("type").and_then(Value::as_str).unwrap_or("");
    let operands: Vec<String> = step
        .get("fields")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    match ty {
        "Filter"
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
        | "SubString"
        | "Replace"
        | "Distinct"
        | "TransformToCalculatedField" => Ok(true),
        "RemoveField" => {
            names.retain(|n| !operands.iter().any(|f| field_eq(f, n)));
            Ok(true)
        }
        "Refactor" => {
            if let Some(rn) = step.pointer("/refactor/rename").and_then(Value::as_str) {
                if !rn.is_empty() {
                    if let Some(pos) = names
                        .iter()
                        .position(|n| operands.first().map(|f| field_eq(f, n)).unwrap_or(false))
                    {
                        let own = field_eq(rn, &names[pos]);
                        let collision = names
                            .iter()
                            .enumerate()
                            .any(|(i, n)| i != pos && field_eq(n, rn));
                        if !own && collision {
                            return Err("refactor/rename");
                        }
                        names[pos] = rn.to_string();
                    }
                }
            }
            Ok(true)
        }
        "AddField" => {
            if let Some(nm) = step.pointer("/addField/name").and_then(Value::as_str) {
                if !nm.is_empty() {
                    if names.iter().any(|n| field_eq(n, nm)) {
                        return Err("addField/name");
                    }
                    names.push(nm.to_string());
                }
            }
            Ok(true)
        }
        _ => Ok(false),
    }
}

// ---------------------------------------------------------------------------
// Node types — DataFlowNodeType, 20 enum values (coverage.md node table).
// ---------------------------------------------------------------------------

/// All 20 `DataFlowNodeType` enum values, in declaration order.
pub const NODE_TYPES: [&str; 20] = [
    "DbTable",
    "ModelTable",
    "Select",
    "Join",
    "Union",
    "Agg",
    "Distinct",
    "Sql",
    "Output",
    "DbOutput",
    "RowToCol",
    "ColToRow",
    "SlowChanging",
    "FileInput",
    "Snapshot",
    "Script",
    "GeoFindLocation",
    "Update",
    "SubProcess",
    "Extend",
];

/// The 17 node types registered in `DwUtilDataFlow.CLASSES`
/// (`DwUtilDataFlow.java:100-120`). The other three fall back to the extend
/// contributor and have no analyzed semantics — they may only appear inside
/// `rawNode`, never as typed nodes.
pub const SUPPORTED_NODE_TYPES: [&str; 17] = [
    "DbTable",
    "ModelTable",
    "Select",
    "Join",
    "Union",
    "Agg",
    "Distinct",
    "Sql",
    "Output",
    "DbOutput",
    "RowToCol",
    "ColToRow",
    "FileInput",
    "Snapshot",
    "Script",
    "Update",
    "SubProcess",
];

/// Node types whose semantics are covered by coverage.md "已分析".
pub fn node_supported(ty: &str) -> bool {
    SUPPORTED_NODE_TYPES.contains(&ty)
}

// ---------------------------------------------------------------------------
// Select step types — DataFlowSelectNodeStepType, 24 enum values.
// ---------------------------------------------------------------------------

/// All 24 `DataFlowSelectNodeStepType` enum values.
pub const STEP_TYPES: [&str; 24] = [
    "Filter",
    "Distinct",
    "AddField",
    "RemoveField",
    "SplitField",
    "Refactor",
    "ReGroup",
    "SubString",
    "Replace",
    "GroupNumber",
    "ToNumber",
    "ToDate",
    "ToUppercase",
    "ToLowercase",
    "ToSynonym",
    "Trim",
    "DeleteNumbers",
    "DeleteLetters",
    "DeleteSpechars",
    "DeletePunctuations",
    "ExcludeEmpty",
    "ExtractNumber",
    "Text2Id",
    "TransformToCalculatedField",
];

/// The 18 step types with an analyzed `doCompileQuery` branch
/// (`DwDataFlowNodeContributor_select.java:117-272`). The remaining six have
/// no matching branch or an empty TODO — they may only appear inside
/// `rawStep`, never as typed steps.
pub const SUPPORTED_STEP_TYPES: [&str; 18] = [
    "Filter",
    "AddField",
    "RemoveField",
    "SplitField",
    "Refactor",
    "SubString",
    "Replace",
    "ToUppercase",
    "ToLowercase",
    "Trim",
    "DeleteNumbers",
    "DeleteLetters",
    "DeleteSpechars",
    "DeletePunctuations",
    "ExcludeEmpty",
    "ExtractNumber",
    "Text2Id",
    "TransformToCalculatedField",
];

/// Steps whose `doCompileQuery` branch is missing/empty TODO — typed form is
/// rejected, `rawStep` required.
pub const UNSUPPORTED_STEP_TYPES: [&str; 6] = [
    "Distinct",
    "ReGroup",
    "GroupNumber",
    "ToNumber",
    "ToDate",
    "ToSynonym",
];

pub fn step_supported(ty: &str) -> bool {
    SUPPORTED_STEP_TYPES.contains(&ty)
}

// ---------------------------------------------------------------------------
// Field objects — closed whitelist of proved scalar keys only.
// ---------------------------------------------------------------------------

/// Allowed keys on a spec field object. `name`, `dataType`, `isDimension` are
/// required; everything else optional scalar. No arbitrary JSON payloads, no
/// `children`/hierarchy containers — anything richer goes through `raw` at
/// the node level (which decompile uses via `rawNode` when needed).
pub const FIELD_KEYS: &[&str] = &[
    // required triple
    "name",
    "dataType",
    "isDimension",
    // lineage / expression / physical mapping
    "dbfield",
    "length",
    "decimal",
    "inputField",
    "inputNode",
    "originalField",
    "originalNode",
    "exp",
    // loader-consumed scalars (DataFlowNodeImpl#loadField / DwTableLoader)
    "dateFormat",
    "aggType",
    "desc",
    "extractable",
    "hidden",
];

pub fn field_key_allowed(k: &str) -> bool {
    FIELD_KEYS.contains(&k)
}

// ---------------------------------------------------------------------------
// Node `config` keys — closed per-type whitelists.
//
// `config` maps to flattened node-level wire keys. Scalar keys are typed by
// JSON kind; object/array payloads exist only as the closed schemas in
// `ClosedKind` (joinConditions / unionMapArray / colToRowInfo / groupFields /
// groupFieldsMap / distinctFields / distinctSorts). Anything else —
// updateConditions, rowToColInfo, arbitrary maps — must go through `raw`.
// ---------------------------------------------------------------------------

/// Scalar config keys allowed on EVERY typed node.
pub const CONFIG_KEYS_COMMON: &[&str] = &["comment", "devQueryFixedRows", "devFixedRowCount"];

/// FileInput loader keys (`FileInputInfoImpl.loadInputInfoWithMap`,
/// java:dw/utils/impl/fileinput/FileInputInfoImpl.java:297-345) — strictly typed.
pub const CONFIG_KEYS_FILEINPUT: &[&str] = &[
    "fileName",
    "absoluteFileName",
    "sheetName",
    "projectName",
    "dbType",
    "fileNameMatchPattern",
    "sheetNameMatchPattern",
    "fileFilter",
    "splitChar",
    "quoteChar",
    "charset",
    "dateFormat",
    "timestampFormat",
    "fileNameMatchMode",
    "sheetNameMatchMode",
    "matchMultipleFiles",
    "includeSubDirectories",
    "matchMultipleSheets",
    "firstRowIsColumnName",
    "inferHeads",
    "sheetIsForm",
    "includeRowNum",
    "includeSheetName",
    "includeFileName",
    "headsStart",
    "headsEnd",
    "fieldsInferSize",
    "dataRowCount",
    "indentFields",
    "substringHeader",
];

/// Scalar config keys allowed per node type (beyond CONFIG_KEYS_COMMON).
pub fn config_scalar_keys(ty: &str) -> &'static [&'static str] {
    match ty {
        "FileInput" => CONFIG_KEYS_FILEINPUT,
        "ModelTable" => &["moduleTablePath"],
        "DbTable" => &["datasource", "dbSchema", "dbTableName", "sql", "selectSql"],
        "Sql" => &["sql", "datasource", "dbSchema", "dbTableName"],
        "Script" => &["scriptES5", "scriptName"],
        "SubProcess" => &["subProcessId"],
        "DbOutput" => &[
            "datasource",
            "dbSchema",
            "dbTableName",
            "periodType",
            "periodField",
            "periodFieldStart",
            "periodFieldEnd",
            "modifyDbTableEnabled",
            "rollbackEnabled",
            "useTruncate",
            "mergeData",
            "rebuildIndexAndKey",
            "extractMethod",
            "md5Field",
        ],
        "Snapshot" => &[
            "periodStartField",
            "periodEndField",
            "snapshotGrain",
            "periodType",
            "periodField",
        ],
        "Select" => &["stepCount"],
        "Union" => &["unionType"],
        _ => &[],
    }
}

/// Closed object/array payload kinds allowed in `config`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedKind {
    /// joinConditions[]: {leftTable,rightTable,joinType?,exp?,clauses?[{leftExp,rightExp,operator,exp}]}
    JoinConditions,
    /// unionMapArray[]: {visible?,values:[string|null,...]}
    UnionMapArray,
    /// colToRowInfo: {headers:[string], valuesArray:[[string|null|fieldref]]}
    ColToRowInfo,
    /// groupFields: [string]
    StrArray,
    /// groupFieldsMap: {string:string}
    StrMap,
    /// distinctSorts[]: {exp,type}
    Sorts,
    /// incrementRange[]: clause objects with arbitrary loader keys
    /// (`ClauseFilterImpl.loadJsonMap` accepts any map).
    Clauses,
    /// filter object: {matchAll?,enabled?,clauses?:[object]}
    Condition,
}

/// Closed payload keys per node type. `filter`/`incrementRange` are read on
/// every node by the loaders, but the typed spec only admits them where the
/// platform actually writes them (Agg conditions, DbOutput increments).
pub fn config_closed_keys(ty: &str) -> &'static [(&'static str, ClosedKind)] {
    match ty {
        "Join" => &[("joinConditions", ClosedKind::JoinConditions)],
        "Union" => &[("unionMapArray", ClosedKind::UnionMapArray)],
        "ColToRow" => &[("colToRowInfo", ClosedKind::ColToRowInfo)],
        "Agg" => &[
            ("groupFields", ClosedKind::StrArray),
            ("groupFieldsMap", ClosedKind::StrMap),
            ("filter", ClosedKind::Condition),
        ],
        "Distinct" => &[("distinctSorts", ClosedKind::Sorts)],
        "DbOutput" => &[("incrementRange", ClosedKind::Clauses)],
        _ => &[],
    }
}

/// distinctFields is a plain string array on Distinct nodes.
pub fn config_strarray_keys(ty: &str) -> &'static [&'static str] {
    match ty {
        "Distinct" => &["distinctFields"],
        _ => &[],
    }
}

/// Expected scalar JSON kind for a config key (strict typing for FileInput).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScalarKind {
    Str,
    Num,
    Bool,
    AnyScalar,
}

pub fn config_scalar_kind(ty: &str, k: &str) -> ScalarKind {
    if ty == "FileInput" {
        return match k {
            "headsStart" | "headsEnd" | "fieldsInferSize" | "dataRowCount" => ScalarKind::Num,
            "inferHeads"
            | "matchMultipleFiles"
            | "includeSubDirectories"
            | "matchMultipleSheets"
            | "firstRowIsColumnName"
            | "sheetIsForm"
            | "includeRowNum"
            | "includeSheetName"
            | "includeFileName" => ScalarKind::Bool,
            _ => ScalarKind::Str,
        };
    }
    match k {
        "devQueryFixedRows"
        | "modifyDbTableEnabled"
        | "rollbackEnabled"
        | "useTruncate"
        | "mergeData"
        | "rebuildIndexAndKey"
        | "extractable" => ScalarKind::Bool,
        "devFixedRowCount" | "stepCount" | "headsStart" | "headsEnd" | "fieldsInferSize"
        | "dataRowCount" | "length" | "decimal" => ScalarKind::Num,
        _ => ScalarKind::AnyScalar,
    }
}

/// Union type enum values (wire `unionType`).
/// Join node condition types (`JoinRelation.JOINTYPES`, union types excluded).
pub const JOIN_TYPES: [&str; 7] =
    ["InnerJoin", "LeftJoin", "RightJoin", "FullJoin", "CrossJoin", "AntiJoin", "SemiJoin"];

pub const UNION_TYPES: [&str; 4] = ["UnionAll", "Union", "Minus", "Intersect"];

pub fn config_key_allowed(ty: &str, k: &str) -> bool {
    CONFIG_KEYS_COMMON.contains(&k)
        || config_scalar_keys(ty).contains(&k)
        || config_strarray_keys(ty).contains(&k)
        || config_closed_keys(ty).iter().any(|(ck, _)| *ck == k)
}

// ---------------------------------------------------------------------------
// Select step payload keys (DataFlowSelectNodeStepImpl ctor :96-170).
// ---------------------------------------------------------------------------

/// Typed step shape: `type` + `fields` (string array where applicable) +
/// `isRegExp` + exactly the payload key(s) for that step type.
pub fn step_allowed_keys(ty: &str) -> &'static [&'static str] {
    match ty {
        "Filter" => &["type", "fields", "filter"],
        "AddField" => &["type", "fields", "addField"],
        "RemoveField" => &["type", "fields"],
        "SplitField" => &["type", "fields", "split"],
        "Refactor" => &["type", "fields", "refactor"],
        "SubString" => &["type", "fields", "subString"],
        "Replace" => &["type", "fields", "replace"],
        "ToUppercase" | "ToLowercase" | "ToNumber" | "ToDate" | "ToSynonym" | "Distinct"
        | "DeleteNumbers" | "DeleteLetters" | "DeletePunctuations" | "ExcludeEmpty"
        | "ExtractNumber" => &["type", "fields"],
        "Trim" => &["type", "fields", "trim"],
        "DeleteSpechars" => &["type", "fields", "spechars", "isRegExp"],
        "ReGroup" => &["type", "fields", "group"],
        "GroupNumber" => &["type", "fields", "groupNumber"],
        "Text2Id" => &["type", "fields", "text2id"],
        "TransformToCalculatedField" => &["type", "fields", "transformFieldInfo"],
        _ => &[],
    }
}

/// Step types that carry a `fields` string-array operand.
pub fn step_uses_fields(ty: &str) -> bool {
    step_allowed_keys(ty).contains(&"fields")
}

/// Closed payload sub-object key sets (proved constructor-read keys only).
/// `None` = payload must be a closed object validated by `step_payload_keys`.
pub const STEP_PAYLOAD_KEYS: [(&str, &[&str]); 10] = [
    (
        "addField",
        // field-shaped: reuse FIELD_KEYS (name/dataType/isDimension/exp/...)
        &[
            "name",
            "dataType",
            "isDimension",
            "exp",
            "dbfield",
            "length",
            "decimal",
            "inputField",
            "inputNode",
            "originalField",
            "originalNode",
            "dateFormat",
            "aggType",
            "desc",
            "extractable",
            "hidden",
        ],
    ),
    (
        "split",
        &[
            "splitString",
            "splitOff",
            "splitType",
            "splitFieldsCount",
            "childFields",
            "showSplitResultOrder",
            "resultOrderField",
        ],
    ),
    ("replace", &["replaceValues", "replace"]),
    ("group", &["type"]), // placeholder; ReGroup is rawStep-only anyway
    // groupNumber.interval is an ARRAY of integers (verified), but
    // GroupNumber is rawStep-only — kept for wire-side reference only.
    ("groupNumber", &["interval", "intervalCaption"]),
    ("refactor", &["rename", "desc", "dataType"]),
    (
        "text2id",
        &[
            "mapTablePath",
            "textField",
            "idField",
            "synonym",
            "sortField",
            "srcField",
        ],
    ),
    ("trim", &["trimPosition", "trimAllBlank"]),
    (
        "subString",
        &[
            "type",
            "boundaryType",
            "count",
            "start",
            "end",
            "headChar",
            "footChar",
            "includeBoundary",
        ],
    ),
    ("transformFieldInfo", &["exp", "dataType"]),
];

/// Payload sub-object key lookup.
pub fn step_payload_keys(name: &str) -> Option<&'static [&'static str]> {
    STEP_PAYLOAD_KEYS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, v)| *v)
}

/// Which payload object key (if any) a typed step type carries.
pub fn step_payload_name(ty: &str) -> Option<&'static str> {
    match ty {
        "Filter" => Some("filter"), // scalar string, not object
        "AddField" => Some("addField"),
        "SplitField" => Some("split"),
        "Refactor" => Some("refactor"),
        "SubString" => Some("subString"),
        "Replace" => Some("replace"),
        "Trim" => Some("trim"),
        "Text2Id" => Some("text2id"),
        "TransformToCalculatedField" => Some("transformFieldInfo"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Spec structural keys.
// ---------------------------------------------------------------------------

/// Closed top-level keys of the generated form.
pub const SPEC_TOP_KEYS: &[&str] = &[
    "format",
    "version",
    "properties",
    "parameters",
    "subProcesses",
    "nodes",
    "raw",
    // archive / rawTable forms use these instead of the generated keys
    "archive",
    "rawTable",
];

/// Closed keys of a generated-form node entry.
pub const SPEC_NODE_KEYS: &[&str] = &[
    "key",
    "type",
    "alias",
    "id",
    "inputs",
    "fields",
    "originalFields",
    "config",
    "steps",
    "raw",
    "rawNode",
];

/// Value shape of a typed `properties` key, as read by the loader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropKind {
    /// fixed to "DataFlow" for generated specs
    ModelDataType,
    Bool,
    Str,
    StrList,
    /// list of "field"/"index"/"constraint", or a legacy boolean
    ModifyDbTable,
}

/// Typed `properties` keys: every model property that
/// `DwTableLoader#loadProperties` reads for a DataFlow model, with its shape.
/// Anything else goes under `properties.raw` and is copied verbatim.
pub const TYPED_PROPERTIES: &[(&str, PropKind)] = &[
    ("modelDataType", PropKind::ModelDataType),
    ("datasource", PropKind::Str),
    ("dbSchema", PropKind::Str),
    ("dbTableName", PropKind::Str),
    ("extractDataEnabled", PropKind::Bool),
    ("dataReadonly", PropKind::Bool),
    ("rebuildIndexAndKey", PropKind::Bool),
    ("extractTargetMode", PropKind::Str),
    ("modifyDbTableEnabled", PropKind::ModifyDbTable),
    ("dateKeyValueFormat", PropKind::Str),
    ("useTruncate", PropKind::Bool),
    ("enableTempTable", PropKind::Bool),
    ("rollbackEnabled", PropKind::Bool),
    ("disableQueryCache", PropKind::Bool),
    ("disableRowCount", PropKind::Bool),
    ("allowDownloadAllData", PropKind::Bool),
    ("primaryKeys", PropKind::StrList),
    ("businessKeys", PropKind::StrList),
];

pub const MODIFY_DB_TABLE_ITEMS: [&str; 3] = ["field", "index", "constraint"];

pub fn prop_kind(k: &str) -> Option<PropKind> {
    TYPED_PROPERTIES.iter().find(|(n, _)| *n == k).map(|(_, kind)| *kind)
}

/// Whether `v` has the shape the loader expects for a typed property.
pub fn prop_value_fits(kind: PropKind, v: &Value) -> bool {
    match kind {
        PropKind::ModelDataType => v.as_str() == Some("DataFlow"),
        PropKind::Bool => v.is_boolean(),
        PropKind::Str => v.is_string(),
        PropKind::StrList => v
            .as_array()
            .map(|a| a.iter().all(Value::is_string))
            .unwrap_or(false),
        PropKind::ModifyDbTable => {
            v.is_boolean()
                || v.as_array()
                    .map(|a| {
                        a.iter().all(|x| {
                            x.as_str().map(|s| MODIFY_DB_TABLE_ITEMS.contains(&s)).unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
        }
    }
}

/// Closed keys of a `parameters[]` item.
pub const PARAMETER_KEYS: &[&str] = &["name", "dataType", "defaultValue", "raw"];

/// Closed keys of a `subProcesses` item.
pub const SUBPROCESS_KEYS: &[&str] = &["id", "caption", "rootNodeId", "nodeIds", "raw"];

/// Evidence citations used by `check` rules (relative product paths).
pub mod cite {
    pub const LOADER: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/DwTableLoader.java";
    pub const NODE_IMPL: &str =
        "com.succez.bi/src/main/java/com/succez/dw/meta/impl/flow/DataFlowNodeImpl.java";
    pub const SELECT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_select.java";
    pub const OUTPUT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_output.java";
    pub const JOIN_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_join.java";
    pub const UNION_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_union.java";
    pub const COLUMNS_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls";
    pub const BASE_CONTRIB: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributorImpl.java";
    pub const DWUTIL: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/DwUtilDataFlow.java";
    pub const STEP_IMPL: &str =
        "com.succez.bi/src/main/java/com/succez/dw/meta/impl/flow/DataFlowSelectNodeStepImpl.java";
    pub const JOIN_REL: &str =
        "com.succez.bi/src/main/java/com/succez/dw/utils/impl/query/meta/JoinRelationImpl.java";
    pub const NODEIMPL_TS: &str = "com.succez.bi/web/static-file/dw/dataflow/nodeimpl.ts";
    pub const BUILDER_TS: &str = "com.succez.bi/web/static-file/dw/dwbuilder.ts";
    pub const QUERY_IMPL: &str =
        "com.succez.bi/src/main/java/com/succez/dw/utils/impl/query/QueryImpl.java";
    pub const FILEINPUT_INFO: &str =
        "com.succez.bi/src/main/java/com/succez/dw/utils/impl/fileinput/FileInputInfoImpl.java";
    pub const QUERY_HELPER: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/helper/DwDataFlowQueryHelper.java";
    pub const DATAFLOW_CTX: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/DwDataFlowContext.java";
    pub const NODE_EXECUTOR: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/DwDataFlowNodeExecutorImpl.java";
    // individual contributor files under nodeImpls/ (COLUMNS_NODE is the
    // directory; these are the per-class paths)
    pub const DBTABLE_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_dbtable.java";
    pub const SQL_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_sql.java";
    pub const SCRIPT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_script.java";
    pub const DBOUTPUT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_dboutput.java";
    pub const SUBPROCESS_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_subProcess.java";
    pub const DWTABLE_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_dwtable.java";
    pub const COLTOROW_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_colToRow.java";
    pub const ROWTOROC_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_rowToCol.java";
    pub const SNAPSHOT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_Snapshot.java";
    pub const FILEINPUT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_fileInput.java";
    pub const UPDATE_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_update.java";
    pub const AGG_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_agg.java";
    pub const DISTINCT_NODE: &str =
        "com.succez.bi/src/main/java/com/succez/dw/services/impl/dataflow/nodeImpls/DwDataFlowNodeContributor_distinct.java";
}

/// 提取 Select 步骤表达式中的方括号引用。
/// 单个标识符含点号或属于方括号链时，`qualified` 为 `true`。
/// 步骤校验据此跳过无法在当前字段集合中解析的限定引用。
/// 引号内的文本会被跳过，标识符中的 `]]` 表示字面量 `]`。
pub fn exp_bracket_refs(exp: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    scan_bracket_refs(exp, |reference| {
        let qualified =
            reference.chained_before || reference.chained_after || reference.name.contains('.');
        out.push((reference.name, qualified));
    });
    out
}

/// 提取 Join 表达式中方括号链首的表别名。
/// `[alias].[field].[attr]` 只返回 `alias`。点号两侧允许空白，
/// 词法规则与 [`exp_bracket_refs`] 相同。
pub fn exp_table_qualifiers(exp: &str) -> Vec<String> {
    let mut out = Vec::new();
    scan_bracket_refs(exp, |reference| {
        if reference.chained_after && !reference.chained_before {
            out.push(reference.name);
        }
    });
    out
}

struct BracketRef {
    name: String,
    chained_before: bool,
    chained_after: bool,
}

// 跳过引号内的文本，仅提取闭合的方括号标识符，并记录两侧的链连接。
fn scan_bracket_refs(exp: &str, mut visit: impl FnMut(BracketRef)) {
    let b = exp.as_bytes();
    let mut i = 0;
    let mut chained_before = false;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' || c == b'"' {
            let q = c;
            i += 1;
            while i < b.len() {
                if b[i] == b'\\' {
                    i += 2;
                    continue;
                }
                if b[i] == q {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == b'[' {
            let mut name = String::new();
            let mut start = i + 1;
            let mut j = start;
            while j < b.len() {
                if b[j] == b']' {
                    if j + 1 < b.len() && b[j + 1] == b']' {
                        // 保留转义对中的第一个右括号。
                        name.push_str(&exp[start..j + 1]);
                        j += 2;
                        start = j;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            if j == b.len() {
                i += 1;
                continue;
            }
            // 切片端点来自 ASCII 括号，保留标识符中的完整 UTF-8 字符。
            name.push_str(&exp[start..j]);
            let rest = exp[j + 1..].trim_start();
            let chained_after = rest
                .strip_prefix('.')
                .is_some_and(|r| r.trim_start().starts_with('['));
            visit(BracketRef {
                name,
                chained_before,
                chained_after,
            });
            chained_before = chained_after;
            i = j + 1;
            continue;
        }
        i += 1;
    }
}
