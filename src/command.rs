use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedCommand {
    Search(SearchCommand),
    FindMap { query: FindCommand },
    LsRecursive { path: PathBuf },
    TreeMap { path: PathBuf },
    Cat { path: PathBuf },
    FileSlice(FileSliceCommand),
    WcLines { paths: Vec<PathBuf> },
    Git(GitCommand),
    Test(TestCommand),
    Deps { path: PathBuf },
    Unsupported { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchKind {
    Rg,
    Grep,
    GitGrep,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCommand {
    pub kind: SearchKind,
    pub pattern: String,
    pub paths: Vec<PathBuf>,
    pub prefer_raw_matches: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindCommand {
    pub path: PathBuf,
    pub name_patterns: Vec<FindNamePattern>,
    pub min_depth: Option<usize>,
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindNamePattern {
    pub pattern: String,
    pub case_insensitive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSliceKind {
    Head,
    Tail,
    Sed,
    NumberedSed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSliceCommand {
    pub kind: FileSliceKind,
    pub path: PathBuf,
    pub range: FileSliceRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSliceRange {
    FirstLines(usize),
    LastLines(usize),
    Explicit { start: usize, end: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitCommand {
    ReadOnly {
        subcommand: GitReadOnly,
        args: Vec<String>,
    },
    Mutating {
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitReadOnly {
    Status,
    Diff,
    Log,
    Show,
    Branch,
    LsFiles,
    LsTree,
    RevParse,
    Remote,
    Config,
    MergeBase,
    Describe,
    Blame,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestCommand {
    CargoTest,
    CargoCheck,
    CargoClippy,
    Pytest,
    GoTest,
    Npm,
    Pnpm,
    Yarn,
    Vitest,
    Jest,
    Playwright,
    Ruff,
    Mypy,
}

impl GitReadOnly {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Diff => "diff",
            Self::Log => "log",
            Self::Show => "show",
            Self::Branch => "branch",
            Self::LsFiles => "ls-files",
            Self::LsTree => "ls-tree",
            Self::RevParse => "rev-parse",
            Self::Remote => "remote",
            Self::Config => "config",
            Self::MergeBase => "merge-base",
            Self::Describe => "describe",
            Self::Blame => "blame",
        }
    }
}

#[derive(Debug, Error)]
pub enum ParseCommandError {
    #[error("could not parse shell command: {0}")]
    Shell(String),
}

pub fn parse_command(command: &str) -> Result<ParsedCommand, ParseCommandError> {
    let words =
        shell_words::split(command).map_err(|error| ParseCommandError::Shell(error.to_string()))?;
    if words.is_empty() {
        return Ok(ParsedCommand::Unsupported {
            reason: "empty command".to_string(),
        });
    }

    let executable = executable_name(&words[0]);
    match executable.as_str() {
        "rg" => parse_rg(&words),
        "grep" => parse_grep(&words),
        "find" => parse_find(&words),
        "ls" => parse_ls(&words),
        "tree" => parse_tree(&words),
        "cat" => parse_cat(&words),
        "head" => parse_head_tail(&words, FileSliceKind::Head),
        "tail" => parse_head_tail(&words, FileSliceKind::Tail),
        "sed" => parse_sed(&words),
        "nl" => parse_numbered_sed(&words),
        "wc" => parse_wc(&words),
        "git" => Ok(parse_git(&words)),
        "cargo" => parse_cargo(&words),
        "pytest" | "py.test" => parse_pytest(&words),
        "python" | "python3" => parse_python(&words),
        "go" => parse_go(&words),
        "npm" => parse_node_package_manager(&words, TestCommand::Npm),
        "pnpm" => parse_node_package_manager(&words, TestCommand::Pnpm),
        "yarn" => parse_node_package_manager(&words, TestCommand::Yarn),
        "npx" => parse_npx(&words),
        "vitest" => Ok(ParsedCommand::Test(TestCommand::Vitest)),
        "jest" => Ok(ParsedCommand::Test(TestCommand::Jest)),
        "playwright" => Ok(ParsedCommand::Test(TestCommand::Playwright)),
        "ruff" => Ok(ParsedCommand::Test(TestCommand::Ruff)),
        "mypy" => Ok(ParsedCommand::Test(TestCommand::Mypy)),
        "deps" => parse_deps(&words),
        _ => Ok(ParsedCommand::Unsupported {
            reason: format!("unsupported command: {}", words[0]),
        }),
    }
}

fn parse_cargo(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    match words.get(1).map(String::as_str) {
        Some("test") => Ok(ParsedCommand::Test(TestCommand::CargoTest)),
        Some("check") => Ok(ParsedCommand::Test(TestCommand::CargoCheck)),
        Some("clippy") => Ok(ParsedCommand::Test(TestCommand::CargoClippy)),
        _ => Ok(ParsedCommand::Unsupported {
            reason: "cargo command is not cargo test/check/clippy".to_string(),
        }),
    }
}

fn parse_node_package_manager(
    words: &[String],
    command: TestCommand,
) -> Result<ParsedCommand, ParseCommandError> {
    let Some(script) = node_script_name(words) else {
        return Ok(ParsedCommand::Unsupported {
            reason: "package-manager command is not a test/build/lint script".to_string(),
        });
    };
    if matches!(
        script,
        "test" | "build" | "lint" | "typecheck" | "check" | "vitest" | "jest" | "playwright"
    ) {
        Ok(ParsedCommand::Test(command))
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "package-manager script is passed through".to_string(),
        })
    }
}

fn node_script_name(words: &[String]) -> Option<&str> {
    let first = words.get(1)?.as_str();
    if first == "run" {
        return words.get(2).map(String::as_str);
    }
    if first == "exec" || first == "dlx" {
        return words.get(2).map(String::as_str);
    }
    Some(first)
}

fn parse_npx(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let runner = words
        .iter()
        .skip(1)
        .find(|word| !word.starts_with('-'))
        .map(String::as_str);
    match runner {
        Some("vitest") => Ok(ParsedCommand::Test(TestCommand::Vitest)),
        Some("jest") => Ok(ParsedCommand::Test(TestCommand::Jest)),
        Some("playwright") => Ok(ParsedCommand::Test(TestCommand::Playwright)),
        _ => Ok(ParsedCommand::Unsupported {
            reason: "npx command is not a known test runner".to_string(),
        }),
    }
}

fn parse_pytest(_words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    Ok(ParsedCommand::Test(TestCommand::Pytest))
}

fn parse_python(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    if words.get(1).map(String::as_str) == Some("-m")
        && words.get(2).map(String::as_str) == Some("pytest")
    {
        Ok(ParsedCommand::Test(TestCommand::Pytest))
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "python command is not python -m pytest".to_string(),
        })
    }
}

fn parse_go(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    if words.get(1).map(String::as_str) == Some("test") {
        Ok(ParsedCommand::Test(TestCommand::GoTest))
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "go command is not go test".to_string(),
        })
    }
}

fn parse_deps(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    if words.len() > 2 {
        return Ok(ParsedCommand::Unsupported {
            reason: "deps accepts at most one path".to_string(),
        });
    }
    Ok(ParsedCommand::Deps {
        path: words
            .get(1)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".")),
    })
}

fn parse_tree(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let mut path = None;
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        if word == "-L" || word == "--filelimit" || word == "-I" || word == "-P" {
            i += 2;
            continue;
        }
        if word.starts_with('-') {
            i += 1;
            continue;
        }
        path = Some(PathBuf::from(word));
        i += 1;
    }
    Ok(ParsedCommand::TreeMap {
        path: path.unwrap_or_else(|| PathBuf::from(".")),
    })
}

fn executable_name(word: &str) -> String {
    Path::new(word)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(word)
        .to_string()
}

fn parse_rg(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let mut pattern = None;
    let mut paths = Vec::new();
    let mut prefer_raw_matches = false;
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            continue;
        }
        if let Some(value) = word.strip_prefix("--regexp=") {
            pattern = Some(value.to_string());
            prefer_raw_matches = true;
            i += 1;
            break;
        }
        if let Some(value) = word.strip_prefix("-e").filter(|value| !value.is_empty()) {
            pattern = Some(value.to_string());
            prefer_raw_matches = true;
            i += 1;
            break;
        }
        if matches!(word.as_str(), "-e" | "--regexp") && words.get(i + 1).is_some() {
            pattern = words.get(i + 1).cloned();
            prefer_raw_matches = true;
            i += 2;
            break;
        }
        if rg_option_takes_value(word) {
            prefer_raw_matches = true;
            i += 2;
            continue;
        }
        if rg_inline_option_value(word) || word.starts_with('-') {
            prefer_raw_matches = true;
            i += 1;
            continue;
        }
        pattern = Some(word.clone());
        i += 1;
        break;
    }

    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            continue;
        }
        if rg_option_takes_value(word) {
            prefer_raw_matches = true;
            i += 2;
            continue;
        }
        if rg_inline_option_value(word) || word.starts_with('-') {
            prefer_raw_matches = true;
            i += 1;
            continue;
        }
        paths.push(PathBuf::from(word));
        i += 1;
    }

    match pattern {
        Some(pattern) => Ok(ParsedCommand::Search(SearchCommand {
            kind: SearchKind::Rg,
            pattern,
            paths: default_paths(paths),
            prefer_raw_matches,
        })),
        None => Ok(ParsedCommand::Unsupported {
            reason: "rg command has no pattern".to_string(),
        }),
    }
}

