//! envguard: check .env files against .env.example and scan projects for leaked secrets.
//!
//!     envguard check [--env .env] [--example .env.example]
//!     envguard scan [PATH] [--exclude NAME]
//!
//! No dependencies. Exit codes: 0 clean, 1 problems found, 2 usage or I/O error.
//! Add `envguard:ignore` to a line to make `scan` skip it.

#![allow(dead_code)]

use std::io::{self, Write};
use std::process;

// ==========================================================================
// finding
// ==========================================================================

mod finding {
//! Common result type shared by `check` and `scan`.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub rule: &'static str,
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

impl Finding {
    pub fn new(
        severity: Severity,
        rule: &'static str,
        file: &str,
        line: Option<usize>,
        message: String,
    ) -> Finding {
        Finding {
            severity,
            rule,
            file: file.to_string(),
            line,
            message,
        }
    }
}

}

// ==========================================================================
// envfile
// ==========================================================================

mod envfile {
//! A small, forgiving parser for dotenv-style files.
//!
//! Supported: `KEY=value`, `export KEY=value`, single and double quotes,
//! inline `# comments` (only after whitespace), blank lines, CRLF, a UTF-8 BOM.
//! Multi-line values are not supported and are reported as parse issues.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub value: String,
    /// 1-based line number.
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseIssue {
    /// 1-based line number.
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct EnvFile {
    pub entries: Vec<Entry>,
    pub issues: Vec<ParseIssue>,
}

impl EnvFile {
    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }
}

pub fn parse(content: &str) -> EnvFile {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut file = EnvFile::default();

    for (idx, raw_line) in content.lines().enumerate() {
        let line_no = idx + 1;
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let line = match line.strip_prefix("export") {
            Some(rest) if rest.starts_with(char::is_whitespace) => rest.trim_start(),
            _ => line,
        };

        let (key_part, value_part) = match line.split_once('=') {
            Some(pair) => pair,
            None => {
                file.issues.push(ParseIssue {
                    line: line_no,
                    message: "expected KEY=VALUE".to_string(),
                });
                continue;
            }
        };

        let key = key_part.trim();
        if !is_valid_key(key) {
            file.issues.push(ParseIssue {
                line: line_no,
                message: format!("invalid variable name '{}'", key),
            });
            continue;
        }

        match parse_value(value_part) {
            Ok(value) => file.entries.push(Entry {
                key: key.to_string(),
                value,
                line: line_no,
            }),
            Err(message) => file.issues.push(ParseIssue {
                line: line_no,
                message,
            }),
        }
    }

    file
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

/// Parses everything to the right of the first `=`.
pub fn parse_value(right: &str) -> Result<String, String> {
    let trimmed = right.trim_start();
    match trimmed.chars().next() {
        Some(q) if q == '"' || q == '\'' => parse_quoted(&trimmed[1..], q),
        _ => Ok(strip_inline_comment(right).trim().to_string()),
    }
}

fn parse_quoted(rest: &str, quote: char) -> Result<String, String> {
    let mut value = String::new();
    let mut escaped = false;

    for (i, c) in rest.char_indices() {
        if escaped {
            match c {
                'n' => value.push('\n'),
                other => value.push(other),
            }
            escaped = false;
            continue;
        }
        if c == '\\' && quote == '"' {
            escaped = true;
            continue;
        }
        if c == quote {
            let tail = rest[i + c.len_utf8()..].trim();
            if tail.is_empty() || tail.starts_with('#') {
                return Ok(value);
            }
            return Err("unexpected text after closing quote".to_string());
        }
        value.push(c);
    }

    Err("unterminated quote".to_string())
}

/// Cuts an unquoted value at the first `#` that follows whitespace.
pub fn strip_inline_comment(s: &str) -> &str {
    let mut prev_is_space = false;
    for (i, c) in s.char_indices() {
        if c == '#' && prev_is_space {
            return &s[..i];
        }
        prev_is_space = c.is_whitespace();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(f: &EnvFile) -> Vec<(&str, &str)> {
        f.entries
            .iter()
            .map(|e| (e.key.as_str(), e.value.as_str()))
            .collect()
    }

    #[test]
    fn parses_simple_pairs() {
        let f = parse("A=1\nB=two\n");
        assert_eq!(pairs(&f), vec![("A", "1"), ("B", "two")]);
        assert!(f.issues.is_empty());
    }

    #[test]
    fn records_line_numbers() {
        let f = parse("# comment\n\nA=1\nB=2\n");
        assert_eq!(f.entries[0].line, 3);
        assert_eq!(f.entries[1].line, 4);
    }

    #[test]
    fn skips_comments_and_blank_lines() {
        let f = parse("# hello\n\n   \n  # indented\nA=1\n");
        assert_eq!(pairs(&f), vec![("A", "1")]);
        assert!(f.issues.is_empty());
    }

    #[test]
    fn accepts_export_prefix() {
        let f = parse("export A=1\nexport   B=2\n");
        assert_eq!(pairs(&f), vec![("A", "1"), ("B", "2")]);
    }

    #[test]
    fn key_named_like_export_is_not_confused() {
        let f = parse("EXPORT_DIR=/tmp\nexporter=1\n");
        assert_eq!(pairs(&f), vec![("EXPORT_DIR", "/tmp"), ("exporter", "1")]);
    }

    #[test]
    fn double_quotes_keep_hash_and_spaces() {
        let f = parse("A=\"x # y\"\n");
        assert_eq!(pairs(&f), vec![("A", "x # y")]);
    }

    #[test]
    fn single_quotes_are_literal() {
        let f = parse("A='a\\nb'\n");
        assert_eq!(pairs(&f), vec![("A", "a\\nb")]);
    }

    #[test]
    fn double_quote_escapes() {
        let f = parse("A=\"say \\\"hi\\\"\\n\"\n");
        assert_eq!(pairs(&f), vec![("A", "say \"hi\"\n")]);
    }

    #[test]
    fn inline_comment_after_whitespace_is_stripped() {
        let f = parse("A=1 # note\nB=2\t# tab note\n");
        assert_eq!(pairs(&f), vec![("A", "1"), ("B", "2")]);
    }

    #[test]
    fn hash_without_leading_space_is_part_of_value() {
        let f = parse("A=abc#def\nB=#start\n");
        assert_eq!(pairs(&f), vec![("A", "abc#def"), ("B", "#start")]);
    }

    #[test]
    fn empty_value_with_comment() {
        let f = parse("A= # fill me in\n");
        assert_eq!(pairs(&f), vec![("A", "")]);
    }

    #[test]
    fn equals_sign_inside_value() {
        let f = parse("URL=http://x.test/?a=b&c=d\n");
        assert_eq!(pairs(&f), vec![("URL", "http://x.test/?a=b&c=d")]);
    }

    #[test]
    fn spaces_around_equals() {
        let f = parse("A = 1\n");
        assert_eq!(pairs(&f), vec![("A", "1")]);
    }

    #[test]
    fn handles_crlf() {
        let f = parse("A=1\r\nB=2\r\n");
        assert_eq!(pairs(&f), vec![("A", "1"), ("B", "2")]);
    }

    #[test]
    fn handles_bom() {
        let f = parse("\u{feff}A=1\n");
        assert_eq!(pairs(&f), vec![("A", "1")]);
    }

    #[test]
    fn no_trailing_newline() {
        let f = parse("A=1");
        assert_eq!(pairs(&f), vec![("A", "1")]);
    }

    #[test]
    fn reports_line_without_equals() {
        let f = parse("A=1\nBROKEN\n");
        assert_eq!(f.entries.len(), 1);
        assert_eq!(f.issues.len(), 1);
        assert_eq!(f.issues[0].line, 2);
    }

    #[test]
    fn reports_invalid_names() {
        let f = parse("1A=x\nA-B=y\n=z\n");
        assert!(f.entries.is_empty());
        let lines: Vec<usize> = f.issues.iter().map(|i| i.line).collect();
        assert_eq!(lines, vec![1, 2, 3]);
    }

    #[test]
    fn reports_unterminated_quote() {
        let f = parse("A=\"oops\nB=2\n");
        assert_eq!(f.issues.len(), 1);
        assert_eq!(f.issues[0].line, 1);
        assert_eq!(pairs(&f), vec![("B", "2")]);
    }

    #[test]
    fn reports_text_after_closing_quote() {
        let f = parse("A=\"x\" y\n");
        assert_eq!(f.issues.len(), 1);
        assert!(f.entries.is_empty());
    }

    #[test]
    fn allows_trailing_comment_after_quote() {
        let f = parse("A=\"x\" # why\n");
        assert_eq!(pairs(&f), vec![("A", "x")]);
        assert!(f.issues.is_empty());
    }

    #[test]
    fn unicode_values() {
        let f = parse("GREETING=\"привет\"\nB=日本語\n");
        assert_eq!(pairs(&f), vec![("GREETING", "привет"), ("B", "日本語")]);
    }

    #[test]
    fn get_finds_first_entry() {
        let f = parse("A=1\nA=2\n");
        assert_eq!(f.get("A").map(|e| e.value.as_str()), Some("1"));
        assert!(f.get("Z").is_none());
    }
}

}

// ==========================================================================
// compare
// ==========================================================================

mod compare {
//! Compares an env file with its `.env.example`.

use std::collections::{HashMap, HashSet};

use crate::envfile::EnvFile;
use crate::finding::{Finding, Severity};

pub fn check(
    env_path: &str,
    env: &EnvFile,
    example_path: &str,
    example: &EnvFile,
) -> Vec<Finding> {
    let mut out = Vec::new();

    parse_issue_findings(env_path, env, &mut out);
    parse_issue_findings(example_path, example, &mut out);
    duplicate_findings(env_path, env, &mut out);
    duplicate_findings(example_path, example, &mut out);

    let env_keys: HashSet<&str> = env.entries.iter().map(|e| e.key.as_str()).collect();
    let example_keys: HashSet<&str> = example.entries.iter().map(|e| e.key.as_str()).collect();

    let mut reported: HashSet<&str> = HashSet::new();
    for entry in &example.entries {
        if !env_keys.contains(entry.key.as_str()) && reported.insert(entry.key.as_str()) {
            out.push(Finding::new(
                Severity::Error,
                "missing-key",
                env_path,
                None,
                format!(
                    "{} is defined in {} but missing in {}",
                    entry.key, example_path, env_path
                ),
            ));
        }
    }

    let mut reported: HashSet<&str> = HashSet::new();
    for entry in &env.entries {
        if !example_keys.contains(entry.key.as_str()) && reported.insert(entry.key.as_str()) {
            out.push(Finding::new(
                Severity::Warning,
                "extra-key",
                env_path,
                Some(entry.line),
                format!("{} is not documented in {}", entry.key, example_path),
            ));
        }
    }

    for entry in &env.entries {
        if entry.value.is_empty() {
            out.push(Finding::new(
                Severity::Warning,
                "empty-value",
                env_path,
                Some(entry.line),
                format!("{} has an empty value", entry.key),
            ));
        }
    }

    out
}

fn parse_issue_findings(path: &str, file: &EnvFile, out: &mut Vec<Finding>) {
    for issue in &file.issues {
        out.push(Finding::new(
            Severity::Error,
            "parse-error",
            path,
            Some(issue.line),
            issue.message.clone(),
        ));
    }
}

fn duplicate_findings(path: &str, file: &EnvFile, out: &mut Vec<Finding>) {
    let mut first_seen: HashMap<&str, usize> = HashMap::new();
    for entry in &file.entries {
        let previous = first_seen.get(entry.key.as_str()).copied();
        match previous {
            Some(first_line) => out.push(Finding::new(
                Severity::Error,
                "duplicate-key",
                path,
                Some(entry.line),
                format!(
                    "{} is defined more than once (first on line {})",
                    entry.key, first_line
                ),
            )),
            None => {
                first_seen.insert(entry.key.as_str(), entry.line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envfile::parse;

    fn run(env: &str, example: &str) -> Vec<Finding> {
        check(".env", &parse(env), ".env.example", &parse(example))
    }

    fn rules(findings: &[Finding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn identical_keys_are_clean() {
        let f = run("A=1\nB=2\n", "A=\nB=\n");
        assert!(f.is_empty(), "unexpected: {:?}", f);
    }

    #[test]
    fn order_does_not_matter() {
        let f = run("B=2\nA=1\n", "A=\nB=\n");
        assert!(f.is_empty());
    }

    #[test]
    fn missing_key_is_an_error() {
        let f = run("A=1\n", "A=\nDB_URL=\n");
        assert_eq!(rules(&f), vec!["missing-key"]);
        assert_eq!(f[0].severity, Severity::Error);
        assert_eq!(f[0].file, ".env");
        assert_eq!(f[0].line, None);
        assert!(f[0].message.contains("DB_URL"));
        assert!(f[0].message.contains(".env.example"));
    }

    #[test]
    fn extra_key_is_a_warning_with_line() {
        let f = run("A=1\nB=2\n", "A=\n");
        assert_eq!(rules(&f), vec!["extra-key"]);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].line, Some(2));
    }

    #[test]
    fn empty_value_is_a_warning() {
        let f = run("A=\nB=2\n", "A=\nB=\n");
        assert_eq!(rules(&f), vec!["empty-value"]);
        assert_eq!(f[0].severity, Severity::Warning);
        assert_eq!(f[0].line, Some(1));
    }

    #[test]
    fn duplicate_key_in_env_is_an_error() {
        let f = run("A=1\nB=2\nA=3\n", "A=\nB=\n");
        assert_eq!(rules(&f), vec!["duplicate-key"]);
        assert_eq!(f[0].line, Some(3));
        assert!(f[0].message.contains("first on line 1"));
    }

    #[test]
    fn duplicate_key_in_example_is_reported_once_with_missing() {
        let f = run("", "A=1\nA=2\n");
        assert_eq!(rules(&f), vec!["duplicate-key", "missing-key"]);
        assert_eq!(f[0].file, ".env.example");
    }

    #[test]
    fn parse_errors_come_from_both_files() {
        let f = run("A=1\nBROKEN\n", "A=\nALSO BROKEN\n");
        let parse_errors: Vec<&Finding> =
            f.iter().filter(|x| x.rule == "parse-error").collect();
        assert_eq!(parse_errors.len(), 2);
        assert_eq!(parse_errors[0].file, ".env");
        assert_eq!(parse_errors[1].file, ".env.example");
    }

    #[test]
    fn missing_key_is_reported_once_even_if_listed_twice() {
        let f = run("", "A=1\nA=2\n");
        let missing = f.iter().filter(|x| x.rule == "missing-key").count();
        assert_eq!(missing, 1);
    }

    #[test]
    fn extra_key_is_reported_once_even_if_listed_twice() {
        let f = run("Z=1\nZ=2\n", "");
        let extra = f.iter().filter(|x| x.rule == "extra-key").count();
        assert_eq!(extra, 1);
    }

    #[test]
    fn key_names_are_case_sensitive() {
        let f = run("api_key=1\n", "API_KEY=\n");
        assert_eq!(rules(&f), vec!["missing-key", "extra-key"]);
    }
}

}

// ==========================================================================
// secrets
// ==========================================================================

mod secrets {
//! Heuristic detection of hard-coded secrets, built on plain string
//! matching (no regex engine, no dependencies).
//!
//! The scanner is deliberately conservative: vendor-specific token formats are
//! matched exactly, while the generic "password = ..." rule ignores obvious
//! placeholders. Add `envguard:ignore` to a line to silence it.

use std::fs;
use std::path::Path;

use crate::envfile::strip_inline_comment;
use crate::finding::{Finding, Severity};

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_LINE_BYTES: usize = 4000;
const IGNORE_MARKER: &str = "envguard:ignore";
const PEM_BEGIN: &str = "-----BEGIN ";
const PEM_KIND: &str = "PRIVATE KEY-----";

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".idea",
    ".vscode",
];

const SENSITIVE_WORDS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "private_key",
    "access_key",
    "auth_key",
];

const BENIGN_SUFFIXES: &[&str] = &[
    "_type", "_name", "_field", "_label", "_length", "_file", "_path", "_header", "_prefix",
    "_ttl", "_env",
];

struct TokenRule {
    id: &'static str,
    label: &'static str,
    prefixes: &'static [&'static str],
    tail_min: usize,
    exact: bool,
    allowed: fn(char) -> bool,
}

fn is_upper_digit(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

fn is_alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

fn is_alnum_dash(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-'
}

fn is_alnum_underscore(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_alnum_underscore_dash(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

const TOKEN_RULES: &[TokenRule] = &[
    TokenRule {
        id: "aws-access-key-id",
        label: "AWS access key ID",
        prefixes: &["AKIA", "ASIA"],
        tail_min: 16,
        exact: true,
        allowed: is_upper_digit,
    },
    TokenRule {
        id: "github-token",
        label: "GitHub token",
        prefixes: &["ghp_", "gho_", "ghu_", "ghs_", "ghr_"],
        tail_min: 36,
        exact: false,
        allowed: is_alnum,
    },
    TokenRule {
        id: "github-token",
        label: "GitHub fine-grained token",
        prefixes: &["github_pat_"],
        tail_min: 22,
        exact: false,
        allowed: is_alnum_underscore,
    },
    TokenRule {
        id: "slack-token",
        label: "Slack token",
        prefixes: &["xoxb-", "xoxp-", "xoxa-", "xoxr-", "xoxs-"],
        tail_min: 10,
        exact: false,
        allowed: is_alnum_dash,
    },
    TokenRule {
        id: "stripe-secret-key",
        label: "Stripe secret key",
        prefixes: &["sk_live_", "rk_live_"],
        tail_min: 24,
        exact: false,
        allowed: is_alnum,
    },
    TokenRule {
        id: "google-api-key",
        label: "Google API key",
        prefixes: &["AIza"],
        tail_min: 35,
        exact: false,
        allowed: is_alnum_underscore_dash,
    },
];

/// Scans a file or a directory tree. `excludes` holds file or directory names
/// that are skipped wherever they appear.
pub fn scan_path(root: &Path, excludes: &[String]) -> Vec<Finding> {
    let mut out = Vec::new();
    if root.is_file() {
        scan_file(root, &mut out);
    } else {
        walk(root, excludes, &mut out);
    }
    out
}

fn walk(dir: &Path, excludes: &[String], out: &mut Vec<Finding>) {
    let read_dir = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            out.push(unreadable(&display_path(dir), &e));
            return;
        }
    };

    let mut entries: Vec<fs::DirEntry> = read_dir.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if excludes.iter().any(|x| *x == name) {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            walk(&path, excludes, out);
        } else if file_type.is_file() {
            scan_file(&path, out);
        }
    }
}

fn scan_file(path: &Path, out: &mut Vec<Finding>) {
    let display = display_path(path);
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    if should_skip_file(&file_name) {
        return;
    }

    match fs::metadata(path) {
        Ok(meta) => {
            if meta.len() > MAX_FILE_BYTES {
                return;
            }
        }
        Err(e) => {
            out.push(unreadable(&display, &e));
            return;
        }
    }

    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            out.push(unreadable(&display, &e));
            return;
        }
    };

    let head = &bytes[..bytes.len().min(8192)];
    if head.contains(&0u8) {
        return; // binary file
    }

    let text = String::from_utf8_lossy(&bytes);
    out.extend(scan_text(&display, &file_name, &text));
}

fn unreadable(display: &str, e: &std::io::Error) -> Finding {
    Finding::new(
        Severity::Warning,
        "unreadable",
        display,
        None,
        format!("could not read: {}", e),
    )
}

fn display_path(path: &Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    s.strip_prefix("./").unwrap_or(&s).to_string()
}

fn should_skip_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".lock")
        || lower == "package-lock.json"
        || lower.ends_with(".min.js")
        || lower.ends_with(".min.css")
        || lower.ends_with(".map")
}

