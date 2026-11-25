//! Static rules for server scripts (`.action.ts`).
//!
//! DataFlow scripts live in `/<project>/data/script/`, batch scripts in
//! `/<project>/data/batch/`. Rules are checked on the code with comments
//! removed, except where a rule is about the comments themselves.

use crate::model::Diag;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptKind {
    DataFlow,
    Batch,
    Other,
}

pub fn kind_of(path: &str) -> ScriptKind {
    let p = path.replace('\\', "/");
    if p.contains("/data/batch/") {
        ScriptKind::Batch
    } else if p.contains("/data/script/") {
        ScriptKind::DataFlow
    } else {
        ScriptKind::Other
    }
}

fn diag(rule: &str, msg: impl Into<String>) -> Diag {
    Diag::spec("", rule, msg)
}

/// Check one script source. `path` decides the kind (dataflow vs batch).
pub fn check_script(path: &str, src: &str) -> Vec<Diag> {
    let code = strip_comments(src);
    let kind = kind_of(path);
    let mut d = Vec::new();
    if let Some(line) = line_of(&code, "context.params") {
        d.push(diag(
            "script.params",
            format!("line {}: reads context.params; put configuration in top-level constants", line),
        ));
    }
    if let Some(line) = find_date_const(&code) {
        d.push(diag(
            "script.dateConst",
            format!(
                "line {}: a constant holds an absolute date; keep only a lookback length \
                 (LOOKBACK_DAYS/LOOKBACK_MONTHS) and derive the window at run time",
                line
            ),
        ));
    }
    // the preview limit is for DataFlow script nodes, whose entry point is
    // onProcessData (DwDataFlowNodeContributor_script); extension-point
    // scripts (onQueryData/onUpdateData) in the same directory are exempt,
    // and placeholder scripts never write rows
    if kind == ScriptKind::DataFlow
        && defines_function(&code, "onProcessData")
        && !src.contains("占位")
        && (!code.contains("context.isPreview")
            || (!code.contains("PREVIEW_MAX_ROWS") && !src.contains("第一页")))
    {
        d.push(diag(
            "script.previewLimit",
            "no preview limit: paged writers need context.isPreview + PREVIEW_MAX_ROWS; \
             full dedup scans read only the first page in preview",
        ));
    }
    if kind == ScriptKind::Batch && has_loop(&code) && !code.contains("isCanceled") {
        d.push(diag(
            "script.cancelCheck",
            "a long loop without an isCanceled() check cannot be stopped",
        ));
    }
    if code.contains("BELONGTOID")
        && (code.contains("rootOf") || src.contains("归并"))
        && !has_linkage_note(src)
    {
        d.push(diag(
            "script.belongtoLinkage",
            "BELONGTOID merge chain without a note that its definition is shared/synced \
             with the other scripts that compute it (e.g. 与…同步口径)",
        ));
    }
    d
}