fn parse_grep(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let mut recursive = false;
    let mut pattern = None;
    let mut paths = Vec::new();
    let mut prefer_raw_matches = false;
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            continue;
        }
        if let Some(value) = word.strip_prefix("--regexp=") {
            pattern = Some(value.to_string());
            prefer_raw_matches = true;
            i += 1;
            break;
        }
        if let Some(value) = word.strip_prefix("-e").filter(|value| !value.is_empty()) {
            pattern = Some(value.to_string());
            prefer_raw_matches = true;
            i += 1;
            break;
        }
        if matches!(word.as_str(), "-e" | "--regexp") && words.get(i + 1).is_some() {
            pattern = words.get(i + 1).cloned();
            prefer_raw_matches = true;
            i += 2;
            break;
        }
        if matches!(word.as_str(), "-f" | "--file") || word.starts_with("--file=") {
            return Ok(ParsedCommand::Unsupported {
                reason: "grep pattern files are passed through".to_string(),
            });
        }
        if word.starts_with('-') {
            if word.contains('R') || word.contains('r') || word == "--recursive" {
                recursive = true;
            }
            if matches!(word.as_str(), "-R" | "-r" | "--recursive") {
                i += 1;
                continue;
            }
            if grep_option_takes_value(word) {
                prefer_raw_matches = true;
                i += 2;
                continue;
            }
            if grep_inline_option_value(word) {
                prefer_raw_matches = true;
                i += 1;
                continue;
            }
            prefer_raw_matches = true;
            i += 1;
            continue;
        }
        pattern = Some(word.clone());
        i += 1;
        break;
    }
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            continue;
        }
        if grep_option_takes_value(word) {
            prefer_raw_matches = true;
            i += 2;
            continue;
        }
        if grep_inline_option_value(word) {
            prefer_raw_matches = true;
            i += 1;
            continue;
        }
        if word.starts_with('-') {
            prefer_raw_matches = true;
        } else {
            paths.push(PathBuf::from(word));
        }
        i += 1;
    }

    if !recursive {
        return Ok(ParsedCommand::Unsupported {
            reason: "grep command is not recursive".to_string(),
        });
    }

    match pattern {
        Some(pattern) => Ok(ParsedCommand::Search(SearchCommand {
            kind: SearchKind::Grep,
            pattern,
            paths: default_paths(paths),
            prefer_raw_matches,
        })),
        None => Ok(ParsedCommand::Unsupported {
            reason: "grep -R command has no pattern".to_string(),
        }),
    }
}