fn is_template_file(lower_name: &str) -> bool {
    ["example", "sample", "template", "dist", "tpl", "tmpl"]
        .iter()
        .any(|ext| lower_name.ends_with(&format!(".{}", ext)))
}

/// Files where `KEY=value` or `key: value` without quotes is normal syntax.
fn is_config_style(lower_name: &str) -> bool {
    if lower_name == ".env" || lower_name.starts_with(".env.") || lower_name.ends_with(".env") {
        return true;
    }
    match lower_name.rsplit_once('.') {
        Some((_, ext)) => matches!(
            ext,
            "yml" | "yaml" | "toml" | "ini" | "cfg" | "conf" | "properties" | "env"
        ),
        None => false,
    }
}

/// Scans in-memory text. `display` is used in findings, `file_name` decides
/// which rules apply (for example templates such as `.env.example`).
pub fn scan_text(display: &str, file_name: &str, text: &str) -> Vec<Finding> {
    let lower_name = file_name.to_ascii_lowercase();
    let template = is_template_file(&lower_name);
    let config_style = is_config_style(&lower_name);
    let mut out = Vec::new();

    for (idx, line) in text.lines().enumerate() {
        if line.len() > MAX_LINE_BYTES || line.contains(IGNORE_MARKER) {
            continue;
        }
        let line_no = idx + 1;

        let before = out.len();
        scan_line_tokens(line, display, line_no, &mut out);
        if out.len() > before {
            continue;
        }

        if let Some((user, host)) = find_url_credentials(line) {
            out.push(Finding::new(
                Severity::Error,
                "credentials-in-url",
                display,
                Some(line_no),
                format!("password embedded in URL (user '{}', host '{}')", user, host),
            ));
            continue;
        }

        if template {
            continue;
        }

        if let Some((name, value)) = find_generic_secret(line, config_style) {
            out.push(Finding::new(
                Severity::Error,
                "generic-secret",
                display,
                Some(line_no),
                format!(
                    "possible hard-coded secret in '{}' (value {})",
                    name,
                    redact(&value)
                ),
            ));
        }
    }

    out
}

