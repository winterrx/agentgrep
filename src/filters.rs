use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use ignore::{DirEntry, WalkBuilder};

pub fn collect_source_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        if root.is_file() {
            if should_include_file(root) {
                files.push(root.clone());
            }
            continue;
        }

        let mut builder = WalkBuilder::new(root);
        builder
            .standard_filters(true)
            .hidden(true)
            .parents(true)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .filter_entry(|entry| !should_skip_entry(entry));

        for result in builder.build() {
            let Ok(entry) = result else {
                continue;
            };
            let path = entry.path();
            if path.is_file() && should_include_file(path) {
                files.push(path.to_path_buf());
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

pub fn should_include_file(path: &Path) -> bool {
    !is_excluded_path(path) && !is_binary_or_media(path) && !is_lockfile(path)
}

pub fn is_excluded_path(path: &Path) -> bool {
    for component in path.components() {
        let Component::Normal(part) = component else {
            continue;
        };
        let Some(name) = part.to_str() else {
            continue;
        };
        if matches!(
            name,
            ".git"
                | ".hg"
                | ".svn"
                | ".agentgrep"
                | "node_modules"
                | "vendor"
                | "vendors"
                | "third_party"
                | "target"
                | "dist"
                | "build"
                | "coverage"
                | ".next"
                | ".nuxt"
                | ".turbo"
                | ".cache"
                | "out"
                | "tmp"
                | "temp"
        ) {
            return true;
        }
    }

    let Some(name) = path.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || name.contains(".generated.")
        || name.ends_with(".generated.ts")
        || name.ends_with(".generated.js")
        || name.ends_with(".pb.go")
}

pub fn is_lockfile(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(OsStr::to_str),
        Some(
            "Cargo.lock"
                | "package-lock.json"
                | "pnpm-lock.yaml"
                | "yarn.lock"
                | "bun.lockb"
                | "poetry.lock"
                | "Pipfile.lock"
                | "Gemfile.lock"
                | "go.sum"
        )
    )
}

fn is_binary_or_media(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "pdf"
                | "zip"
                | "gz"
                | "xz"
                | "zst"
                | "tar"
                | "mp4"
                | "mov"
                | "mp3"
                | "wav"
                | "wasm"
                | "so"
                | "dylib"
                | "dll"
                | "exe"
                | "class"
                | "jar"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
        )
    )
}

pub fn is_text_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buf = [0_u8; 8192];
    let Ok(read) = file.read(&mut buf) else {
        return false;
    };
    !buf[..read].contains(&0)
}

fn should_skip_entry(entry: &DirEntry) -> bool {
    is_excluded_path(entry.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excludes_generated_vendor_and_locks() {
        assert!(is_excluded_path(Path::new("node_modules/pkg/index.js")));
        assert!(is_excluded_path(Path::new("src/api.generated.ts")));
        assert!(is_lockfile(Path::new("Cargo.lock")));
        assert!(should_include_file(Path::new("src/main.rs")));
    }
}
