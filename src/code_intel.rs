use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::Serialize;

use crate::filters::{collect_source_files, is_text_file};

#[derive(Debug, Clone, Serialize)]
pub struct CodeIntelStatus {
    pub root: String,
    pub files: usize,
    pub bytes: u64,
    pub symbols: usize,
    pub imports: usize,
    pub sequence: u64,
    pub refreshes: u64,
    pub build_ms: f64,
    pub indexed_unix: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct OutlineSummary {
    pub path: String,
    pub bytes: u64,
    pub lines: usize,
    pub imports: Vec<ImportRef>,
    pub symbols: Vec<SymbolRef>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolRef {
    pub name: String,
    pub kind: String,
    pub path: String,
    pub line_number: usize,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportRef {
    pub path: String,
    pub line_number: usize,
    pub target: String,
    pub raw: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CallerRef {
    pub symbol: String,
    pub path: String,
    pub line_number: usize,
    pub line: String,
    pub enclosing_symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DependencyView {
    pub path: String,
    pub imports: Vec<ImportRef>,
    pub imported_by: Vec<ImportRef>,
    pub manifests: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct CodeIntelIndex {
    root: PathBuf,
    files: Vec<IndexedFile>,
    symbols: Vec<SymbolRef>,
    imports: Vec<ImportRef>,
    signature: IndexSignature,
    sequence: u64,
    refreshes: u64,
    build_ms: f64,
    indexed_unix: u64,
}

#[derive(Debug, Clone)]
struct IndexedFile {
    path: String,
    bytes: u64,
    lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IndexSignature {
    files: usize,
    bytes: u64,
    latest_modified_unix: u64,
}

impl CodeIntelIndex {
    pub fn build(root: &Path) -> Result<Self> {
        let root = root.canonicalize().with_context(|| {
            format!(
                "failed to resolve code intelligence root {}",
                root.display()
            )
        })?;
        let mut index = Self {
            root,
            files: Vec::new(),
            symbols: Vec::new(),
            imports: Vec::new(),
            signature: IndexSignature {
                files: 0,
                bytes: 0,
                latest_modified_unix: 0,
            },
            sequence: 0,
            refreshes: 0,
            build_ms: 0.0,
            indexed_unix: 0,
        };
        index.rebuild()?;
        Ok(index)
    }

    pub fn refresh_if_stale(&mut self) -> Result<bool> {
        let signature = scan_signature(&self.root)?;
        if signature == self.signature {
            return Ok(false);
        }
        self.rebuild()?;
        self.refreshes += 1;
        Ok(true)
    }

    pub fn status(&self) -> CodeIntelStatus {
        CodeIntelStatus {
            root: self.root.display().to_string(),
            files: self.files.len(),
            bytes: self.files.iter().map(|file| file.bytes).sum(),
            symbols: self.symbols.len(),
            imports: self.imports.len(),
            sequence: self.sequence,
            refreshes: self.refreshes,
            build_ms: self.build_ms,
            indexed_unix: self.indexed_unix,
        }
    }

    pub fn outline(&self, path: &Path, limit: usize) -> Result<OutlineSummary> {
        let relative = normalize_relative(path);
        let relative_text = relative.display().to_string();
        let file = self
            .files
            .iter()
            .find(|file| file.path == relative_text)
            .ok_or_else(|| anyhow::anyhow!("{} is not indexed", relative.display()))?;
        let (imports, imports_truncated) = capped_with_truncation(
            self.imports
                .iter()
                .filter(|import| import.path == relative_text)
                .cloned(),
            limit,
        );
        let (symbols, symbols_truncated) = capped_with_truncation(
            self.symbols
                .iter()
                .filter(|symbol| symbol.path == relative_text)
                .cloned(),
            limit,
        );
        Ok(OutlineSummary {
            path: relative_text,
            bytes: file.bytes,
            lines: file.lines,
            imports,
            symbols,
            truncated: imports_truncated || symbols_truncated,
        })
    }

    pub fn symbols(&self, query: &str, limit: usize) -> Vec<SymbolRef> {
        let query_lower = query.to_ascii_lowercase();
        let mut matches = self
            .symbols
            .iter()
            .filter_map(|symbol| {
                let name_lower = symbol.name.to_ascii_lowercase();
                let signature_lower = symbol.signature.to_ascii_lowercase();
                let score = if name_lower == query_lower {
                    0
                } else if name_lower.starts_with(&query_lower) {
                    1
                } else if name_lower.contains(&query_lower) {
                    2
                } else if signature_lower.contains(&query_lower) {
                    3
                } else {
                    return None;
                };
                Some((score, symbol.clone()))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(score, symbol)| {
            (
                *score,
                code_rank(Path::new(&symbol.path)),
                symbol.path.clone(),
                symbol.line_number,
            )
        });
        matches
            .into_iter()
            .take(limit)
            .map(|(_, symbol)| symbol)
            .collect()
    }

    pub fn callers(&self, symbol: &str, limit: usize) -> Result<Vec<CallerRef>> {
        let matcher = word_matcher(symbol)?;
        let mut callers = Vec::new();
        for file in &self.files {
            let path = self.root.join(&file.path);
            let content = match fs::read_to_string(&path) {
                Ok(content) => content,
                Err(_) => continue,
            };
            for (idx, line) in content.lines().enumerate() {
                if callers.len() >= limit {
                    return Ok(callers);
                }
                if !matcher.is_match(line) || self.is_symbol_definition(symbol, &file.path, idx + 1)
                {
                    continue;
                }
                callers.push(CallerRef {
                    symbol: symbol.to_string(),
                    path: file.path.clone(),
                    line_number: idx + 1,
                    line: line.to_string(),
                    enclosing_symbol: self.enclosing_symbol(&file.path, idx + 1),
                });
            }
        }
        Ok(callers)
    }

    pub fn deps(&self, path: &Path, limit: usize) -> Result<DependencyView> {
        let relative = normalize_relative(path);
        let relative_text = relative.display().to_string();
        if !self.files.iter().any(|file| file.path == relative_text) {
            bail!("{} is not indexed", relative.display());
        }

        let (imports, imports_truncated) = capped_with_truncation(
            self.imports
                .iter()
                .filter(|import| import.path == relative_text)
                .cloned(),
            limit,
        );
        let mut imported_by_all = Vec::new();
        for import in &self.imports {
            if import.path == relative_text {
                continue;
            }
            if self.import_targets_path(import, &relative_text) {
                imported_by_all.push(import.clone());
            }
        }
        imported_by_all.sort_by_key(|import| (import.path.clone(), import.line_number));
        let imported_by_truncated = imported_by_all.len() > limit;
        let imported_by = imported_by_all.iter().take(limit).cloned().collect();

        Ok(DependencyView {
            path: relative_text,
            imports,
            imported_by,
            manifests: dependency_manifests(&self.root),
            truncated: imports_truncated || imported_by_truncated,
        })
    }

    fn rebuild(&mut self) -> Result<()> {
        let start = Instant::now();
        let mut files = Vec::new();
        let mut symbols = Vec::new();
        let mut imports = Vec::new();
        let mut signature = IndexSignature {
            files: 0,
            bytes: 0,
            latest_modified_unix: 0,
        };

        for path in collect_source_files(std::slice::from_ref(&self.root)) {
            if !is_text_file(&path) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            };
            let relative = relative_path(&self.root, &path);
            let modified_unix = modified_unix(&metadata);
            signature.files += 1;
            signature.bytes += metadata.len();
            signature.latest_modified_unix = signature.latest_modified_unix.max(modified_unix);

            for (idx, line) in content.lines().enumerate() {
                if let Some(symbol) = extract_symbol(&relative, idx + 1, line) {
                    symbols.push(symbol);
                }
                if let Some(import) = extract_import(&relative, idx + 1, line) {
                    imports.push(import);
                }
            }

            files.push(IndexedFile {
                path: relative,
                bytes: metadata.len(),
                lines: content.lines().count(),
            });
        }

        files.sort_by_key(|file| (code_rank(Path::new(&file.path)), file.path.clone()));
        symbols.sort_by_key(|symbol| {
            (
                code_rank(Path::new(&symbol.path)),
                symbol.path.clone(),
                symbol.line_number,
            )
        });
        imports.sort_by_key(|import| {
            (
                code_rank(Path::new(&import.path)),
                import.path.clone(),
                import.line_number,
            )
        });

        self.files = files;
        self.symbols = symbols;
        self.imports = imports;
        self.signature = signature;
        self.sequence += 1;
        self.build_ms = start.elapsed().as_secs_f64() * 1000.0;
        self.indexed_unix = now_unix();
        Ok(())
    }

    fn is_symbol_definition(&self, name: &str, path: &str, line_number: usize) -> bool {
        self.symbols.iter().any(|symbol| {
            symbol.name == name && symbol.path == path && symbol.line_number == line_number
        })
    }

    fn enclosing_symbol(&self, path: &str, line_number: usize) -> Option<String> {
        self.symbols
            .iter()
            .filter(|symbol| symbol.path == path && symbol.line_number <= line_number)
            .max_by_key(|symbol| symbol.line_number)
            .map(|symbol| symbol.name.clone())
    }

    fn import_targets_path(&self, import: &ImportRef, target_path: &str) -> bool {
        let target = import.target.as_str();
        if target.starts_with('.') {
            let base = Path::new(&import.path)
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(target);
            return local_import_candidates(&base)
                .iter()
                .any(|candidate| candidate == Path::new(target_path));
        }

        let target_stem = Path::new(target_path)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default();
        target.ends_with(target_stem)
    }
}

fn scan_signature(root: &Path) -> Result<IndexSignature> {
    let mut signature = IndexSignature {
        files: 0,
        bytes: 0,
        latest_modified_unix: 0,
    };
    for path in collect_source_files(&[root.to_path_buf()]) {
        if !is_text_file(&path) {
            continue;
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        signature.files += 1;
        signature.bytes += metadata.len();
        signature.latest_modified_unix =
            signature.latest_modified_unix.max(modified_unix(&metadata));
    }
    Ok(signature)
}

fn extract_symbol(path: &str, line_number: usize, line: &str) -> Option<SymbolRef> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
        return None;
    }
    for (kind, regex) in symbol_patterns() {
        if let Some(captures) = regex.captures(line) {
            let name = captures
                .name("name")
                .or_else(|| captures.name("impl_name"))?
                .as_str()
                .trim_matches('{')
                .to_string();
            return Some(SymbolRef {
                name,
                kind: (*kind).to_string(),
                path: path.to_string(),
                line_number,
                signature: trimmed.to_string(),
            });
        }
    }
    None
}

fn extract_import(path: &str, line_number: usize, line: &str) -> Option<ImportRef> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with('#') {
        return None;
    }
    for regex in import_patterns() {
        if let Some(captures) = regex.captures(line) {
            let target = captures
                .name("target")
                .expect("import regexes use target capture")
                .as_str()
                .trim()
                .trim_end_matches(';')
                .to_string();
            return Some(ImportRef {
                path: path.to_string(),
                line_number,
                target,
                raw: trimmed.to_string(),
            });
        }
    }
    None
}

fn symbol_patterns() -> &'static [(&'static str, Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            vec![
                (
                    "function",
                    Regex::new(
                        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
                    )
                    .expect("rust fn regex compiles"),
                ),
                (
                    "type",
                    Regex::new(
                        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?P<kind>struct|enum|trait|type)\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)",
                    )
                    .expect("rust type regex compiles"),
                ),
                (
                    "impl",
                    Regex::new(r"^\s*impl(?:<[^>]+>)?\s+(?P<impl_name>[A-Za-z_][A-Za-z0-9_:<>]*)")
                        .expect("rust impl regex compiles"),
                ),
                (
                    "function",
                    Regex::new(
                        r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)",
                    )
                    .expect("js function regex compiles"),
                ),
                (
                    "function",
                    Regex::new(
                        r"^\s*(?:export\s+)?(?:const|let|var)\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)\s*=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][A-Za-z0-9_$]*)\s*=>",
                    )
                    .expect("js arrow regex compiles"),
                ),
                (
                    "type",
                    Regex::new(
                        r"^\s*(?:export\s+)?(?:default\s+)?(?:class|interface|type)\s+(?P<name>[A-Za-z_$][A-Za-z0-9_$]*)",
                    )
                    .expect("js type regex compiles"),
                ),
                (
                    "function",
                    Regex::new(r"^\s*(?:async\s+)?def\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
                        .expect("python def regex compiles"),
                ),
                (
                    "type",
                    Regex::new(r"^\s*class\s+(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
                        .expect("python class regex compiles"),
                ),
                (
                    "function",
                    Regex::new(r"^\s*func\s+(?:\([^)]*\)\s*)?(?P<name>[A-Za-z_][A-Za-z0-9_]*)")
                        .expect("go func regex compiles"),
                ),
            ]
        })
        .as_slice()
}

fn import_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            vec![
                Regex::new(
                    r#"^\s*import(?:\s+type)?(?:\s+.+?\s+from)?\s+["'](?P<target>[^"']+)["']"#,
                )
                .expect("js import regex compiles"),
                Regex::new(r#"require\(\s*["'](?P<target>[^"']+)["']\s*\)"#)
                    .expect("js require regex compiles"),
                Regex::new(r"^\s*(?:pub\s+)?use\s+(?P<target>[^;]+);")
                    .expect("rust use regex compiles"),
                Regex::new(r"^\s*(?:pub\s+)?mod\s+(?P<target>[A-Za-z_][A-Za-z0-9_]*);?")
                    .expect("rust mod regex compiles"),
                Regex::new(r"^\s*from\s+(?P<target>[A-Za-z_][A-Za-z0-9_.]*)\s+import\s+")
                    .expect("python from regex compiles"),
                Regex::new(r"^\s*import\s+(?P<target>[A-Za-z_][A-Za-z0-9_.,\s]*)$")
                    .expect("python import regex compiles"),
                Regex::new(
                    r#"^\s*import\s+(?:[A-Za-z_][A-Za-z0-9_]*\s+)?["`](?P<target>[^"`]+)["`]"#,
                )
                .expect("go import regex compiles"),
            ]
        })
        .as_slice()
}