fn scan_line_tokens(line: &str, display: &str, line_no: usize, out: &mut Vec<Finding>) {
    if line.contains(PEM_BEGIN) && line.contains(PEM_KIND) {
        out.push(Finding::new(
            Severity::Error,
            "private-key",
            display,
            Some(line_no),
            "private key block found".to_string(),
        ));
    }

    for rule in TOKEN_RULES {
        for prefix in rule.prefixes {
            if let Some(token) = find_token(line, prefix, rule.tail_min, rule.exact, rule.allowed)
            {
                if token.ends_with("EXAMPLE") {
                    continue; // documentation placeholders such as the AWS sample key
                }
                out.push(Finding::new(
                    Severity::Error,
                    rule.id,
                    display,
                    Some(line_no),
                    format!("possible {} ({})", rule.label, redact(token)),
                ));
                break;
            }
        }
    }
}

/// Finds `prefix` followed by at least `tail_min` allowed characters.
/// With `exact`, the tail must be exactly `tail_min` long and must not be
/// followed by another alphanumeric character. All allowed characters are
/// ASCII, so character counts equal byte counts.
fn find_token<'a>(
    line: &'a str,
    prefix: &str,
    tail_min: usize,
    exact: bool,
    allowed: fn(char) -> bool,
) -> Option<&'a str> {
    for (start, _) in line.match_indices(prefix) {
        if let Some(prev) = line[..start].chars().next_back() {
            if prev.is_ascii_alphanumeric() {
                continue;
            }
        }

        let tail_start = start + prefix.len();
        let tail = &line[tail_start..];
        let mut count = 0usize;
        for c in tail.chars() {
            if allowed(c) {
                count += 1;
            } else {
                break;
            }
        }

        if count < tail_min {
            continue;
        }
        if exact {
            if count != tail_min {
                continue;
            }
            if let Some(next) = tail[count..].chars().next() {
                if next.is_ascii_alphanumeric() {
                    continue;
                }
            }
        }

        return Some(&line[start..tail_start + count]);
    }
    None
}

