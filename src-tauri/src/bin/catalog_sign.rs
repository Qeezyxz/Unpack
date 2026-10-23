//! unpack-catalog-sign: make a keypair, sign a catalog, verify a signature.
//! The catalog owner keeps the secret key; users pin the public key in Unpack.
//!
//!   unpack-catalog-sign keygen <secret-key-file>
//!   unpack-catalog-sign pubkey <secret-key-file | ->
//!   unpack-catalog-sign sign <secret-key-file | -> <catalog.json> [signature-file]
//!
//! The secret key is only ever read from a file or from stdin (`-`), never taken as a literal
//! argument, so it can't land in shell history or process listings (R12 LOW-5). keygen makes the
//! file owner-only: mode 0600 on Unix; on Windows inheritance is removed and only the current
//! user is granted access (icacls), with a warning if that fails.
//!   unpack-catalog-sign verify <public-key-hex | public-key-file> <catalog.json> [signature-file]
//!   unpack-catalog-sign check <catalog.json>        (schema check only, no signature)

use std::io::Read;
use std::process::ExitCode;

/// Public key: hex literal or a file holding it.
fn read_key(arg: &str) -> Result<String, String> {
    let t = if std::path::Path::new(arg).is_file() { std::fs::read_to_string(arg).map_err(|e| format!("{arg}: {e}"))? } else { arg.to_string() };
    Ok(t.split_whitespace().next().unwrap_or("").to_string())
}

/// Secret key: a file, or `-` for stdin. A literal key is refused (and never echoed).
fn read_secret(arg: &str) -> Result<String, String> {
    let t = if arg == "-" {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin().take(4096), &mut s).map_err(|e| format!("stdin: {e}"))?;
        s
    } else if std::path::Path::new(arg).is_file() {
        std::fs::read_to_string(arg).map_err(|e| format!("Cannot read the secret key file: {e}"))?
    } else if arg.len() == 64 && arg.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Give the secret key as a file (or - to read it from stdin), not on the command line.".into());
    } else {
        return Err("Secret key file not found.".into());
    };
    Ok(t.split_whitespace().next().unwrap_or("").to_string())
}

#[cfg(windows)]
fn restrict_acl(path: &str) -> Option<String> {
    let user = std::env::var("USERNAME").ok()?;
    let ok = std::process::Command::new("icacls").arg(path).args(["/inheritance:r", "/grant:r"]).arg(format!("{user}:F")).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().map(|s| s.success()).unwrap_or(false);
    if ok { None } else { Some(format!("WARNING: couldn't restrict access to {path} (icacls). Keep it in a private folder.")) }
}
#[cfg(not(windows))]
fn restrict_acl(_: &str) -> Option<String> { None }

fn run(args: &[String]) -> Result<String, String> {
    let usage = "usage:\n  unpack-catalog-sign keygen <secret-key-file>\n  unpack-catalog-sign pubkey <secret-key-file | ->\n  unpack-catalog-sign sign <secret-key-file | -> <catalog.json> [signature-file]\n  unpack-catalog-sign verify <public-key> <catalog.json> [signature-file]\n  unpack-catalog-sign check <catalog.json>";
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    match a.as_slice() {
        ["keygen", out] => {
            if std::path::Path::new(out).exists() { return Err(format!("{out} already exists; not overwriting a key.")); }
            let (secret, public) = unpack_lib::catalog::keygen()?;
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            { use std::os::unix::fs::OpenOptionsExt; o.mode(0o600); }
            use std::io::Write;
            o.open(out).and_then(|mut f| f.write_all(format!("{secret}\n").as_bytes())).map_err(|e| format!("{out}: {e}"))?;
            let warn = restrict_acl(out).map(|w| format!("{w}\n")).unwrap_or_default();
            Ok(format!("{warn}Secret key written to {out} (keep it private).\nPublic key (give this to users):\n{public}"))
        }
        ["pubkey", key] => unpack_lib::catalog::public_of(&read_secret(key)?),
        ["sign", key, cat, rest @ ..] if rest.len() <= 1 => {
            let bytes = std::fs::read(cat).map_err(|e| format!("{cat}: {e}"))?;
            let secret = read_secret(key)?;
            let sig = unpack_lib::catalog::sign(&bytes, &secret)?;
            let public = unpack_lib::catalog::public_of(&secret)?;
            // Sanity: the signed file must load in Unpack with this key.
            let l = unpack_lib::catalog::load_bytes(&bytes, sig.as_bytes(), &public, "check")?;
            let out = rest.first().map(|s| s.to_string()).unwrap_or(format!("{cat}.sig"));
            std::fs::write(&out, format!("{sig}\n")).map_err(|e| format!("{out}: {e}"))?;
            let near = unpack_lib::catalog::MAX_CATALOG_VERSION - 1_000_000;
            let mut msg = String::new();
            if l.catalog_version > near { msg.push_str("WARNING: catalog_version is close to the maximum; later catalogs can't go higher.\n"); }
            msg.push_str(&format!("Signed {cat} (catalog_version {}, {} entries) -> {out}\nPublic key: {public}", l.catalog_version, l.entries.len()));
            for p in &l.problems { msg.push_str(&format!("\nWARNING skipped {p}")); }
            Ok(msg)
        }
        ["verify", key, cat, rest @ ..] if rest.len() <= 1 => {
            let bytes = std::fs::read(cat).map_err(|e| format!("{cat}: {e}"))?;
            let sp = rest.first().map(|s| s.to_string()).unwrap_or(format!("{cat}.sig"));
            let sig = std::fs::read(&sp).map_err(|e| format!("{sp}: {e}"))?;
            let l = unpack_lib::catalog::load_bytes(&bytes, &sig, &read_key(key)?, "check")?;
            let mut msg = format!("OK: signature valid. catalog_version {}, {} valid entries.", l.catalog_version, l.entries.len());
            for p in &l.problems { msg.push_str(&format!("\nWARNING skipped {p}")); }
            Ok(msg)
        }
        ["check", cat] => {
            let bytes = std::fs::read(cat).map_err(|e| format!("{cat}: {e}"))?;
            // Sign with a throwaway key just to run the same loader.
            let (s, p) = unpack_lib::catalog::keygen()?;
            let sig = unpack_lib::catalog::sign(&bytes, &s)?;
            let l = unpack_lib::catalog::load_bytes(&bytes, sig.as_bytes(), &p, "check")?;
            let mut msg = format!("Schema OK: {} valid entries, {} skipped.", l.entries.len(), l.problems.len());
            for p in &l.problems { msg.push_str(&format!("\nskipped {p}")); }
            Ok(msg)
        }
        _ => Err(usage.into()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(m) => { println!("{m}"); ExitCode::SUCCESS }
        Err(e) => { eprintln!("{e}"); ExitCode::from(1) }
    }
}
