//! Retention policy for recorded audio files.
//!
//! The `end` hook owns each recording and is expected to consume it, but files
//! are left behind whenever there is no hook, the hook fails, or the take was
//! too short to fire one. This module prunes the gateway's own record directory
//! so those leftovers cannot grow without bound.
//!
//! Only files matching the gateway's own `record_*.wav` naming are considered,
//! so pointing `--record-dir` at a directory holding other data is safe.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long to keep recordings, and how many.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Delete files older than this. `None` disables age-based pruning.
    pub max_age: Option<Duration>,
    /// Keep at most this many of the newest files. `None` disables count pruning.
    pub max_files: Option<usize>,
}

impl RetentionPolicy {
    /// Builds a policy from CLI values, where `0` means "unlimited".
    pub fn from_args(keep_hours: u64, keep_max: usize) -> Self {
        Self {
            max_age: (keep_hours > 0).then(|| Duration::from_secs(keep_hours * 3600)),
            max_files: (keep_max > 0).then_some(keep_max),
        }
    }

    pub fn is_disabled(&self) -> bool {
        self.max_age.is_none() && self.max_files.is_none()
    }

    pub fn describe(&self) -> String {
        match (self.max_age, self.max_files) {
            (None, None) => "disabled (files kept forever)".to_string(),
            (Some(age), None) => format!("delete after {}h", age.as_secs() / 3600),
            (None, Some(max)) => format!("keep newest {max}"),
            (Some(age), Some(max)) => {
                format!("delete after {}h, keep newest {max}", age.as_secs() / 3600)
            }
        }
    }
}

/// Files the gateway created, newest first.
fn collect(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<(PathBuf, SystemTime)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            // Restrict to our own output so an unrelated --record-dir is untouched.
            if !name.starts_with("record_") || !name.ends_with(".wav") {
                return None;
            }
            let meta = entry.metadata().ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta.modified().ok()?;
            Some((path, modified))
        })
        .collect();
    files.sort_by(|a, b| b.1.cmp(&a.1));
    files
}

/// Applies `policy` to `dir`, deleting the recordings it selects.
///
/// `keep` is never deleted, so a sweep triggered right after a recording cannot
/// remove the file whose `end` hook is still running.
///
/// Returns the number of files deleted.
pub fn sweep(dir: &Path, policy: &RetentionPolicy, keep: Option<&Path>) -> usize {
    if policy.is_disabled() {
        return 0;
    }

    let files = collect(dir);
    let now = SystemTime::now();
    let mut deleted = 0;

    for (index, (path, modified)) in files.iter().enumerate() {
        if keep.is_some_and(|k| k == path) {
            continue;
        }

        let too_old = policy.max_age.is_some_and(|max_age| {
            now.duration_since(*modified).is_ok_and(|age| age > max_age)
        });
        // `files` is newest-first, so anything at or past the limit is surplus.
        let surplus = policy.max_files.is_some_and(|max| index >= max);

        if !(too_old || surplus) {
            continue;
        }

        match std::fs::remove_file(path) {
            Ok(()) => {
                log::debug!(
                    "[RETENTION] Removed {} ({})",
                    path.display(),
                    if too_old { "expired" } else { "over limit" }
                );
                deleted += 1;
            }
            Err(e) => log::warn!("[RETENTION] Could not remove {}: {e}", path.display()),
        }
    }

    if deleted > 0 {
        log::info!("[RETENTION] Cleaned up {deleted} old recording(s) in {}", dir.display());
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_args_treats_zero_as_unlimited() {
        assert_eq!(
            RetentionPolicy::from_args(0, 0),
            RetentionPolicy { max_age: None, max_files: None }
        );
        assert!(RetentionPolicy::from_args(0, 0).is_disabled());
        assert_eq!(
            RetentionPolicy::from_args(24, 100),
            RetentionPolicy {
                max_age: Some(Duration::from_secs(86_400)),
                max_files: Some(100),
            }
        );
    }
}