fn rg_option_takes_value(word: &str) -> bool {
    matches!(
        word,
        "-g" | "--glob"
            | "-t"
            | "--type"
            | "-T"
            | "--type-not"
            | "-A"
            | "--after-context"
            | "-B"
            | "--before-context"
            | "-C"
            | "--context"
            | "-m"
            | "--max-count"
            | "-j"
            | "--threads"
            | "--sort"
            | "--sortr"
            | "--ignore-file"
            | "--path-separator"
            | "--engine"
            | "--encoding"
            | "--colors"
            | "--max-filesize"
    )
}

fn rg_inline_option_value(word: &str) -> bool {
    matches!(
        word.split_once('=').map(|(flag, _)| flag),
        Some(
            "--glob"
                | "--type"
                | "--type-not"
                | "--after-context"
                | "--before-context"
                | "--context"
                | "--max-count"
                | "--threads"
                | "--sort"
                | "--sortr"
                | "--ignore-file"
                | "--path-separator"
                | "--engine"
                | "--encoding"
                | "--colors"
                | "--max-filesize"
        )
    ) || matches!(
        word.get(..2),
        Some("-g" | "-t" | "-T" | "-A" | "-B" | "-C" | "-m" | "-j")
    ) && word.len() > 2
}

fn grep_option_takes_value(word: &str) -> bool {
    matches!(
        word,
        "-A" | "--after-context"
            | "-B"
            | "--before-context"
            | "-C"
            | "--context"
            | "-m"
            | "--max-count"
            | "--include"
            | "--exclude"
            | "--exclude-dir"
            | "--binary-files"
            | "--label"
    )
}

