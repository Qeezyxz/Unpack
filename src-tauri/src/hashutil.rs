use sha1::{Digest, Sha1};
use sha2::Sha256;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_file(path: &Path) -> Result<String, String> {
    hash_file::<Sha256>(path)
}

pub fn sha1_file(path: &Path) -> Result<String, String> {
    hash_file::<Sha1>(path)
}

fn hash_file<D: Digest>(path: &Path) -> Result<String, String> {
    let file = File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut hasher = D::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(&hasher.finalize()))
}

/// Accepts a SHA-1 (40 hex) or SHA-256 (64 hex) value, with an optional
/// `sha1:` / `sha256:` label and any spaces. `None` means nothing to check.
pub fn normalize_hash(expected: &str) -> Result<Option<String>, String> {
    let lower = expected.trim().to_lowercase();
    let value = lower
        .strip_prefix("sha256:")
        .or_else(|| lower.strip_prefix("sha-256:"))
        .or_else(|| lower.strip_prefix("sha1:"))
        .or_else(|| lower.strip_prefix("sha-1:"))
        .unwrap_or(&lower);
    let value: String = value.chars().filter(|c| !c.is_whitespace()).collect();
    if value.is_empty() {
        return Ok(None);
    }
    if !value.chars().all(|c| c.is_ascii_hexdigit()) || !matches!(value.len(), 40 | 64) {
        return Err(format!(
            "That is not a SHA-1 (40 characters) or SHA-256 (64 characters) hash; it has {} characters. Fix it or clear the box.",
            value.len()
        ));
    }
    Ok(Some(value))
}

pub fn verify_hash(path: &Path, expected: &str) -> Result<String, String> {
    let Some(expected) = normalize_hash(expected)? else {
        return sha256_file(path);
    };
    let actual = if expected.len() == 40 {
        sha1_file(path)?
    } else {
        sha256_file(path)?
    };
    if actual != expected {
        return Err(format!(
            "hash mismatch for {}: expected {expected}, got {actual}",
            path.display()
        ));
    }
    Ok(actual)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn hashes_known_bytes() {
        let dir = std::env::temp_dir().join("unpack-hash-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("hello.txt");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"hello")
            .unwrap();
        assert_eq!(
            sha1_file(&path).unwrap(),
            "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d"
        );
        assert_eq!(
            sha256_file(&path).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(verify_hash(&path, "aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d").is_ok());
        assert!(verify_hash(&path, "deadbeef").unwrap_err().contains("not a SHA-1"));
        assert!(verify_hash(&path, "SHA1: AAF4C61DDCC5E8A2DABEDE0F3B482CD9AEA9434D").is_ok());
        assert!(verify_hash(&path, &"z".repeat(40)).is_err());
        assert!(verify_hash(&path, &"0".repeat(64)).unwrap_err().contains("mismatch"));
        assert_eq!(normalize_hash("   ").unwrap(), None);
    }
}