/// Returns `(user, host)` for `scheme://user:password@host` with a real-looking
/// password. Local development hosts and placeholders are ignored.
fn find_url_credentials(line: &str) -> Option<(String, String)> {
    for (idx, _) in line.match_indices("://") {
        let after = &line[idx + 3..];
        let end = after
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c == '"' || c == '\'' || c == '`'
                    || c.is_whitespace()
            })
            .unwrap_or(after.len());
        let authority = &after[..end];

        let (userinfo, host) = match authority.rsplit_once('@') {
            Some(parts) => parts,
            None => continue,
        };
        let (user, password) = match userinfo.split_once(':') {
            Some(parts) => parts,
            None => continue,
        };
        if user.is_empty() || password.is_empty() {
            continue;
        }

        let host_only = host.split(':').next().unwrap_or("").to_ascii_lowercase();
        if host_only == "localhost" || host_only == "127.0.0.1" || host_only == "0.0.0.0" {
            continue;
        }
        if is_placeholder(password, 3) {
            continue;
        }
        return Some((user.to_string(), host_only));
    }
    None
}

/// Returns `(name, value)` for assignments such as `DB_PASSWORD=...`,
/// `"api_key": "..."` or `token = '...'`.
fn find_generic_secret(line: &str, config_style: bool) -> Option<(String, String)> {
    let pos = line.find(|c: char| c == '=' || c == ':')?;
    let bytes = line.as_bytes();
    if bytes[pos] == b'=' {
        if pos > 0 && matches!(bytes[pos - 1], b'!' | b'<' | b'>' | b'=') {
            return None; // comparison operators
        }
        if matches!(bytes.get(pos + 1).copied(), Some(b'=') | Some(b'>')) {
            return None; // `==` and `=>`
        }
    }

    let left = line[..pos]
        .trim_end()
        .trim_end_matches(|c: char| c == '"' || c == '\'' || c == '`');
    let mut start = left.len();
    for (i, c) in left.char_indices().rev() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
            start = i;
        } else {
            break;
        }
    }
    let name = &left[start..];
    if name.is_empty() {
        return None;
    }

    let lname = name.to_ascii_lowercase().replace('-', "_").replace('.', "_");
    if !SENSITIVE_WORDS.iter().any(|w| lname.contains(*w)) {
        return None;
    }
    if BENIGN_SUFFIXES.iter().any(|s| lname.ends_with(*s)) {
        return None;
    }

    let value = candidate_value(&line[pos + 1..], config_style)?;
    if value.starts_with('/') || value.starts_with("./") || value.contains("://") {
        return None;
    }
    if is_placeholder(&value, 8) {
        return None;
    }
    Some((name.to_string(), value))
}

/// Quoted values are always candidates; unquoted ones only in config-style files.
fn candidate_value(right: &str, config_style: bool) -> Option<String> {
    let t = right.trim();
    let first = t.chars().next()?;
    if first == '"' || first == '\'' {
        let inner = &t[1..];
        let end = inner.find(first)?;
        return Some(inner[..end].to_string());
    }
    if config_style {
        let v = strip_inline_comment(right).trim();
        if v.is_empty() {
            return None;
        }
        return Some(v.to_string());
    }
    None
}

fn is_placeholder(value: &str, min_len: usize) -> bool {
    if value.chars().count() < min_len {
        return true;
    }
    if value.starts_with('$')
        || value.starts_with('{')
        || value.starts_with('<')
        || value.starts_with('%')
    {
        return true;
    }
    if value.chars().any(char::is_whitespace) {
        return true;
    }
    let first = value.chars().next().unwrap_or(' ');
    if value.chars().all(|c| c == first) {
        return true; // "xxxxxxxx", "********", "00000000"
    }

    let lower = value.to_ascii_lowercase();
    const EXACT: &[&str] = &[
        "password",
        "passwd",
        "secret",
        "token",
        "changeme",
        "admin",
        "default",
        "pass",
        "user",
        "test",
        "null",
        "none",
        "undefined",
        "true",
        "false",
    ];
    if EXACT.contains(&lower.as_str()) {
        return true;
    }
    const CONTAINS: &[&str] = &[
        "changeme",
        "change_me",
        "change-me",
        "your_",
        "your-",
        "<your",
        "xxxx",
        "example",
        "placeholder",
        "dummy",
        "sample",
        "redacted",
        "replace_me",
        "insert_",
        "todo",
        "fixme",
        "process.env",
        "getenv",
        "environ",
    ];
    CONTAINS.iter().any(|p| lower.contains(*p))
}

