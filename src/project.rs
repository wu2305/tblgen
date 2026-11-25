//! Project layout on disk and cross-file reference resolution.
//!
//! Platform resource paths look like `/<project>/data/tables/a.tbl`. On disk a
//! project is a directory that contains `data/` (apps nest their own `data/`
//! under `<name>.app/`, which is not a project). References between models are
//! resolved with the rules of `MetaUtils#resolveReferencePath`.

use std::path::{Path, PathBuf};

/// A file located inside a project directory.
#[derive(Debug, Clone)]
pub struct Located {
    /// directory holding all projects (parent of the project directory)
    pub projects_root: PathBuf,
    /// platform path of the file, e.g. `/DEV/data/tables/ads/x.tbl`
    pub platform_path: String,
}

/// Find the project that contains `file` and its platform path, or `None`
/// when the file is not inside a project layout.
pub fn locate(file: &Path) -> Option<Located> {
    let abs = std::fs::canonicalize(file).ok()?;
    let mut dir = abs.parent();
    while let Some(d) = dir {
        let name = d.file_name()?.to_string_lossy();
        if !name.ends_with(".app") && d.join("data").is_dir() {
            let rel = abs.strip_prefix(d).ok()?;
            let rel: Vec<String> = rel.iter().map(|s| s.to_string_lossy().into_owned()).collect();
            return Some(Located {
                projects_root: d.parent()?.to_path_buf(),
                platform_path: format!("/{}/{}", name, rel.join("/")),
            });
        }
        dir = d.parent();
    }
    None
}

/// Project name of a platform path (`/DEV/data/...` -> `DEV`).
pub fn project_name(platform_path: &str) -> &str {
    platform_path.trim_start_matches('/').split('/').next().unwrap_or("")
}

fn parent_path(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => "",
    }
}

/// Join and normalise `.`/`..` segments (FilenameUtils#resolvePath).
fn resolve_path(base: &str, rel: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let joined = if rel.starts_with('/') { rel.to_string() } else { format!("{}/{}", base, rel) };
    for seg in joined.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

/// `MetaUtils#getParentAppPath`: the enclosing `<name>.app` path, if any.
fn parent_app_path(p: &str) -> Option<&str> {
    p.rfind(".app").map(|i| &p[..i + 4])
}

/// Resolve `reference` as written in the model at `from_file`
/// (`MetaUtils#resolveDwReferencePath`). Returns a platform path.
pub fn resolve_reference(from_file: &str, reference: &str) -> Option<String> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    let from_dir = parent_path(from_file);
    if reference.starts_with('/') {
        return Some(reference.to_string());
    }
    if let Some(rest) = reference.strip_prefix('$') {
        let i = rest.find(':')?;
        let module = rest[..i].to_lowercase();
        let end = &rest[i + 1..];
        let module_path = match module.as_str() {
            "projectimg" => "settings/attachments/library",
            "innerimg" => "settings/attachments/private",
            "data" => "data/tables",
            "userdata" => "data/users",
            "tapp" => return parent_app_path(from_dir).map(|a| format!("{}{}", a, end)),
            other => other,
        };
        return Some(format!("/{}/{}{}", project_name(from_file), module_path, end));
    }
    Some(resolve_path(from_dir, reference))
}

/// Disk path of a platform path.
pub fn to_disk(projects_root: &Path, platform_path: &str) -> PathBuf {
    let mut p = projects_root.to_path_buf();
    for seg in platform_path.trim_start_matches('/').split('/') {
        p.push(seg);
    }
    p
}

/// Server script candidates for a Script node (`DwServiceMeta#getServerScriptPath`):
/// the compiled `.action` the runner loads, or its `.action.ts` source.
pub fn script_candidates(projects_root: &Path, project: &str, script_name: &str) -> [PathBuf; 2] {
    let base = format!("/{}/data/script/{}", project, script_name);
    [
        to_disk(projects_root, &format!("{}.action", base)),
        to_disk(projects_root, &format!("{}.action.ts", base)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_platform_reference_forms() {
        let from = "/P/data/tables/a/b/x.tbl";
        assert_eq!(resolve_reference(from, "../ods/y.tbl").unwrap(), "/P/data/tables/a/ods/y.tbl");
        assert_eq!(resolve_reference(from, "y.tbl").unwrap(), "/P/data/tables/a/b/y.tbl");
        assert_eq!(resolve_reference(from, "$DATA:/m/y.tbl").unwrap(), "/P/data/tables/m/y.tbl");
        assert_eq!(resolve_reference(from, "$APP:/s.app/data/y.tbl").unwrap(), "/P/app/s.app/data/y.tbl");
        assert_eq!(resolve_reference(from, "/Q/data/tables/y.tbl").unwrap(), "/Q/data/tables/y.tbl");
        let in_app = "/P/app/s.app/data/x.tbl";
        assert_eq!(resolve_reference(in_app, "$TAPP:/data/y.tbl").unwrap(), "/P/app/s.app/data/y.tbl");
    }
}