fn grep_inline_option_value(word: &str) -> bool {
    matches!(
        word.split_once('=').map(|(flag, _)| flag),
        Some(
            "--after-context"
                | "--before-context"
                | "--context"
                | "--max-count"
                | "--include"
                | "--exclude"
                | "--exclude-dir"
                | "--binary-files"
                | "--label"
        )
    ) || matches!(word.get(..2), Some("-A" | "-B" | "-C" | "-m")) && word.len() > 2
}

fn parse_find(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    if words.len() < 2 {
        return Ok(ParsedCommand::Unsupported {
            reason: "find command is not find <path> -type f".to_string(),
        });
    }
    let mut path = None;
    let mut type_f = false;
    let mut name_patterns = Vec::new();
    let mut min_depth = None;
    let mut max_depth = None;
    let mut i = 1;

    while i < words.len() {
        let word = &words[i];
        match word.as_str() {
            "--" => {
                i += 1;
            }
            "-type" => {
                let Some(value) = words.get(i + 1) else {
                    return Ok(ParsedCommand::Unsupported {
                        reason: "find -type has no value".to_string(),
                    });
                };
                if value == "f" {
                    type_f = true;
                    i += 2;
                } else {
                    return Ok(ParsedCommand::Unsupported {
                        reason: "find command does not request files with -type f".to_string(),
                    });
                }
            }
            "-name" | "-iname" => {
                let Some(value) = words.get(i + 1) else {
                    return Ok(ParsedCommand::Unsupported {
                        reason: format!("{word} has no pattern"),
                    });
                };
                if !is_safe_find_name_pattern(value) {
                    return Ok(ParsedCommand::Unsupported {
                        reason: format!("{word} pattern is not supported by the compact map"),
                    });
                }
                name_patterns.push(FindNamePattern {
                    pattern: value.clone(),
                    case_insensitive: word == "-iname",
                });
                i += 2;
            }
            "-maxdepth" => {
                let Some(depth) = words
                    .get(i + 1)
                    .and_then(|value| parse_zero_based_count(value))
                else {
                    return Ok(ParsedCommand::Unsupported {
                        reason: "find -maxdepth has no numeric value".to_string(),
                    });
                };
                max_depth = Some(depth);
                i += 2;
            }
            "-mindepth" => {
                let Some(depth) = words
                    .get(i + 1)
                    .and_then(|value| parse_zero_based_count(value))
                else {
                    return Ok(ParsedCommand::Unsupported {
                        reason: "find -mindepth has no numeric value".to_string(),
                    });
                };
                min_depth = Some(depth);
                i += 2;
            }
            "-print" => {
                i += 1;
            }
            "-path" | "-ipath" | "-regex" | "-iregex" | "-prune" | "-exec" | "-execdir"
            | "-delete" | "-o" | "-or" | "-a" | "-and" | "!" | "-not" | "(" | ")" => {
                return Ok(ParsedCommand::Unsupported {
                    reason: format!("find predicate {word} is passed through for exact semantics"),
                });
            }
            _ if word.starts_with('-') => {
                return Ok(ParsedCommand::Unsupported {
                    reason: format!("find predicate {word} is passed through for exact semantics"),
                });
            }
            _ => {
                if path.is_some() {
                    return Ok(ParsedCommand::Unsupported {
                        reason: "find command has multiple search roots".to_string(),
                    });
                }
                path = Some(PathBuf::from(word));
                i += 1;
            }
        }
    }

    if type_f {
        Ok(ParsedCommand::FindMap {
            query: FindCommand {
                path: path.unwrap_or_else(|| PathBuf::from(".")),
                name_patterns,
                min_depth,
                max_depth,
            },
        })
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "find command does not request files with -type f".to_string(),
        })
    }
}

