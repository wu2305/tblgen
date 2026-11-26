use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde_json::Value;

pub(super) fn json_text(value: &Value) -> Result<String, String> {
    let mut text = serde_json::to_string_pretty(value).map_err(|e| format!("serialize: {}", e))?;
    text.push('\n');
    Ok(text)
}

pub(super) fn write_out(path: Option<&str>, value: &Value) -> Result<(), String> {
    let text = json_text(value)?;
    match path {
        Some(path) => write_exclusive_all(&[(Path::new(path), text)]),
        None => {
            print!("{}", text);
            Ok(())
        }
    }
}

/// Reserve every target before writing and roll back this invocation's files on I/O failure.
/// Callers prepare paths and parent directories according to their command's rules.
pub(super) fn write_exclusive_all(targets: &[(impl AsRef<Path>, String)]) -> Result<(), String> {
    let mut reserved = Vec::with_capacity(targets.len());
    let result: Result<(), String> = (|| {
        for (path, _) in targets {
            let path = path.as_ref();
            let file = File::options()
                .write(true)
                .create_new(true)
                .open(path)
                .map_err(|e| {
                    if e.kind() == io::ErrorKind::AlreadyExists {
                        format!("{}: already exists (use a fresh path)", path.display())
                    } else {
                        format!("{}: {}", path.display(), e)
                    }
                })?;
            reserved.push((path, file));
        }
        for ((path, file), (_, text)) in reserved.iter_mut().zip(targets) {
            file.write_all(text.as_bytes())
                .map_err(|e| format!("{}: {}", path.display(), e))?;
        }
        Ok(())
    })();

    if let Err(mut error) = result {
        for (path, file) in reserved.into_iter().rev() {
            drop(file);
            if let Err(e) = fs::remove_file(path) {
                error.push_str(&format!("\n{}: rollback failed: {}", path.display(), e));
            }
        }
        return Err(error);
    }

    for (path, file) in reserved {
        drop(file);
        eprintln!("tblgen: wrote {}", path.display());
    }
    Ok(())
}
