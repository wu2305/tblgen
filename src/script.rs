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
    let mut out = std::collections::HashMap::new();
    let mut objects: Vec<(Option<String>, Option<String>)> = Vec::new();
    let mut tokens = source_tokens(src)
        .filter(|token| !matches!(token, Token::Comment(_) | Token::Whitespace(_)))
        .peekable();
    while let Some(token) = tokens.next() {
        match token {
            Token::Code("{") => objects.push((None, None)),
            Token::Code("}") => {
                if let Some((Some(name), Some(dbfield))) = objects.pop() {
                    out.insert(dbfield.to_uppercase(), name);
                }
            }
            Token::Code(key @ ("name" | "dbfield")) => {
                if let Some((name, dbfield)) = objects.last_mut() {
                    let value = if key == "name" { name } else { dbfield };
                    if value.is_none() && matches!(tokens.peek(), Some(Token::Code(":"))) {
                        tokens.next();
                        if let Some(Token::Quoted(literal)) = tokens.peek() {
                            *value = string_value(literal);
                            tokens.next();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Generated literals use JSON escapes. Normalize quoting and raw control
/// characters from hand-written scripts before using the same decoder.
fn string_value(literal: &str) -> Option<String> {
    let quote = match literal.chars().next()? {
        quote @ ('"' | '\'') => quote,
        _ => return None,
    };
    if quote == '"' {
        if let Ok(decoded) = serde_json::from_str(literal) {
            return Some(decoded);
        }
    }
    let body = literal.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut json = String::with_capacity(literal.len());
    json.push('"');
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => json.push_str(r#"\""#),
            '\\' => {
                let escaped = chars.next()?;
                if escaped != '\'' {
                    json.push('\\');
                }
                json.push(escaped);
            }
            c if c < '\u{20}' => json.push_str(&format!(r"\u{:04x}", c as u32)),
            _ => json.push(c),
        }
    }
    json.push('"');
    serde_json::from_str(&json).ok()
}

enum Token<'a> {
    Code(&'a str),
    Whitespace(&'a str),
    Quoted(&'a str),
    Comment(&'a str),
}

// Keep quotes and comments together so braces and property names inside
// them are opaque to the field scanner.
fn source_tokens<'a>(mut src: &'a str) -> impl Iterator<Item = Token<'a>> {
    std::iter::from_fn(move || {
        let first = src.chars().next()?;
        let (len, kind): (usize, fn(&'a str) -> Token<'a>) = match first {
            '"' | '\'' | '`' => (quoted_len(src, first), Token::Quoted),
            '/' if src.starts_with("/*") => {
                (src[2..].find("*/").map_or(src.len(), |i| i + 4), Token::Comment)
            }
            '/' if src.starts_with("//") => {
                (src.find(['\r', '\n', '\u{2028}', '\u{2029}']).unwrap_or(src.len()), Token::Comment)
            }
            c if c.is_whitespace() => {
                (src.find(|c: char| !c.is_whitespace()).unwrap_or(src.len()), Token::Whitespace)
            }
            c if is_word(c) || c == '$' => {
                (src.find(|c| !is_word(c) && c != '$').unwrap_or(src.len()), Token::Code)
            }
            c => (c.len_utf8(), Token::Code),
        };
        let (token, rest) = src.split_at(len);
        src = rest;
        Some(kind(token))
    })
}

fn quoted_len(src: &str, quote: char) -> usize {
    let mut chars = src[1..].char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            c if c == quote => return i + 2,
            '\r' | '\n' if quote != '`' => return i + 1,
            '\\' => {
                if matches!(chars.next(), Some((_, '\r'))) && matches!(chars.peek(), Some((_, '\n'))) {
                    chars.next();
                }
            }
            _ => {}
        }
    }
    src.len()
}

/// Remove comments while preserving quoted text and line numbers.
pub fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for token in source_tokens(src) {
        match token {
            Token::Code(text) | Token::Whitespace(text) | Token::Quoted(text) => out.push_str(text),
            Token::Comment(text) => out.extend(text.chars().filter(|c| *c == '\n')),
        }
    }
    out
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
    fn field_scanner_distinguishes_literals_from_syntax() {
        let src = r#"let fieldsInfo = [
  { hint: "{ name: '错误', dbfield: 'FAKE' }", name: '门\'店 "A" \\ //', dbfield: 'shop' },
  /* { name: "注释", dbfield: "GONE" } */
  { name: "会员\n号\u2028 // /* */ {fields} \"A\"", dbfield: "\u0056ip" },
];"#;
        let fields = declared_fields(src);
        assert_eq!(fields.len(), 2);
        assert_eq!(fields["SHOP"], "门'店 \"A\" \\ //");
        assert_eq!(fields["VIP"], "会员\n号\u{2028} // /* */ {fields} \"A\"");
        assert_eq!(string_value("\"A\tB\"").as_deref(), Some("A\tB"));
        let code = strip_comments(src);
        assert!(!code.contains("GONE"));
        assert!(code.contains(r#"门\'店 "A" \\ //"#));
        assert!(code.contains("// /* */ {fields}"));
        assert_eq!(code.lines().count(), src.lines().count());
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
