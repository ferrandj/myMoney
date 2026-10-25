//! Build-time facts: the fingerprint of the reading code (every adapter, the PDF reader, and the rules that name the
//! people and the files), which a document remembers so that a reading made by older code is redone.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            files(&p, out);
        } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn main() {
    let src = Path::new("src");
    let mut all = vec![];
    for dir in ["adapters", "pdf"] {
        files(&src.join(dir), &mut all);
        println!("cargo:rerun-if-changed=src/{dir}");
    }
    for file in ["services/naming.rs", "services/holders.rs"] {
        all.push(src.join(file));
        println!("cargo:rerun-if-changed=src/{file}");
    }
    let mut rel: Vec<(String, PathBuf)> = all.into_iter().map(|p| (p.strip_prefix(src).unwrap().to_string_lossy().replace('\\', "/"), p)).collect();
    rel.sort();
    let mut h = Sha256::new();
    for (name, path) in rel {
        h.update(name.as_bytes());
        h.update(std::fs::read(path).unwrap());
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    println!("cargo:rustc-env=MYMONEY_READER_VERSION={}", &hex[..8]);
}
