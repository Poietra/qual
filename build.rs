use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=Cargo.lock");

    let mut files = Vec::new();
    collect_files(Path::new("src"), &mut files);
    for path in [
        PathBuf::from("build.rs"),
        PathBuf::from("Cargo.toml"),
        PathBuf::from("Cargo.lock"),
    ] {
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();

    let mut digest = Sha256::new();
    for path in files {
        let shown = path.to_string_lossy();
        let bytes = std::fs::read(&path).unwrap_or_else(|error| {
            panic!("cannot hash analysis source {}: {error}", path.display())
        });
        update_field(&mut digest, shown.as_bytes());
        update_field(&mut digest, &bytes);
    }
    println!(
        "cargo:rustc-env=MANIM_LINT_BUILD_ID={:x}",
        digest.finalize()
    );
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()))
        .map(|entry| entry.expect("read source entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_files(&path, files);
        } else if path.is_file() {
            files.push(path);
        }
    }
}

fn update_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(
        u64::try_from(bytes.len())
            .expect("field length fits u64")
            .to_le_bytes(),
    );
    digest.update(bytes);
}
