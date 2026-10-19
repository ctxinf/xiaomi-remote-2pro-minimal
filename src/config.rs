//! JSONC reader shared by gateway and hook configuration.
use anyhow::Context;
use std::path::Path;

/// Accept JSON with line/block comments. Preserve offsets for useful errors.
pub fn read(path: &Path) -> anyhow::Result<serde_json::Value> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("Cannot read configuration {}", path.display()))?;
    let mut bytes = text.into_bytes();
    let mut i = 0;
    let mut quoted = false;
    while i < bytes.len() {
        if quoted {
            if bytes[i] == b'\\' {
                i += 2;
                continue;
            }
            if bytes[i] == b'"' {
                quoted = false;
            }
        } else if bytes[i] == b'"' {
            quoted = true;
        } else if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                bytes[i] = b' ';
                i += 1;
            }
            continue;
        } else if bytes[i..].starts_with(b"/*") {
            bytes[i] = b' ';
            bytes[i + 1] = b' ';
            i += 2;
            while i < bytes.len() && !bytes[i..].starts_with(b"*/") {
                if bytes[i] != b'\n' && bytes[i] != b'\r' {
                    bytes[i] = b' ';
                }
                i += 1;
            }
            anyhow::ensure!(
                i < bytes.len(),
                "Unterminated comment in {}",
                path.display()
            );
            bytes[i] = b' ';
            bytes[i + 1] = b' ';
            i += 2;
            continue;
        }
        i += 1;
    }
    serde_json::from_slice(&bytes).with_context(|| format!("Invalid JSONC in {}", path.display()))
}
