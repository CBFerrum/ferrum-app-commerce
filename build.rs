//! Embeds this app's metadata packages (`packages/<name>/...`) into the crate.

use std::fmt::Write;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = read.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().to_string();
        let rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        if e.path().is_dir() {
            walk(&e.path(), &rel, out);
        } else {
            out.push((rel, e.path()));
        }
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut code = String::from("// (package, path inside the package, content)\npub static EMBEDDED: &[(&str, &str, &str)] = &[\n");
    let mut packages: Vec<_> =
        std::fs::read_dir(&root).expect("packages/").filter_map(|e| e.ok()).filter(|e| e.path().join("package.yaml").is_file()).collect();
    packages.sort_by_key(|e| e.file_name());
    for pkg in packages {
        let name = pkg.file_name().to_string_lossy().to_string();
        let mut files = Vec::new();
        walk(&pkg.path(), "", &mut files);
        for (rel, path) in files {
            let abs = path.canonicalize().expect("package file");
            writeln!(code, "    ({name:?}, {rel:?}, include_str!({abs:?})),").expect("write");
        }
    }
    code.push_str("];\n");
    let out = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).to_path_buf();
    std::fs::write(out.join("packages.rs"), code).expect("write packages.rs");

    // The interface's built files (ui/dist, built from ui/src and committed; Ferrum
    // docs/plan/apps-ui.md), without the build's own notes (dotfiles such as .source).
    let ui = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui").join("dist");
    println!("cargo:rerun-if-changed={}", ui.display());
    let mut code = String::from("/// This app's own interface: its built files (path under ui/dist, content).\npub static UI: &[(&str, &[u8])] = &[\n");
    let mut files = Vec::new();
    walk(&ui, "", &mut files);
    for (rel, path) in files.into_iter().filter(|(rel, _)| !rel.split('/').any(|s| s.starts_with('.'))) {
        let abs = path.canonicalize().expect("ui file");
        writeln!(code, "    ({rel:?}, include_bytes!({abs:?})),").expect("write");
    }
    code.push_str("];\n");
    std::fs::write(out.join("ui.rs"), code).expect("write ui.rs");
}