/// Never print a whole secret: long values keep four characters, short ones none.
fn redact(s: &str) -> String {
    if s.chars().count() >= 12 {
        let head: String = s.chars().take(4).collect();
        format!("{}****", head)
    } else {
        "****".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn scan(name: &str, text: &str) -> Vec<Finding> {
        scan_text(name, name, text)
    }

    fn rules(findings: &[Finding]) -> Vec<&'static str> {
        findings.iter().map(|f| f.rule).collect()
    }

    fn fake_aws_key() -> String {
        ["AKIA", "Z7Q3M5K2P9R4T6W8"].concat()
    }

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "envguard-secrets-{}-{}-{}",
            std::process::id(),
            name,
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn detects_aws_access_key() {
        let key = fake_aws_key();
        let text = format!("aws_key = '{}'\n", key);
        let f = scan("config.py", &text);
        assert_eq!(rules(&f), vec!["aws-access-key-id"]);
        assert_eq!(f[0].line, Some(1));
        assert_eq!(f[0].severity, Severity::Error);
        assert!(!f[0].message.contains(&key), "secret must be redacted");
        assert!(f[0].message.contains("AKIA****"));
    }

    #[test]
    fn aws_key_inside_longer_word_is_ignored() {
        let text = format!("x{}\n", fake_aws_key());
        assert!(scan("a.txt", &text).is_empty());
    }

    #[test]
    fn aws_key_followed_by_more_characters_is_ignored() {
        let text = format!("{}ABCD\n", fake_aws_key());
        assert!(scan("a.txt", &text).is_empty());
    }

    #[test]
    fn aws_documented_example_key_is_ignored() {
        assert!(scan("a.txt", "AWS_KEY=AKIAIOSFODNN7EXAMPLE\n").is_empty());
    }

    #[test]
    fn detects_github_token() {
        let token = format!("ghp_{}", "aB3".repeat(12)); // envguard:ignore
        let text = format!("export GH={}\n", token);
        let f = scan("deploy.sh", &text);
        assert_eq!(rules(&f), vec!["github-token"]);
    }

    #[test]
    fn short_github_token_is_ignored() {
        let token = format!("ghp_{}", "aB3".repeat(5));
        let text = format!("export GH={}\n", token);
        assert!(scan("deploy.sh", &text).is_empty());
    }

    #[test]
    fn detects_slack_token() {
        let token = format!("xoxb-{}", "1234567890-abcdef");
        let text = format!("SLACK={}\n", token);
        assert_eq!(rules(&scan("a.txt", &text)), vec!["slack-token"]);
    }

    #[test]
    fn detects_stripe_live_key() {
        let key = format!("sk_live_{}", "a1B2c3D4".repeat(3));
        let text = format!("STRIPE={}\n", key);
        assert_eq!(rules(&scan("a.txt", &text)), vec!["stripe-secret-key"]);
    }

    #[test]
    fn stripe_test_key_is_ignored() {
        let key = format!("sk_test_{}", "a1B2c3D4".repeat(3));
        let text = format!("STRIPE={}\n", key);
        assert!(scan("a.txt", &text).is_empty());
    }

    #[test]
    fn detects_google_api_key() {
        let key = format!("AIza{}", "A1b2C3d4E5".repeat(3) + "x1y2z");
        let text = format!("const k = \"{}\";\n", key);
        assert_eq!(rules(&scan("a.js", &text)), vec!["google-api-key"]);
    }

    #[test]
    fn detects_private_key_header() {
        let header = ["-----BEGIN ", "RSA PRIVATE KEY", "-----"].concat(); // envguard:ignore
        let f = scan("id_rsa", &header);
        assert_eq!(rules(&f), vec!["private-key"]);
    }

    #[test]
    fn public_key_header_is_fine() {
        let header = ["-----BEGIN ", "PUBLIC KEY", "-----"].concat();
        assert!(scan("id_rsa.pub", &header).is_empty());
    }

    #[test]
    fn detects_generic_secret_in_dotenv() {
        let f = scan(".env", "DB_PASSWORD=s3cr3tValue99\n"); // envguard:ignore
        assert_eq!(rules(&f), vec!["generic-secret"]);
        assert_eq!(f[0].line, Some(1));
        assert!(!f[0].message.contains("s3cr3tValue99"));
    }

    #[test]
    fn detects_generic_secret_with_export_and_quotes() {
        let f = scan(".env.local", "export API_TOKEN=\"abcd1234efgh5678\"\n"); // envguard:ignore
        assert_eq!(rules(&f), vec!["generic-secret"]);
    }

    #[test]
    fn detects_quoted_secret_in_source_code() {
        let f = scan("settings.py", "API_KEY = \"abcd1234efgh5678\"\n"); // envguard:ignore
        assert_eq!(rules(&f), vec!["generic-secret"]);
    }

    #[test]
    fn detects_json_style_secret() {
        let f = scan("config.json", "  \"client_secret\": \"abcd1234efgh5678\",\n"); // envguard:ignore
        assert_eq!(rules(&f), vec!["generic-secret"]);
    }

    #[test]
    fn detects_unquoted_yaml_secret() {
        let f = scan("app.yml", "  db_password: s3cr3tValue99\n"); // envguard:ignore
        assert_eq!(rules(&f), vec!["generic-secret"]);
    }

    #[test]
    fn unquoted_values_in_source_code_are_ignored() {
        assert!(scan("a.py", "password = get_password()\n").is_empty());
        assert!(scan("a.py", "API_KEY = os.environ[\"API_KEY\"]\n").is_empty());
    }

    #[test]
    fn placeholders_are_ignored() {
        let text = "\
PASSWORD=changeme
API_KEY=${API_KEY}
SECRET_KEY=your_secret_key_here
TOKEN=<token>
DB_PASSWORD=xxxxxxxxxxxx
AUTH_TOKEN=
";
        assert!(scan(".env", text).is_empty());
    }

    #[test]
    fn short_values_are_ignored() {
        assert!(scan(".env", "DB_PASSWORD=abc123\n").is_empty());
    }

    #[test]
    fn comparison_operators_are_not_assignments() {
        let text = "if password == \"abcdefgh1234\" { }\n"; // envguard:ignore
        assert!(scan("a.rs", text).is_empty());
    }

    #[test]
    fn benign_suffixes_are_ignored() {
        assert!(scan(".env", "TOKEN_TYPE=bearerbearer\nSECRET_FILE=abcd1234efgh\n").is_empty());
    }

    #[test]
    fn templates_skip_generic_rule_but_not_vendor_rules() {
        assert!(scan(".env.example", "DB_PASSWORD=s3cr3tValue99\n").is_empty()); // envguard:ignore
        let text = format!("AWS={}\n", fake_aws_key());
        assert_eq!(
            rules(&scan(".env.example", &text)),
            vec!["aws-access-key-id"]
        );
    }

    #[test]
    fn detects_password_in_url() {
        let url = ["postgres://admin:", "s3cretPass9", "@db.prod.internal/app"].concat();
        let text = format!("DATABASE_URL={}\n", url);
        let f = scan(".env", &text);
        assert_eq!(rules(&f), vec!["credentials-in-url"]);
        assert!(!f[0].message.contains("s3cretPass9"));
        assert!(f[0].message.contains("db.prod.internal"));
    }

    #[test]
    fn localhost_and_placeholder_urls_are_ignored() {
        let text = "\
A=postgres://postgres:postgres@localhost:5432/app
B=https://user:${PASS}@example.com/x
C=https://user@example.com/x
";
        assert!(scan(".env", text).is_empty());
    }

    #[test]
    fn ignore_marker_silences_a_line() {
        let f = scan(".env", "API_KEY=abcd1234efgh5678 # envguard:ignore\n");
        assert!(f.is_empty());
    }

    #[test]
    fn reports_correct_line_numbers() {
        let text = format!("one\ntwo\nkey: {}\n", fake_aws_key());
        let f = scan("notes.txt", &text);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].line, Some(3));
    }

    #[test]
    fn one_finding_per_rule_per_line() {
        let key = fake_aws_key();
        let text = format!("{} {}\n", key, key);
        assert_eq!(scan("a.txt", &text).len(), 1);
    }

    #[test]
    fn redact_keeps_at_most_four_characters() {
        assert_eq!(redact("short"), "****");
        assert_eq!(redact(&fake_aws_key()), "AKIA****");
    }

    #[test]
    fn scan_path_walks_and_skips() {
        let root = temp_dir("walk");
        let key = fake_aws_key();
        fs::write(root.join("app.py"), format!("k = '{}'\n", key)).unwrap();

        fs::create_dir_all(root.join(".git")).unwrap();
        fs::write(root.join(".git").join("config"), format!("k = '{}'\n", key)).unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules").join("x.js"), format!("k = '{}'\n", key)).unwrap();
        fs::write(root.join("Cargo.lock"), format!("k = '{}'\n", key)).unwrap();

        let mut binary = key.clone().into_bytes();
        binary.push(0);
        fs::write(root.join("blob.dat"), binary).unwrap();

        let f = scan_path(&root, &[]);
        assert_eq!(f.len(), 1, "unexpected findings: {:?}", f);
        assert!(f[0].file.ends_with("app.py"));
        assert_eq!(f[0].rule, "aws-access-key-id");
    }

    #[test]
    fn scan_path_respects_excludes() {
        let root = temp_dir("exclude");
        let key = fake_aws_key();
        fs::create_dir_all(root.join("fixtures")).unwrap();
        fs::write(root.join("fixtures").join("a.txt"), format!("k = '{}'\n", key)).unwrap();
        fs::write(root.join("b.txt"), format!("k = '{}'\n", key)).unwrap();

        let all = scan_path(&root, &[]);
        assert_eq!(all.len(), 2);

        let some = scan_path(&root, &["fixtures".to_string()]);
        assert_eq!(some.len(), 1);
        assert!(some[0].file.ends_with("b.txt"));
    }

    #[test]
    fn scan_path_accepts_a_single_file() {
        let root = temp_dir("single");
        let file = root.join("only.txt");
        fs::write(&file, format!("k = '{}'\n", fake_aws_key())).unwrap();
        let f = scan_path(&file, &[]);
        assert_eq!(f.len(), 1);
    }

    #[test]
    fn scan_path_results_are_sorted_by_name() {
        let root = temp_dir("sorted");
        let key = fake_aws_key();
        for name in ["c.txt", "a.txt", "b.txt"] {
            fs::write(root.join(name), format!("k = '{}'\n", key)).unwrap();
        }
        let f = scan_path(&root, &[]);
        let names: Vec<String> = f
            .iter()
            .map(|x| x.file.rsplit('/').next().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.txt", "b.txt", "c.txt"]);
    }

    #[test]
    fn display_path_strips_leading_dot_slash() {
        assert_eq!(display_path(Path::new("./src/a.rs")), "src/a.rs");
        assert_eq!(display_path(Path::new("src/a.rs")), "src/a.rs");
    }
}

}

