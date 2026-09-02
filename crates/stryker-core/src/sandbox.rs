//! In-place sandbox: back up originals, write instrumented text atomically,
//! restore on completion / drop / crash.
//!
//! Only files that actually contain mutants are touched (schemata means the
//! rest of the tree is never rewritten). The backup manifest is written and
//! flushed BEFORE the first file is modified, so `stryker restore` can always
//! recover.

use std::collections::BTreeMap;
use std::io::Write;

use camino::{Utf8Path, Utf8PathBuf};
use serde::{Deserialize, Serialize};

pub const MANIFEST_NAME: &str = "backup-manifest.json";

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    /// rel path in project -> rel path under <temp>/backup/
    files: BTreeMap<Utf8PathBuf, Utf8PathBuf>,
}

pub struct InPlaceSandbox {
    root: Utf8PathBuf,
    temp_dir: Utf8PathBuf,
    manifest: Manifest,
    restored: bool,
}

impl InPlaceSandbox {
    /// Back up `files` (project-relative) and replace their contents.
    pub fn activate(
        root: &Utf8Path,
        temp_dir: &Utf8Path,
        files: &[(Utf8PathBuf, String)],
    ) -> anyhow::Result<Self> {
        let backup_root = temp_dir.join("backup");
        std::fs::create_dir_all(&backup_root)?;

        // Phase 1: copy every original into the backup dir.
        let mut manifest = Manifest { files: BTreeMap::new() };
        for (rel, _) in files {
            let backup_rel = Utf8PathBuf::from("backup").join(rel);
            let backup_abs = temp_dir.join(&backup_rel);
            if let Some(parent) = backup_abs.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(root.join(rel), &backup_abs)?;
            manifest.files.insert(rel.clone(), backup_rel);
        }

        // Phase 2: persist + fsync the manifest before touching anything.
        let manifest_path = temp_dir.join(MANIFEST_NAME);
        let mut f = std::fs::File::create(&manifest_path)?;
        f.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
        f.sync_all()?;

        // Phase 3: swap in the instrumented contents (atomic per file).
        for (rel, content) in files {
            write_atomic(&root.join(rel), content)?;
        }

        Ok(Self {
            root: root.to_owned(),
            temp_dir: temp_dir.to_owned(),
            manifest,
            restored: false,
        })
    }

    pub fn restore(&mut self) -> anyhow::Result<()> {
        if self.restored {
            return Ok(());
        }
        let mut first_error = None;
        for (rel, backup_rel) in &self.manifest.files {
            let result = std::fs::copy(self.temp_dir.join(backup_rel), self.root.join(rel));
            if let Err(e) = result {
                tracing::error!("failed to restore {rel}: {e}");
                first_error.get_or_insert(anyhow::anyhow!("failed to restore {rel}: {e}"));
            }
        }
        if first_error.is_none() {
            self.restored = true;
            let _ = std::fs::remove_file(self.temp_dir.join(MANIFEST_NAME));
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for InPlaceSandbox {
    fn drop(&mut self) {
        if !self.restored {
            tracing::warn!("restoring mutated files from Drop (abnormal exit path)");
            let _ = self.restore();
        }
    }
}

/// Standalone recovery: restore from a manifest left behind by a crashed run.
pub fn restore_from_manifest(root: &Utf8Path, temp_dir: &Utf8Path) -> anyhow::Result<usize> {
    let manifest_path = temp_dir.join(MANIFEST_NAME);
    if !manifest_path.exists() {
        return Ok(0);
    }
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(&manifest_path)?)?;
    let mut restored = 0;
    for (rel, backup_rel) in &manifest.files {
        std::fs::copy(temp_dir.join(backup_rel), root.join(rel))?;
        restored += 1;
    }
    std::fs::remove_file(&manifest_path)?;
    Ok(restored)
}

/// Second-pass safety net for `stryker restore`: find files still carrying
/// the instrumentation header (e.g. manifest lost) so the user can see them.
pub fn find_instrumented_leftovers(
    root: &Utf8Path,
    files: impl Iterator<Item = Utf8PathBuf>,
    header_marker: &str,
) -> Vec<Utf8PathBuf> {
    files
        .filter(|rel| {
            std::fs::read_to_string(root.join(rel))
                .is_ok_and(|content| content.contains(header_marker))
        })
        .collect()
}

/// Refuse to mutate files with uncommitted changes: a crash + failed restore
/// would destroy the user's work. Best-effort — absent git means no check.
pub fn dirty_files(root: &Utf8Path, files: &[Utf8PathBuf]) -> Vec<Utf8PathBuf> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root.as_str())
        .args(["status", "--porcelain", "--"])
        .args(files.iter().map(|f| f.as_str()))
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new(); // not a git repo
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        // Untracked files ("??") are fine: our own backup covers them and
        // there is no committed state to clobber.
        .filter(|line| !line.starts_with("??"))
        .filter_map(|line| line.get(3..).map(Utf8PathBuf::from))
        .collect()
}

fn write_atomic(path: &Utf8Path, content: &str) -> anyhow::Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("no parent for {path}"))?;
    let tmp = dir.join(format!(".{}.stryker-tmp", path.file_name().unwrap_or("file")));
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activate_and_restore_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "original").unwrap();
        let temp = root.join(".stryker-tmp");
        std::fs::create_dir_all(&temp).unwrap();

        let mut sandbox = InPlaceSandbox::activate(
            root,
            &temp,
            &[(Utf8PathBuf::from("src/a.ts"), "instrumented".to_string())],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(root.join("src/a.ts")).unwrap(), "instrumented");
        sandbox.restore().unwrap();
        assert_eq!(std::fs::read_to_string(root.join("src/a.ts")).unwrap(), "original");
    }

    #[test]
    fn crash_recovery_via_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "original").unwrap();
        let temp = root.join(".stryker-tmp");
        std::fs::create_dir_all(&temp).unwrap();

        let sandbox = InPlaceSandbox::activate(
            root,
            &temp,
            &[(Utf8PathBuf::from("src/a.ts"), "instrumented".to_string())],
        )
        .unwrap();
        std::mem::forget(sandbox); // simulate SIGKILL: no Drop, manifest left behind

        let restored = restore_from_manifest(root, &temp).unwrap();
        assert_eq!(restored, 1);
        assert_eq!(std::fs::read_to_string(root.join("src/a.ts")).unwrap(), "original");
        assert_eq!(restore_from_manifest(root, &temp).unwrap(), 0); // idempotent
    }
}
