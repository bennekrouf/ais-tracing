//! Shared plumbing for the service modules.

use std::path::Path;

/// Writes `contents` to `path` via a temporary file and a rename.
///
/// `fs::write` truncates in place, so a crash or a full disk part-way through
/// leaves a half-written file. That matters more here than the size of these
/// files suggests: `history.json` is one file holding every account's traced
/// values, its recent accounts *and* its error rules — domain knowledge the
/// user typed in by hand — so one bad write loses all of it at once.
///
/// A rename within the same directory is atomic on both platforms this ships
/// to. Best-effort throughout: a history that fails to save is a nuisance,
/// never something to interrupt a trace for.
pub fn atomic_write(path: &Path, contents: &str) {
    let Some(parent) = path.parent() else { return };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    // Alongside the target, so the rename cannot cross a filesystem — and
    // named per process, so two copies of the app racing on the same config
    // collide on the target (where the rename settles it) rather than on the
    // scratch file (where they would interleave into one corrupt write).
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, contents).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

pub mod az;
pub mod cache;
pub mod cosmos;
pub mod discover;
pub mod env;
pub mod history;
pub mod trace;

#[cfg(test)]
mod tests {
    use super::atomic_write;

    #[test]
    fn a_write_creates_the_directory_and_replaces_what_was_there() {
        let dir = std::env::temp_dir().join(format!("ais-tracing-test-{}", std::process::id()));
        let path = dir.join("nested").join("history.json");
        let _ = std::fs::remove_dir_all(&dir);

        atomic_write(&path, "first");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");

        atomic_write(&path, "second");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");

        // The scratch file must not be left lying next to the real one.
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