// ==========================================================================
// report
// ==========================================================================

mod report {
//! Output formats: human-readable text, JSON, and GitHub Actions annotations.

use crate::finding::{Finding, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Text,
    Json,
    Github,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        match s {
            "text" => Some(Format::Text),
            "json" => Some(Format::Json),
            "github" => Some(Format::Github),
            _ => None,
        }
    }
}

/// Returns `(errors, warnings)`.
pub fn counts(findings: &[Finding]) -> (usize, usize) {
    let errors = findings
        .iter()
        .filter(|f| f.severity == Severity::Error)
        .count();
    (errors, findings.len() - errors)
}

pub fn render(findings: &[Finding], format: Format) -> String {
    match format {
        Format::Text => render_text(findings),
        Format::Json => render_json(findings),
        Format::Github => render_github(findings),
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{} {}", n, word)
    } else {
        format!("{} {}s", n, word)
    }
}

fn render_text(findings: &[Finding]) -> String {
    let mut s = String::new();
    for f in findings {
        let location = match f.line {
            Some(line) => format!("{}:{}", f.file, line),
            None => f.file.clone(),
        };
        s.push_str(&format!(
            "{}: {}[{}]: {}\n",
            location,
            f.severity.as_str(),
            f.rule,
            f.message
        ));
    }

    if findings.is_empty() {
        s.push_str("No problems found.\n");
    } else {
        let (errors, warnings) = counts(findings);
        s.push_str(&format!(
            "\n{}, {}\n",
            plural(errors, "error"),
            plural(warnings, "warning")
        ));
    }
    s
}

fn render_json(findings: &[Finding]) -> String {
    let mut s = String::from("{\"findings\":[");
    for (i, f) in findings.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let line = match f.line {
            Some(l) => l.to_string(),
            None => "null".to_string(),
        };
        s.push_str(&format!(
            "{{\"severity\":\"{}\",\"rule\":\"{}\",\"file\":\"{}\",\"line\":{},\"message\":\"{}\"}}",
            f.severity.as_str(),
            json_escape(f.rule),
            json_escape(&f.file),
            line,
            json_escape(&f.message)
        ));
    }
    let (errors, warnings) = counts(findings);
    s.push_str(&format!(
        "],\"errors\":{},\"warnings\":{}}}\n",
        errors, warnings
    ));
    s
}

fn render_github(findings: &[Finding]) -> String {
    let mut s = String::new();
    for f in findings {
        let level = f.severity.as_str();
        let mut props = format!("file={}", escape_property(&f.file));
        if let Some(line) = f.line {
            props.push_str(&format!(",line={}", line));
        }
        props.push_str(&format!(",title={}", escape_property(f.rule)));
        s.push_str(&format!("::{} {}::{}\n", level, props, escape_data(&f.message)));
    }
    s
}

pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn escape_data(s: &str) -> String {
    s.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

fn escape_property(s: &str) -> String {
    escape_data(s).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Finding> {
        vec![
            Finding::new(
                Severity::Error,
                "missing-key",
                ".env",
                None,
                "DB_URL is missing".to_string(),
            ),
            Finding::new(
                Severity::Warning,
                "extra-key",
                ".env",
                Some(7),
                "OLD is not documented".to_string(),
            ),
        ]
    }

    #[test]
    fn counts_errors_and_warnings() {
        assert_eq!(counts(&sample()), (1, 1));
        assert_eq!(counts(&[]), (0, 0));
    }

    #[test]
    fn text_output_has_locations_and_summary() {
        let text = render(&sample(), Format::Text);
        assert!(text.contains(".env: error[missing-key]: DB_URL is missing\n"));
        assert!(text.contains(".env:7: warning[extra-key]: OLD is not documented\n"));
        assert!(text.ends_with("\n1 error, 1 warning\n"));
    }

    #[test]
    fn text_output_pluralizes() {
        let mut findings = sample();
        findings.push(findings[1].clone());
        let text = render(&findings, Format::Text);
        assert!(text.ends_with("\n1 error, 2 warnings\n"));
    }

    #[test]
    fn text_output_for_clean_run() {
        assert_eq!(render(&[], Format::Text), "No problems found.\n");
    }

    #[test]
    fn json_output_shape() {
        let json = render(&sample(), Format::Json);
        assert!(json.starts_with("{\"findings\":["));
        assert!(json.contains(
            "{\"severity\":\"error\",\"rule\":\"missing-key\",\"file\":\".env\",\"line\":null,\"message\":\"DB_URL is missing\"}"
        ));
        assert!(json.contains("\"line\":7"));
        assert!(json.ends_with("],\"errors\":1,\"warnings\":1}\n"));
    }

    #[test]
    fn json_output_for_clean_run() {
        assert_eq!(
            render(&[], Format::Json),
            "{\"findings\":[],\"errors\":0,\"warnings\":0}\n"
        );
    }

    #[test]
    fn json_escapes_special_characters() {
        assert_eq!(json_escape("a\"b\\c\nd\te"), "a\\\"b\\\\c\\nd\\te");
        assert_eq!(json_escape("\u{1}"), "\\u0001");
        assert_eq!(json_escape("привет"), "привет");
    }

    #[test]
    fn github_annotations() {
        let out = render(&sample(), Format::Github);
        assert!(out.contains("::error file=.env,title=missing-key::DB_URL is missing\n"));
        assert!(out.contains("::warning file=.env,line=7,title=extra-key::OLD is not documented\n"));
    }

    #[test]
    fn github_escapes_newlines_and_percent() {
        let f = vec![Finding::new(
            Severity::Error,
            "r",
            "a:b,c.txt",
            Some(1),
            "100%\nbad".to_string(),
        )];
        let out = render(&f, Format::Github);
        assert_eq!(
            out,
            "::error file=a%3Ab%2Cc.txt,line=1,title=r::100%25%0Abad\n"
        );
    }

    #[test]
    fn format_parse() {
        assert_eq!(Format::parse("text"), Some(Format::Text));
        assert_eq!(Format::parse("json"), Some(Format::Json));
        assert_eq!(Format::parse("github"), Some(Format::Github));
        assert_eq!(Format::parse("xml"), None);
    }
}

}

