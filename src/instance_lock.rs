//! Keep two gateway processes from driving the same user's remote concurrently.
#[cfg(target_os = "linux")]
pub fn acquire() -> anyhow::Result<std::fs::File> {
    use anyhow::Context;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};

    // Fixed location: changing record_dir or TMPDIR must not bypass the lock.
    let uid = std::fs::metadata("/proc/self")?.uid();
    let dir = std::path::PathBuf::from(format!("/tmp/xiaomi-remote-gateway-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).context("Cannot create gateway lock directory"),
    }
    let metadata = std::fs::symlink_metadata(&dir)?;
    anyhow::ensure!(
        metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o077 == 0,
        "Gateway lock directory {} must be owned by you with mode 700",
        dir.display()
    );
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join("instance.lock"))?;
    file.try_lock().context(
        "Cannot acquire gateway instance lock. Stop the existing gateway/systemd service before starting another instance",
    )?;
    log::debug!(
        "[GATEWAY] Instance lock acquired (pid={})",
        std::process::id()
    );
    // Do not unlink: the kernel releases the lock on close, including crashes.
    Ok(file)
}

#[cfg(not(target_os = "linux"))]
pub fn acquire() -> anyhow::Result<()> {
    Ok(())
}