fn is_safe_find_name_pattern(pattern: &str) -> bool {
    !pattern.is_empty()
        && !pattern.contains('/')
        && !pattern.contains('\\')
        && !pattern.contains('[')
        && !pattern.contains(']')
}

fn parse_ls(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let mut recursive = false;
    let mut path = None;
    for word in &words[1..] {
        if word.starts_with('-') {
            if word.contains('R') {
                recursive = true;
            }
        } else {
            path = Some(PathBuf::from(word));
        }
    }

    if recursive {
        Ok(ParsedCommand::LsRecursive {
            path: path.unwrap_or_else(|| PathBuf::from(".")),
        })
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "ls command is not recursive".to_string(),
        })
    }
}

fn parse_cat(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    if words.len() == 2 && !words[1].starts_with('-') {
        Ok(ParsedCommand::Cat {
            path: PathBuf::from(&words[1]),
        })
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "cat command is not a single file read".to_string(),
        })
    }
}

fn parse_head_tail(
    words: &[String],
    kind: FileSliceKind,
) -> Result<ParsedCommand, ParseCommandError> {
    let mut lines = 10;
    let mut files = Vec::new();
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        if word == "-n" || word == "--lines" {
            if let Some(value) = words.get(i + 1).and_then(|value| parse_count(value)) {
                lines = value;
                i += 2;
                continue;
            }
            return Ok(ParsedCommand::Unsupported {
                reason: "head/tail -n has no numeric value".to_string(),
            });
        }
        if let Some(stripped) = word.strip_prefix("-n")
            && let Some(value) = parse_count(stripped)
        {
            lines = value;
            i += 1;
            continue;
        }
        if word.starts_with('-') {
            if let Some(value) = parse_count(word.trim_start_matches('-')) {
                lines = value;
                i += 1;
                continue;
            }
            return Ok(ParsedCommand::Unsupported {
                reason: "unsupported head/tail flag".to_string(),
            });
        }
        files.push(PathBuf::from(word));
        i += 1;
    }

    if files.len() == 1 {
        let range = match kind {
            FileSliceKind::Head => FileSliceRange::FirstLines(lines),
            FileSliceKind::Tail => FileSliceRange::LastLines(lines),
            FileSliceKind::Sed | FileSliceKind::NumberedSed => unreachable!(),
        };
        Ok(ParsedCommand::FileSlice(FileSliceCommand {
            kind,
            path: files.remove(0),
            range,
        }))
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "head/tail command is not a single file read".to_string(),
        })
    }
}

fn parse_sed(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let mut script = None;
    let mut files = Vec::new();
    let mut i = 1;
    while i < words.len() {
        let word = &words[i];
        if word == "-n" {
            i += 1;
            continue;
        }
        if word.starts_with('-') {
            return Ok(ParsedCommand::Unsupported {
                reason: "unsupported sed flag".to_string(),
            });
        }
        if script.is_none() {
            script = Some(word.clone());
        } else {
            files.push(PathBuf::from(word));
        }
        i += 1;
    }

    let Some(range) = script.as_deref().and_then(parse_sed_range) else {
        return Ok(ParsedCommand::Unsupported {
            reason: "sed command is not a numeric -n range".to_string(),
        });
    };

    if files.len() == 1 {
        Ok(ParsedCommand::FileSlice(FileSliceCommand {
            kind: FileSliceKind::Sed,
            path: files.remove(0),
            range,
        }))
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "sed command is not a single file read".to_string(),
        })
    }
}

fn parse_numbered_sed(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let Some(pipe_index) = words.iter().position(|word| word == "|") else {
        return Ok(ParsedCommand::Unsupported {
            reason: "nl command has no sed pipe".to_string(),
        });
    };
    if words.get(pipe_index + 1).map(String::as_str) != Some("sed") {
        return Ok(ParsedCommand::Unsupported {
            reason: "nl pipe is not sed".to_string(),
        });
    }

    let file = words[1..pipe_index]
        .iter()
        .rfind(|word| !word.starts_with('-'))
        .map(PathBuf::from);
    let script = words[pipe_index + 2..]
        .iter()
        .find(|word| word.as_str() != "-n" && !word.starts_with('-'));

    match (file, script.and_then(|script| parse_sed_range(script))) {
        (Some(path), Some(range)) => Ok(ParsedCommand::FileSlice(FileSliceCommand {
            kind: FileSliceKind::NumberedSed,
            path,
            range,
        })),
        _ => Ok(ParsedCommand::Unsupported {
            reason: "nl | sed command is not a numeric file range".to_string(),
        }),
    }
}