// ==========================================================================
// cli
// ==========================================================================

mod cli {
//! Command-line interface. `run` is separate from `main` so it can be tested
//! without spawning a process.

use std::fs;
use std::io::Write;
use std::path::Path;

use crate::compare;
use crate::envfile;
use crate::finding::Finding;
use crate::report::{self, Format};
use crate::secrets;

const USAGE: &str = "\
envguard: validate .env files and scan projects for leaked secrets

USAGE:
    envguard check [--env FILE] [--example FILE] [OPTIONS]
    envguard scan [PATH] [OPTIONS]

COMMANDS:
    check    Compare an env file with its example file
    scan     Look for hard-coded secrets in a directory or a single file

OPTIONS:
    --env FILE        env file to validate (check only, default: .env)
    --example FILE    reference file (check only, default: .env.example)
    --exclude NAME    skip files or directories with this name (scan only, repeatable)
    --format FORMAT   text (default), json or github
    --strict          treat warnings as errors
    -h, --help        show this help
    -V, --version     show the version

EXIT CODES:
    0  no problems found
    1  problems found
    2  usage or I/O error

Add 'envguard:ignore' to a line to make scan skip it.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Check,
    Scan,
}

#[derive(Debug, Clone)]
struct Options {
    command: Command,
    format: Format,
    strict: bool,
    env: String,
    example: String,
    path: String,
    excludes: Vec<String>,
}

enum Parsed {
    Help,
    Version,
    Run(Options),
}

/// Runs envguard with the given arguments (without the program name) and
/// returns the process exit code.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(message) => {
            let _ = writeln!(err, "error: {}\n", message);
            let _ = write!(err, "{}", USAGE);
            return 2;
        }
    };

    match parsed {
        Parsed::Help => {
            let _ = write!(out, "{}", USAGE);
            0
        }
        Parsed::Version => {
            let _ = writeln!(out, "envguard {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Parsed::Run(opts) => execute(&opts, out, err),
    }
}

fn execute(opts: &Options, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let result = match opts.command {
        Command::Check => run_check(opts),
        Command::Scan => run_scan(opts),
    };

    let findings = match result {
        Ok(findings) => findings,
        Err(message) => {
            let _ = writeln!(err, "error: {}", message);
            return 2;
        }
    };

    let rendered = report::render(&findings, opts.format);
    let _ = out.write_all(rendered.as_bytes());

    let (errors, warnings) = report::counts(&findings);
    if errors > 0 || (opts.strict && warnings > 0) {
        1
    } else {
        0
    }
}

fn run_check(opts: &Options) -> Result<Vec<Finding>, String> {
    let env_text = read_text(&opts.env)?;
    let example_text = read_text(&opts.example)?;
    let env = envfile::parse(&env_text);
    let example = envfile::parse(&example_text);
    Ok(compare::check(&opts.env, &env, &opts.example, &example))
}

fn run_scan(opts: &Options) -> Result<Vec<Finding>, String> {
    let root = Path::new(&opts.path);
    if !root.exists() {
        return Err(format!("path '{}' does not exist", opts.path));
    }
    Ok(secrets::scan_path(root, &opts.excludes))
}

fn read_text(path: &str) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read '{}': {}", path, e))?;
    String::from_utf8(bytes).map_err(|_| format!("'{}' is not valid UTF-8", path))
}

fn parse_args(args: &[String]) -> Result<Parsed, String> {
    let mut iter = args.iter();

    let command = match iter.next().map(|s| s.as_str()) {
        None => return Err("missing command".to_string()),
        Some("-h") | Some("--help") | Some("help") => return Ok(Parsed::Help),
        Some("-V") | Some("--version") => return Ok(Parsed::Version),
        Some("check") => Command::Check,
        Some("scan") => Command::Scan,
        Some(other) => return Err(format!("unknown command '{}'", other)),
    };

    let mut opts = Options {
        command,
        format: Format::Text,
        strict: false,
        env: ".env".to_string(),
        example: ".env.example".to_string(),
        path: ".".to_string(),
        excludes: Vec::new(),
    };
    let mut path_given = false;

    while let Some(arg) = iter.next() {
        let arg = arg.as_str();
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (arg, None),
        };

        match name {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--strict" => opts.strict = true,
            "--format" => {
                let value = take_value(name, inline, &mut iter)?;
                opts.format = Format::parse(&value).ok_or_else(|| {
                    format!("unknown format '{}' (use text, json or github)", value)
                })?;
            }
            "--env" | "--example" if opts.command != Command::Check => {
                return Err(format!("option '{}' is only valid with 'check'", name));
            }
            "--exclude" if opts.command != Command::Scan => {
                return Err(format!("option '{}' is only valid with 'scan'", name));
            }
            "--env" => opts.env = take_value(name, inline, &mut iter)?,
            "--example" => opts.example = take_value(name, inline, &mut iter)?,
            "--exclude" => {
                let value = take_value(name, inline, &mut iter)?;
                opts.excludes.push(value);
            }
            _ if name.starts_with('-') && name.len() > 1 => {
                return Err(format!("unknown option '{}'", name));
            }
            positional => {
                if opts.command == Command::Scan && !path_given {
                    opts.path = positional.to_string();
                    path_given = true;
                } else {
                    return Err(format!("unexpected argument '{}'", positional));
                }
            }
        }
    }

    Ok(Parsed::Run(opts))
}