/// `dbfield` (upper-cased) -> `name` for every object literal in the script
/// that declares both as string literals, e.g. a `fieldsInfo` entry
/// `{ name: "会员号", dbfield: "VIPCODE", ... }`. Fields built at run time
/// are not visible here.
pub fn declared_fields(src: &str) -> std::collections::HashMap<String, String> {
    let code = strip_comments(src);
    let mut out = std::collections::HashMap::new();
    let mut start = None;
    for (i, c) in code.char_indices() {
        match c {
            '{' => start = Some(i),
            '}' => {
                if let Some(s) = start.take() {
                    let obj = &code[s..i];
                    if let (Some(n), Some(d)) = (string_prop(obj, "name"), string_prop(obj, "dbfield")) {
                        out.insert(d.to_uppercase(), n);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Value of `key: "..."` / `key: '...'` in an object literal body.
fn string_prop(obj: &str, key: &str) -> Option<String> {
    let mut from = 0;
    while let Some(off) = obj[from..].find(key) {
        let at = from + off;
        from = at + key.len();
        if obj[..at].chars().next_back().is_some_and(is_word) {
            continue;
        }
        let rest = obj[at + key.len()..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else { continue };
        let rest = rest.trim_start();
        let q = rest.chars().next()?;
        if q != '"' && q != '\'' {
            continue;
        }
        let end = rest[1..].find(q)?;
        return Some(rest[1..1 + end].to_string());
    }
    None
}

/// Remove `/* */` and `//` comments (same as the original lint: string
/// contents are not special-cased).
pub fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            match src[i + 2..].find("*/") {
                Some(j) => {
                    // keep newlines so line numbers stay correct
                    out.extend(src[i..i + 2 + j + 2].bytes().filter(|c| *c == b'\n'));
                    i += 2 + j + 2;
                }
                None => {
                    out.extend_from_slice(&b[i..]);
                    break;
                }
            }
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap_or_default()
}

/// `function <name>` declared in the code (word-bounded name).
fn defines_function(code: &str, name: &str) -> bool {
    let needle = format!("function {}", name);
    code.match_indices(&needle).any(|(i, _)| {
        !code[i + needle.len()..].chars().next().is_some_and(is_word)
    })
}

fn line_of(code: &str, needle: &str) -> Option<usize> {
    code.find(needle).map(|i| code[..i].matches('\n').count() + 1)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `const\s+\w+\s*[:=\w\[\]\. ]*=\s*"20\d{2}-\d{2}-\d{2}` — first match line.
fn find_date_const(code: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(off) = code[start..].find("const") {
        let at = start + off;
        start = at + 5;
        let rest = &code[at + 5..];
        let ws = rest.len() - rest.trim_start().len();
        if ws == 0 {
            continue;
        }
        let rest = &rest[ws..];
        let ident = rest.char_indices().take_while(|(_, c)| is_word(*c)).last().map(|(i, c)| i + c.len_utf8());
        let Some(ident) = ident else { continue };
        let rest = rest[ident..].trim_start();
        let run: Vec<(usize, char)> = rest
            .char_indices()
            .take_while(|(_, c)| is_word(*c) || ":=[]. ".contains(*c))
            .collect();
        for (i, c) in &run {
            if *c == '=' && is_date_literal(rest[i + 1..].trim_start()) {
                return Some(code[..at].matches('\n').count() + 1);
            }
        }
    }
    None
}

fn is_date_literal(s: &str) -> bool {
    let b = s.as_bytes();
    let digit = |i: usize| b.get(i).is_some_and(u8::is_ascii_digit);
    b.len() >= 11
        && b[0] == b'"'
        && b[1] == b'2'
        && b[2] == b'0'
        && digit(3)
        && digit(4)
        && b[5] == b'-'
        && digit(6)
        && digit(7)
        && b[8] == b'-'
        && digit(9)
        && digit(10)
}

/// `\b(for|while)\s*\(`
fn has_loop(code: &str) -> bool {
    for kw in ["for", "while"] {
        let mut start = 0;
        while let Some(off) = code[start..].find(kw) {
            let at = start + off;
            start = at + kw.len();
            let before_ok = code[..at].chars().next_back().map_or(true, |c| !is_word(c));
            if before_ok && code[at + kw.len()..].trim_start().starts_with('(') {
                return true;
            }
        }
    }
    false
}

/// `(同步|共用).{0,12}口径|口径.{0,12}(同步|共用)` within one line.
fn has_linkage_note(src: &str) -> bool {
    let near = |line: &str, a: &str, b: &str| {
        let mut s = 0;
        while let Some(off) = line[s..].find(a) {
            let after = s + off + a.len();
            let window: String = line[after..].chars().take(12 + 2).collect();
            if let Some(j) = window.find(b) {
                if window[..j].chars().count() <= 12 {
                    return true;
                }
            }
            s = after;
        }
        false
    };
    src.lines().any(|l| {
        ["同步", "共用"].iter().any(|k| near(l, k, "口径") || near(l, "口径", k))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(path: &str, src: &str) -> Vec<String> {
        check_script(path, src).into_iter().map(|d| d.rule).collect()
    }

    #[test]
    fn dataflow_script_rules() {
        let ok = "function onProcessData(c) {}\nconst PREVIEW_MAX_ROWS = 200;\nif (context.isPreview) {}";
        assert!(rules("/P/data/script/a.action.ts", ok).is_empty());
        let no_limit = "function onProcessData(c) { output.writeRow([1]); }";
        assert_eq!(rules("/P/data/script/a.action.ts", no_limit), ["script.previewLimit"]);
        // extension-point scripts share data/script but are not DataFlow nodes
        let ext = "function onQueryData(q) {}\nfunction onUpdateData(u) {}";
        assert!(rules("/P/data/script/a.action.ts", ext).is_empty());
        assert!(rules("/P/data/script/a.action.ts", "function onProcessDataX() {}").is_empty());
        // placeholder scripts are exempt; comments are ignored for code rules
        assert!(rules("/P/data/script/a.action.ts", "/** 占位 context.params */").is_empty());
        assert_eq!(
            rules("/P/data/script/a.action.ts", "/*占位*/ let p = context.params;"),
            ["script.params"]
        );
    }

    #[test]
    fn declared_fields_by_dbfield() {
        let src = "let fieldsInfo = [\n  { name: \"会员号\", dataType: FieldDataType.C, dbfield: \"VIPCODE\" },\n  // { name: \"x\", dbfield: \"GONE\" }\n  { dbfield: 'shop', name: '门店' },\n  { tablename: \"t\", dbfield: \"NONAME\" }\n];";
        let d = declared_fields(src);
        assert_eq!(d.get("VIPCODE").map(String::as_str), Some("会员号"));
        assert_eq!(d.get("SHOP").map(String::as_str), Some("门店"));
        assert!(!d.contains_key("GONE") && !d.contains_key("NONAME"));
    }

    #[test]
    fn date_constant_rule() {
        let s = "/*占位*/\nconst START: string = \"2024-01-01\";";
        assert_eq!(rules("/P/data/script/a.action.ts", s), ["script.dateConst"]);
        let s = "/*占位*/\nlet start = \"2024-01-01\";\nconst N = 3;";
        assert!(rules("/P/data/script/a.action.ts", s).is_empty());
    }

    #[test]
    fn batch_and_linkage_rules() {
        assert_eq!(rules("/P/data/batch/b.action.ts", "while (x) {}"), ["script.cancelCheck"]);
        assert!(rules("/P/data/batch/b.action.ts", "while (x) { if (isCanceled()) break; }").is_empty());
        assert!(rules("/P/data/batch/b.action.ts", "before (x)").is_empty());
        let chain = "let BELONGTOID = rootOf(x);";
        assert_eq!(rules("/P/data/batch/b.action.ts", chain), ["script.belongtoLinkage"]);
        let noted = "// 与用户同步脚本共用口径\nlet BELONGTOID = rootOf(x);";
        assert!(rules("/P/data/batch/b.action.ts", noted).is_empty());
    }
}