fn word_matcher(symbol: &str) -> Result<Regex> {
    if symbol.is_empty() || symbol.chars().any(char::is_control) {
        bail!("symbol must be a non-empty string");
    }
    Regex::new(&format!(
        r"(^|[^A-Za-z0-9_$]){}($|[^A-Za-z0-9_$])",
        regex::escape(symbol)
    ))
    .with_context(|| format!("failed to compile symbol matcher for {symbol}"))
}

fn local_import_candidates(base: &Path) -> BTreeSet<PathBuf> {
    let normalized = normalize_relative(base);
    let mut candidates = BTreeSet::new();
    candidates.insert(normalized.clone());
    for extension in ["ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go"] {
        candidates.insert(with_extension(&normalized, extension));
    }
    for extension in ["ts", "tsx", "js", "jsx", "rs", "py", "go"] {
        candidates.insert(normalized.join(format!("index.{extension}")));
        candidates.insert(normalized.join(format!("mod.{extension}")));
    }
    candidates
}

fn with_extension(path: &Path, extension: &str) -> PathBuf {
    let mut value = path.to_path_buf();
    value.set_extension(extension);
    value
}

fn dependency_manifests(root: &Path) -> Vec<String> {
    [
        "Cargo.toml",
        "package.json",
        "requirements.txt",
        "pyproject.toml",
        "go.mod",
    ]
    .iter()
    .map(|path| root.join(path))
    .filter(|path| path.is_file())
    .map(|path| relative_path(root, &path))
    .collect()
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|relative| !relative.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn normalize_relative(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn modified_unix(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn capped_with_truncation<T>(iter: impl Iterator<Item = T>, limit: usize) -> (Vec<T>, bool) {
    let limit = limit.max(1);
    let mut items = iter.take(limit.saturating_add(1)).collect::<Vec<_>>();
    let truncated = items.len() > limit;
    if truncated {
        items.truncate(limit);
    }
    (items, truncated)
}

fn code_rank(path: &Path) -> u8 {
    let value = path.to_string_lossy();
    if value.starts_with("src/") || value.contains("/src/") {
        0
    } else if value.starts_with("tests/") || value.contains("/tests/") {
        1
    } else if value.ends_with(".md") || value.ends_with(".txt") {
        2
    } else {
        3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_ts_outline_and_callers() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src/billing");
        let tests = tmp.path().join("tests");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&tests).unwrap();
        fs::write(
            src.join("stripe.ts"),
            "export async function createStripeSubscription(customerId: string) {\n  return customerId;\n}\n",
        )
        .unwrap();
        fs::write(
            tests.join("billing.test.ts"),
            "import { createStripeSubscription } from \"../src/billing/stripe\";\ncreateStripeSubscription(\"cus_123\");\n",
        )
        .unwrap();

        let index = CodeIntelIndex::build(tmp.path()).unwrap();
        let symbols = index.symbols("createStripeSubscription", 10);
        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].path, "src/billing/stripe.ts");

        let callers = index.callers("createStripeSubscription", 10).unwrap();
        assert!(
            callers
                .iter()
                .any(|caller| caller.path == "tests/billing.test.ts")
        );

        let deps = index.deps(Path::new("src/billing/stripe.ts"), 10).unwrap();
        assert!(
            deps.imported_by
                .iter()
                .any(|import| import.path == "tests/billing.test.ts")
        );
    }

    #[test]
    fn refreshes_when_files_change() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("main.rs"), "fn one() {}\n").unwrap();
        let mut index = CodeIntelIndex::build(tmp.path()).unwrap();
        assert_eq!(index.symbols("two", 10).len(), 0);

        fs::write(tmp.path().join("main.rs"), "fn one() {}\nfn two() {}\n").unwrap();
        index.refresh_if_stale().unwrap();
        assert_eq!(index.symbols("two", 10).len(), 1);
        assert_eq!(index.status().refreshes, 1);
    }
}
