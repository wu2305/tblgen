//! Project layout on disk and cross-file reference resolution.
//!
//! Platform resource paths look like `/<project>/data/tables/a.tbl`. On disk a
//! project is a directory that contains `data/` (apps nest their own `data/`
//! under `<name>.app/`, which is not a project). References between models are
//! resolved with the rules of `MetaUtils#resolveReferencePath`.

use std::io;
use std::path::{Component, Path, PathBuf};

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
    locate_abs(std::fs::canonicalize(file).ok()?)
}

/// Like `locate`, for a file that may not exist yet (its directory must).
pub fn locate_new(file: &Path) -> Option<Located> {
    let abs = normalized_new_path(file).ok()?;
    locate_abs(abs.clone()).or_else(|| prospective_locate(&abs))
}

fn is_app_dir(name: &str) -> bool {
    name.ends_with(".app")
}

/// Locate a project for a path whose directories do not exist yet: the
/// nearest lexical ancestor literally named `data` with a non-`.app`
/// parent is treated as the prospective `<project>/data` root, matching
/// what `locate_abs` would find after the directories are created.
fn prospective_locate(abs: &Path) -> Option<Located> {
    for anc in abs.ancestors() {
        if anc.file_name()?.to_string_lossy() != "data" {
            continue;
        }
        let root = anc.parent()?;
        let name = root.file_name()?.to_string_lossy();
        if is_app_dir(&name) || name.is_empty() {
            continue;
        }
        let rel = abs.strip_prefix(root).ok()?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let rel: Vec<String> = rel
            .iter()
            .map(|s| s.to_string_lossy().into_owned())
            .collect();
        return Some(Located {
            projects_root: root.parent()?.to_path_buf(),
            platform_path: format!("/{}/{}", name, rel.join("/")),
        });
    }
    None
}

/// Normalize a not-yet-existing write destination to an absolute path:
/// canonicalize every existing component (resolving symlink and case
/// aliases in parents), fold `.`/`..` across components that may not exist
/// yet, preserve non-UTF8 segments, and fail when an existing component is
/// a dangling symlink or otherwise unresolvable. The result is safe to
/// compare for duplicates and to create exclusively.
pub fn normalized_new_path(path: &Path) -> io::Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut resolved = PathBuf::new();
    // literal tail of not-yet-created components (no symlinks inside)
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    for comp in abs.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir => resolved.push(comp.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if tail.pop().is_none() {
                    // `resolved` is canonical: popping its last component is
                    // the true parent (root pop is a no-op)
                    resolved.pop();
                }
            }
            Component::Normal(seg) => {
                if !tail.is_empty() {
                    tail.push(seg.to_os_string());
                    continue;
                }
                let cand = resolved.join(seg);
                match std::fs::canonicalize(&cand) {
                    Ok(c) => resolved = c,
                    Err(_) => {
                        // something exists here but cannot be resolved
                        // (dangling symlink, unreadable parent): hard error
                        if std::fs::symlink_metadata(&cand).is_ok() {
                            return Err(io::Error::new(
                                io::ErrorKind::NotFound,
                                format!("cannot resolve path component {}", cand.display()),
                            ));
                        }
                        tail.push(seg.to_os_string());
                    }
                }
            }
        }
    }
    let mut out = resolved;
    for seg in tail {
        out.push(seg);
    }
    Ok(out)
}

fn locate_abs(abs: PathBuf) -> Option<Located> {
    let mut dir = abs.parent();
    while let Some(d) = dir {
        let name = d.file_name()?.to_string_lossy();
        if !is_app_dir(&name) && d.join("data").is_dir() {
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
fn parent_app_path(mut path: &str) -> Option<&str> {
    while let Some((parent, name)) = path.rsplit_once('/') {
        if is_app_dir(name) {
            return Some(path);
        }
        path = parent;
    }
    is_app_dir(path).then_some(path)
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

pub(crate) enum ScriptSource {
    Found(String),
    Missing,
    Unreadable(Vec<(PathBuf, io::Error)>),
}

pub(crate) fn read_script(projects_root: &Path, project: &str, script_name: &str) -> ScriptSource {
    let mut errors = Vec::new();
    // Prefer readable TypeScript source, falling back to the compiled script.
    for path in script_candidates(projects_root, project, script_name).into_iter().rev() {
        match std::fs::read_to_string(&path) {
            Ok(text) => return ScriptSource::Found(text),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => errors.push((path, error)),
        }
    }
    if errors.is_empty() {
        ScriptSource::Missing
    } else {
        ScriptSource::Unreadable(errors)
    }
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

    #[test]
    fn resolves_tapp_to_nearest_app_directory() {
        let from = "/P/app/outer.app/data/内层.app/data/x.tbl";
        assert_eq!(
            resolve_reference(from, "$tapp:/data/y.tbl").as_deref(),
            Some("/P/app/outer.app/data/内层.app/data/y.tbl")
        );

        let from = "/P/app/outer.app/data/x.app.bak/foo.apple//x.tbl";
        assert_eq!(
            resolve_reference(from, "$TAPP:/data/y.tbl").as_deref(),
            Some("/P/app/outer.app/data/y.tbl")
        );
        assert_eq!(
            resolve_reference(from, "$tapp:tail").as_deref(),
            Some("/P/app/outer.apptail")
        );
    }

    #[test]
    fn tapp_requires_an_app_directory() {
        for from in [
            "/P/data/foo.apple/x.tbl",
            "/P/data/x.app.bak/x.tbl",
            "/P/data/outer.APP/x.tbl",
            r"/P/data/outer.app\ordinary/x.tbl",
            "/P/data/x.app",
        ] {
            assert_eq!(resolve_reference(from, "$tapp:/data/y.tbl"), None, "{from}");
        }
    }

    #[test]
    fn normalized_new_path_folds_and_resolves() {
        let dir = std::env::temp_dir().join(format!("tblgen-proj-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        // ./ and .. spellings of the same not-yet-created target normalize equal
        let a = normalized_new_path(&dir.join("real/../real/x.tbl")).unwrap();
        let b = normalized_new_path(&dir.join("./real/x.tbl")).unwrap();
        assert_eq!(a, b);
        // a symlinked parent resolves to the canonical target
        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&real, &link).unwrap();
            let c = normalized_new_path(&link.join("y.tbl")).unwrap();
            let real_c = std::fs::canonicalize(&real).unwrap();
            assert_eq!(c, real_c.join("y.tbl"));
            // dangling symlink component -> hard error
            let dangling = dir.join("dangling");
            std::os::unix::fs::symlink(dir.join("missing"), &dangling).unwrap();
            assert!(normalized_new_path(&dangling.join("z.tbl")).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
