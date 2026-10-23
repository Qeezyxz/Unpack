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

pub fn verify_hash(path: &Path, expected: &str) -> Result<String, String> {
    let expected = expected.trim().to_lowercase();
    let actual = if expected.len() == 40 {
        sha1_file(path)?
    } else {
        sha256_file(path)?
    };
    if !expected.is_empty() && actual != expected {
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
        assert!(verify_hash(&path, "deadbeef").is_err());
    }
}