fn parse_wc(words: &[String]) -> Result<ParsedCommand, ParseCommandError> {
    let mut line_mode = false;
    let mut paths = Vec::new();
    for word in &words[1..] {
        if word == "-l" || word == "--lines" {
            line_mode = true;
        } else if word.starts_with('-') {
            return Ok(ParsedCommand::Unsupported {
                reason: "wc command is not line-only".to_string(),
            });
        } else {
            paths.push(PathBuf::from(word));
        }
    }

    if line_mode && !paths.is_empty() {
        Ok(ParsedCommand::WcLines { paths })
    } else {
        Ok(ParsedCommand::Unsupported {
            reason: "wc command is not wc -l <path>".to_string(),
        })
    }
}

fn parse_sed_range(script: &str) -> Option<FileSliceRange> {
    let script = script.trim();
    let script = script.strip_suffix('p')?;
    if let Some((start, end)) = script.split_once(',') {
        let start = parse_count(start)?;
        if let Some(count) = end.strip_prefix('+').and_then(parse_count) {
            return Some(FileSliceRange::Explicit {
                start,
                end: start.saturating_add(count),
            });
        }
        let end = parse_count(end)?;
        return Some(FileSliceRange::Explicit { start, end });
    }
    let line = parse_count(script)?;
    Some(FileSliceRange::Explicit {
        start: line,
        end: line,
    })
}

fn parse_count(value: &str) -> Option<usize> {
    value
        .trim_start_matches('+')
        .parse::<usize>()
        .ok()
        .filter(|value| *value > 0)
}

fn parse_zero_based_count(value: &str) -> Option<usize> {
    value.parse::<usize>().ok()
}

fn parse_git(words: &[String]) -> ParsedCommand {
    let mut i = 1;
    while i < words.len() && words[i].starts_with('-') {
        if matches!(words[i].as_str(), "-C" | "--git-dir" | "--work-tree") {
            i += 2;
        } else {
            i += 1;
        }
    }
    let Some(subcommand) = words.get(i) else {
        return ParsedCommand::Git(GitCommand::Mutating {
            args: words[1..].to_vec(),
        });
    };

    if subcommand == "grep" {
        return parse_git_grep(words, i).unwrap_or_else(|| ParsedCommand::Unsupported {
            reason: "git grep command has no pattern".to_string(),
        });
    }

    let read_only = match subcommand.as_str() {
        "status" => Some(GitReadOnly::Status),
        "diff" => Some(GitReadOnly::Diff),
        "log" => Some(GitReadOnly::Log),
        "show" => Some(GitReadOnly::Show),
        "branch" => Some(GitReadOnly::Branch),
        "ls-files" => Some(GitReadOnly::LsFiles),
        "ls-tree" => Some(GitReadOnly::LsTree),
        "rev-parse" => Some(GitReadOnly::RevParse),
        "merge-base" => Some(GitReadOnly::MergeBase),
        "describe" => Some(GitReadOnly::Describe),
        "blame" => Some(GitReadOnly::Blame),
        "remote" if is_read_only_remote(words, i) => Some(GitReadOnly::Remote),
        "config" if is_read_only_config(words, i) => Some(GitReadOnly::Config),
        _ => None,
    };

    match read_only {
        Some(subcommand) => ParsedCommand::Git(GitCommand::ReadOnly {
            subcommand,
            args: words[1..].to_vec(),
        }),
        None => ParsedCommand::Git(GitCommand::Mutating {
            args: words[1..].to_vec(),
        }),
    }
}