fn take_value(
    name: &str,
    inline: Option<&str>,
    iter: &mut std::slice::Iter<'_, String>,
) -> Result<String, String> {
    match inline {
        Some(value) => Ok(value.to_string()),
        None => match iter.next() {
            Some(value) => Ok(value.clone()),
            None => Err(format!("option '{}' requires a value", name)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(name: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "envguard-cli-{}-{}-{}",
            std::process::id(),
            name,
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        fs::write(&path, content).unwrap();
        path.to_string_lossy().to_string()
    }

    fn run_args(args: &[&str]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let mut out: Vec<u8> = Vec::new();
        let mut err: Vec<u8> = Vec::new();
        let code = run(&args, &mut out, &mut err);
        (
            code,
            String::from_utf8_lossy(&out).to_string(),
            String::from_utf8_lossy(&err).to_string(),
        )
    }

    fn fake_aws_key() -> String {
        ["AKIA", "Z7Q3M5K2P9R4T6W8"].concat()
    }

    #[test]
    fn no_arguments_is_a_usage_error() {
        let (code, out, err) = run_args(&[]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.contains("missing command"));
        assert!(err.contains("USAGE"));
    }

    #[test]
    fn help_and_version() {
        let (code, out, _) = run_args(&["--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("USAGE"));

        let (code, out, _) = run_args(&["check", "--help"]);
        assert_eq!(code, 0);
        assert!(out.contains("USAGE"));

        let (code, out, _) = run_args(&["--version"]);
        assert_eq!(code, 0);
        assert!(out.starts_with("envguard "));
    }

    #[test]
    fn unknown_command_and_option() {
        let (code, _, err) = run_args(&["frobnicate"]);
        assert_eq!(code, 2);
        assert!(err.contains("unknown command 'frobnicate'"));

        let (code, _, err) = run_args(&["scan", "--nope"]);
        assert_eq!(code, 2);
        assert!(err.contains("unknown option '--nope'"));
    }

    #[test]
    fn options_are_checked_against_the_command() {
        let (code, _, err) = run_args(&["scan", "--env", "x"]);
        assert_eq!(code, 2);
        assert!(err.contains("only valid with 'check'"));

        let (code, _, err) = run_args(&["check", "--exclude", "x"]);
        assert_eq!(code, 2);
        assert!(err.contains("only valid with 'scan'"));
    }

    #[test]
    fn option_without_value_is_an_error() {
        let (code, _, err) = run_args(&["check", "--env"]);
        assert_eq!(code, 2);
        assert!(err.contains("requires a value"));
    }

    #[test]
    fn bad_format_is_an_error() {
        let (code, _, err) = run_args(&["scan", "--format", "xml"]);
        assert_eq!(code, 2);
        assert!(err.contains("unknown format 'xml'"));
    }

    #[test]
    fn extra_positional_arguments_are_rejected() {
        let (code, _, err) = run_args(&["scan", "a", "b"]);
        assert_eq!(code, 2);
        assert!(err.contains("unexpected argument 'b'"));

        let (code, _, err) = run_args(&["check", "a"]);
        assert_eq!(code, 2);
        assert!(err.contains("unexpected argument 'a'"));
    }

    #[test]
    fn check_clean_exits_zero() {
        let dir = temp_dir("clean");
        let env = write(&dir, ".env", "A=1\nB=2\n");
        let example = write(&dir, ".env.example", "A=\nB=\n");
        let (code, out, err) = run_args(&["check", "--env", &env, "--example", &example]);
        assert_eq!(code, 0, "stderr: {}", err);
        assert!(out.contains("No problems found."));
    }

    #[test]
    fn check_missing_key_exits_one() {
        let dir = temp_dir("missing");
        let env = write(&dir, ".env", "A=1\n");
        let example = write(&dir, ".env.example", "A=\nB=\n");
        let (code, out, _) = run_args(&["check", "--env", &env, "--example", &example]);
        assert_eq!(code, 1);
        assert!(out.contains("error[missing-key]"));
        assert!(out.contains("B is defined in"));
        assert!(out.contains("1 error, 0 warnings"));
    }

    #[test]
    fn warnings_only_pass_unless_strict() {
        let dir = temp_dir("warn");
        let env = write(&dir, ".env", "A=1\nEXTRA=2\n");
        let example = write(&dir, ".env.example", "A=\n");

        let (code, out, _) = run_args(&["check", "--env", &env, "--example", &example]);
        assert_eq!(code, 0);
        assert!(out.contains("warning[extra-key]"));

        let (code, _, _) = run_args(&["check", "--strict", "--env", &env, "--example", &example]);
        assert_eq!(code, 1);
    }

    #[test]
    fn check_reports_duplicates_and_parse_errors() {
        let dir = temp_dir("dups");
        let env = write(&dir, ".env", "A=1\nA=2\nBROKEN\n");
        let example = write(&dir, ".env.example", "A=\n");
        let (code, out, _) = run_args(&["check", "--env", &env, "--example", &example]);
        assert_eq!(code, 1);
        assert!(out.contains("error[duplicate-key]"));
        assert!(out.contains("error[parse-error]"));
        assert!(out.contains("2 errors"));
    }

    #[test]
    fn check_supports_inline_option_values() {
        let dir = temp_dir("inline");
        let env = write(&dir, ".env", "A=1\n");
        let example = write(&dir, ".env.example", "A=\n");
        let env_arg = format!("--env={}", env);
        let example_arg = format!("--example={}", example);
        let (code, _, err) = run_args(&["check", &env_arg, &example_arg, "--format=json"]);
        assert_eq!(code, 0, "stderr: {}", err);
    }

    #[test]
    fn check_missing_file_exits_two() {
        let dir = temp_dir("nofile");
        let example = write(&dir, ".env.example", "A=\n");
        let missing = dir.join("nope.env").to_string_lossy().to_string();
        let (code, out, err) = run_args(&["check", "--env", &missing, "--example", &example]);
        assert_eq!(code, 2);
        assert!(out.is_empty());
        assert!(err.contains("cannot read"));
    }

    #[test]
    fn check_json_format() {
        let dir = temp_dir("json");
        let env = write(&dir, ".env", "A=1\n");
        let example = write(&dir, ".env.example", "A=\nB=\n");
        let (code, out, _) = run_args(&[
            "check", "--format", "json", "--env", &env, "--example", &example,
        ]);
        assert_eq!(code, 1);
        assert!(out.starts_with("{\"findings\":["));
        assert!(out.contains("\"rule\":\"missing-key\""));
        assert!(out.contains("\"errors\":1"));
    }

    #[test]
    fn scan_finds_secret_and_hides_it() {
        let dir = temp_dir("scan");
        let key = fake_aws_key();
        write(&dir, "app.py", &format!("k = '{}'\n", key));
        let root = dir.to_string_lossy().to_string();
        let (code, out, _) = run_args(&["scan", &root]);
        assert_eq!(code, 1);
        assert!(out.contains("error[aws-access-key-id]"));
        assert!(out.contains("app.py:1:"));
        assert!(!out.contains(&key));
    }

    #[test]
    fn scan_clean_directory_exits_zero() {
        let dir = temp_dir("scanclean");
        write(&dir, "readme.txt", "nothing to see here\n");
        let root = dir.to_string_lossy().to_string();
        let (code, out, _) = run_args(&["scan", &root]);
        assert_eq!(code, 0);
        assert!(out.contains("No problems found."));
    }

    #[test]
    fn scan_exclude_option() {
        let dir = temp_dir("scanexclude");
        write(&dir, "app.py", &format!("k = '{}'\n", fake_aws_key()));
        let root = dir.to_string_lossy().to_string();
        let (code, _, _) = run_args(&["scan", &root, "--exclude", "app.py"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn scan_missing_path_exits_two() {
        let dir = temp_dir("scanmissing");
        let missing = dir.join("nope").to_string_lossy().to_string();
        let (code, _, err) = run_args(&["scan", &missing]);
        assert_eq!(code, 2);
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn scan_github_format() {
        let dir = temp_dir("scangh");
        write(&dir, "app.py", &format!("k = '{}'\n", fake_aws_key()));
        let root = dir.to_string_lossy().to_string();
        let (code, out, _) = run_args(&["scan", &root, "--format", "github"]);
        assert_eq!(code, 1);
        assert!(out.starts_with("::error file="));
        assert!(out.contains("title=aws-access-key-id"));
    }
}

}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdout = io::stdout();
    let stderr = io::stderr();
    let mut out = stdout.lock();
    let mut err = stderr.lock();
    let code = cli::run(&args, &mut out, &mut err);
    let _ = out.flush();
    let _ = err.flush();
    process::exit(code);
}