fn parse_git_grep(words: &[String], grep_index: usize) -> Option<ParsedCommand> {
    let mut pattern = None;
    let mut paths = Vec::new();
    let mut i = grep_index + 1;
    while i < words.len() {
        let word = &words[i];
        if word == "--" {
            i += 1;
            break;
        }
        if word == "-e" || word == "--regexp" {
            pattern = words.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if word.starts_with('-') {
            i += 1;
            continue;
        }
        pattern = Some(word.clone());
        i += 1;
        break;
    }
    while i < words.len() {
        if words[i] != "--" {
            paths.push(PathBuf::from(&words[i]));
        }
        i += 1;
    }

    pattern.map(|pattern| {
        ParsedCommand::Search(SearchCommand {
            kind: SearchKind::GitGrep,
            pattern,
            paths: default_paths(paths),
            prefer_raw_matches: false,
        })
    })
}

fn is_read_only_remote(words: &[String], subcommand_index: usize) -> bool {
    words[subcommand_index + 1..].is_empty()
        || matches!(
            words.get(subcommand_index + 1).map(String::as_str),
            Some("-v" | "--verbose" | "get-url" | "show")
        )
}

fn is_read_only_config(words: &[String], subcommand_index: usize) -> bool {
    words[subcommand_index + 1..].iter().any(|word| {
        matches!(
            word.as_str(),
            "--get" | "--get-regexp" | "--list" | "--name-only" | "-l"
        )
    })
}

fn default_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    if paths.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        paths
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rg_pattern() {
        assert_eq!(
            parse_command("rg stripe").unwrap(),
            ParsedCommand::Search(SearchCommand {
                kind: SearchKind::Rg,
                pattern: "stripe".to_string(),
                paths: vec![PathBuf::from(".")],
                prefer_raw_matches: false,
            })
        );
    }

    #[test]
    fn parses_grep_recursive() {
        assert_eq!(
            parse_command("grep -R stripe .").unwrap(),
            ParsedCommand::Search(SearchCommand {
                kind: SearchKind::Grep,
                pattern: "stripe".to_string(),
                paths: vec![PathBuf::from(".")],
                prefer_raw_matches: false,
            })
        );
    }

    #[test]
    fn parses_complex_search_flags_in_raw_compatibility_mode() {
        assert_eq!(
            parse_command("rg -g '*.rs' --sort path stripe src").unwrap(),
            ParsedCommand::Search(SearchCommand {
                kind: SearchKind::Rg,
                pattern: "stripe".to_string(),
                paths: vec![PathBuf::from("src")],
                prefer_raw_matches: true,
            })
        );
        assert_eq!(
            parse_command("grep -R --include '*.rs' stripe .").unwrap(),
            ParsedCommand::Search(SearchCommand {
                kind: SearchKind::Grep,
                pattern: "stripe".to_string(),
                paths: vec![PathBuf::from(".")],
                prefer_raw_matches: true,
            })
        );
    }

    #[test]
    fn parses_repo_map_commands() {
        assert_eq!(
            parse_command("find . -type f").unwrap(),
            ParsedCommand::FindMap {
                query: FindCommand {
                    path: PathBuf::from("."),
                    name_patterns: Vec::new(),
                    min_depth: None,
                    max_depth: None,
                }
            }
        );
        assert_eq!(
            parse_command("find src -maxdepth 2 -type f -name '*.rs'").unwrap(),
            ParsedCommand::FindMap {
                query: FindCommand {
                    path: PathBuf::from("src"),
                    name_patterns: vec![FindNamePattern {
                        pattern: "*.rs".to_string(),
                        case_insensitive: false,
                    }],
                    min_depth: None,
                    max_depth: Some(2),
                }
            }
        );
        assert!(matches!(
            parse_command("find . -path './target' -prune -o -type f -print").unwrap(),
            ParsedCommand::Unsupported { .. }
        ));
        assert_eq!(
            parse_command("ls -laR src").unwrap(),
            ParsedCommand::LsRecursive {
                path: PathBuf::from("src")
            }
        );
    }

    #[test]
    fn parses_cat_single_file() {
        assert_eq!(
            parse_command("cat src/main.rs").unwrap(),
            ParsedCommand::Cat {
                path: PathBuf::from("src/main.rs")
            }
        );
    }

    #[test]
    fn parses_file_slice_commands() {
        assert_eq!(
            parse_command("head -n 20 src/main.rs").unwrap(),
            ParsedCommand::FileSlice(FileSliceCommand {
                kind: FileSliceKind::Head,
                path: PathBuf::from("src/main.rs"),
                range: FileSliceRange::FirstLines(20),
            })
        );
        assert_eq!(
            parse_command("tail -40 src/main.rs").unwrap(),
            ParsedCommand::FileSlice(FileSliceCommand {
                kind: FileSliceKind::Tail,
                path: PathBuf::from("src/main.rs"),
                range: FileSliceRange::LastLines(40),
            })
        );
        assert_eq!(
            parse_command("sed -n '10,20p' src/main.rs").unwrap(),
            ParsedCommand::FileSlice(FileSliceCommand {
                kind: FileSliceKind::Sed,
                path: PathBuf::from("src/main.rs"),
                range: FileSliceRange::Explicit { start: 10, end: 20 },
            })
        );
        assert_eq!(
            parse_command("nl -ba src/main.rs | sed -n '10,20p'").unwrap(),
            ParsedCommand::FileSlice(FileSliceCommand {
                kind: FileSliceKind::NumberedSed,
                path: PathBuf::from("src/main.rs"),
                range: FileSliceRange::Explicit { start: 10, end: 20 },
            })
        );
    }

    #[test]
    fn parses_wc_and_tree_commands() {
        assert_eq!(
            parse_command("wc -l src/main.rs src/lib.rs").unwrap(),
            ParsedCommand::WcLines {
                paths: vec![PathBuf::from("src/main.rs"), PathBuf::from("src/lib.rs")],
            }
        );
        assert_eq!(
            parse_command("tree -L 2 src").unwrap(),
            ParsedCommand::TreeMap {
                path: PathBuf::from("src")
            }
        );
    }

    #[test]
    fn marks_mutating_git_passthrough() {
        assert_eq!(
            parse_command("git add src/main.rs").unwrap(),
            ParsedCommand::Git(GitCommand::Mutating {
                args: vec!["add".to_string(), "src/main.rs".to_string()],
            })
        );
    }

    #[test]
    fn parses_read_only_git() {
        assert_eq!(
            parse_command("git diff -- src/main.rs").unwrap(),
            ParsedCommand::Git(GitCommand::ReadOnly {
                subcommand: GitReadOnly::Diff,
                args: vec![
                    "diff".to_string(),
                    "--".to_string(),
                    "src/main.rs".to_string()
                ],
            })
        );
        assert_eq!(
            parse_command("git grep stripe -- src").unwrap(),
            ParsedCommand::Search(SearchCommand {
                kind: SearchKind::GitGrep,
                pattern: "stripe".to_string(),
                paths: vec![PathBuf::from("src")],
                prefer_raw_matches: false,
            })
        );
        assert_eq!(
            parse_command("git ls-tree -r --name-only HEAD").unwrap(),
            ParsedCommand::Git(GitCommand::ReadOnly {
                subcommand: GitReadOnly::LsTree,
                args: vec![
                    "ls-tree".to_string(),
                    "-r".to_string(),
                    "--name-only".to_string(),
                    "HEAD".to_string()
                ],
            })
        );
    }

    #[test]
    fn parses_test_runner_commands() {
        assert_eq!(
            parse_command("cargo test -- --nocapture").unwrap(),
            ParsedCommand::Test(TestCommand::CargoTest)
        );
        assert_eq!(
            parse_command("cargo check").unwrap(),
            ParsedCommand::Test(TestCommand::CargoCheck)
        );
        assert_eq!(
            parse_command("cargo clippy --all-targets").unwrap(),
            ParsedCommand::Test(TestCommand::CargoClippy)
        );
        assert_eq!(
            parse_command("pytest tests -q").unwrap(),
            ParsedCommand::Test(TestCommand::Pytest)
        );
        assert_eq!(
            parse_command("python -m pytest tests").unwrap(),
            ParsedCommand::Test(TestCommand::Pytest)
        );
        assert_eq!(
            parse_command("go test ./...").unwrap(),
            ParsedCommand::Test(TestCommand::GoTest)
        );
        assert_eq!(
            parse_command("npm run test -- --watch=false").unwrap(),
            ParsedCommand::Test(TestCommand::Npm)
        );
        assert_eq!(
            parse_command("pnpm vitest run").unwrap(),
            ParsedCommand::Test(TestCommand::Pnpm)
        );
        assert_eq!(
            parse_command("npx playwright test").unwrap(),
            ParsedCommand::Test(TestCommand::Playwright)
        );
        assert_eq!(
            parse_command("npx --no-install vitest run").unwrap(),
            ParsedCommand::Test(TestCommand::Vitest)
        );
        assert_eq!(
            parse_command("ruff check .").unwrap(),
            ParsedCommand::Test(TestCommand::Ruff)
        );
        assert_eq!(
            parse_command("mypy src").unwrap(),
            ParsedCommand::Test(TestCommand::Mypy)
        );
    }

    #[test]
    fn parses_deps_command() {
        assert_eq!(
            parse_command("deps .").unwrap(),
            ParsedCommand::Deps {
                path: PathBuf::from(".")
            }
        );
    }
}
