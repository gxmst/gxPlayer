use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use url::Url;

use crate::require_window;

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const MAX_RECENT_ENTRIES: usize = 1000;
const DEFAULT_RECENT_ENTRIES: usize = 200;
const MAX_CATEGORY_CHARS: usize = 64;
const MAX_SOURCE_CHARS: usize = 160;
const MAX_SUMMARY_CHARS: usize = 2000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLogEntry {
    pub timestamp_ms: u64,
    pub category: String,
    pub source: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLogStatus {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLogExportResult {
    pub path: PathBuf,
    pub entry_count: usize,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedDiagnosticSettings {
    enabled: bool,
}

impl Default for PersistedDiagnosticSettings {
    fn default() -> Self {
        Self { enabled: true }
    }
}

struct DiagnosticLogInner {
    enabled: bool,
}

pub struct DiagnosticLogState {
    settings_path: PathBuf,
    log_path: PathBuf,
    rotated_path: PathBuf,
    inner: Mutex<DiagnosticLogInner>,
}

impl DiagnosticLogState {
    pub fn open(app_data: &Path) -> Self {
        let settings_path = app_data.join("diagnostic-log-settings.json");
        let log_path = app_data.join("diagnostic.log.jsonl");
        let rotated_path = app_data.join("diagnostic.log.1.jsonl");
        let enabled = load_settings(&settings_path).enabled;
        Self {
            settings_path,
            log_path,
            rotated_path,
            inner: Mutex::new(DiagnosticLogInner { enabled }),
        }
    }

    pub fn status(&self) -> DiagnosticLogStatus {
        DiagnosticLogStatus {
            enabled: self.inner.lock().unwrap().enabled,
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<DiagnosticLogStatus, String> {
        let mut inner = self.inner.lock().unwrap();
        persist_settings(&self.settings_path, enabled)?;
        inner.enabled = enabled;
        Ok(DiagnosticLogStatus { enabled })
    }

    pub fn record(
        &self,
        category: &str,
        source: Option<&str>,
        summary: &str,
    ) -> Result<bool, String> {
        let inner = self.inner.lock().unwrap();
        if !inner.enabled {
            return Ok(false);
        }
        let entry = DiagnosticLogEntry {
            timestamp_ms: unix_time_ms(),
            category: sanitize_field(category, MAX_CATEGORY_CHARS),
            source: source.map(|value| sanitize_field(value, MAX_SOURCE_CHARS)),
            summary: sanitize_field(summary, MAX_SUMMARY_CHARS),
        };
        append_entry(&self.log_path, &self.rotated_path, &entry)?;
        Ok(true)
    }

    pub fn recent(&self, limit: Option<usize>) -> Result<Vec<DiagnosticLogEntry>, String> {
        let _inner = self.inner.lock().unwrap();
        let limit = limit
            .unwrap_or(DEFAULT_RECENT_ENTRIES)
            .min(MAX_RECENT_ENTRIES);
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut entries = self.all_entries()?;
        let retain_from = entries.len().saturating_sub(limit);
        entries.drain(..retain_from);
        Ok(entries)
    }

    pub fn clear(&self) -> Result<(), String> {
        let _inner = self.inner.lock().unwrap();
        remove_if_exists(&self.log_path)?;
        remove_if_exists(&self.rotated_path)?;
        Ok(())
    }

    pub fn export(&self, path: &Path) -> Result<DiagnosticLogExportResult, String> {
        let _inner = self.inner.lock().unwrap();
        if path == self.log_path || path == self.rotated_path || path == self.settings_path {
            return Err("diagnostic log cannot be exported over an application data file".into());
        }
        let entries = self.all_entries()?;
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut output = File::create(path).map_err(|error| error.to_string())?;
        for entry in &entries {
            serde_json::to_writer(&mut output, entry).map_err(|error| error.to_string())?;
            output.write_all(b"\n").map_err(|error| error.to_string())?;
        }
        output.flush().map_err(|error| error.to_string())?;
        Ok(DiagnosticLogExportResult {
            path: path.to_path_buf(),
            entry_count: entries.len(),
        })
    }

    fn all_entries(&self) -> Result<Vec<DiagnosticLogEntry>, String> {
        let mut entries = read_entries(&self.rotated_path)?;
        entries.extend(read_entries(&self.log_path)?);
        Ok(entries)
    }
}

/// Records one bounded, redacted diagnostic event without making callers depend on state layout.
/// Logging is best-effort so a diagnostic failure never replaces the application's real error.
pub(crate) fn record_diagnostic(
    app: &AppHandle,
    category: &str,
    source: Option<&str>,
    summary: impl AsRef<str>,
) {
    let Some(state) = app.try_state::<DiagnosticLogState>() else {
        return;
    };
    if let Err(error) = state.record(category, source, summary.as_ref()) {
        eprintln!("failed to write diagnostic log: {error}");
    }
}

#[tauri::command]
pub fn diagnostic_log_status(
    window: WebviewWindow,
    state: State<'_, DiagnosticLogState>,
) -> Result<DiagnosticLogStatus, String> {
    require_window(&window, "main")?;
    Ok(state.status())
}

#[tauri::command]
pub fn diagnostic_log_set_enabled(
    window: WebviewWindow,
    state: State<'_, DiagnosticLogState>,
    enabled: bool,
) -> Result<DiagnosticLogStatus, String> {
    require_window(&window, "main")?;
    state.set_enabled(enabled)
}

#[tauri::command]
pub fn diagnostic_log_recent(
    window: WebviewWindow,
    state: State<'_, DiagnosticLogState>,
    limit: Option<usize>,
) -> Result<Vec<DiagnosticLogEntry>, String> {
    require_window(&window, "main")?;
    state.recent(limit)
}

#[tauri::command]
pub fn diagnostic_log_clear(
    window: WebviewWindow,
    state: State<'_, DiagnosticLogState>,
) -> Result<(), String> {
    require_window(&window, "main")?;
    state.clear()
}

#[tauri::command]
pub fn diagnostic_log_export(
    window: WebviewWindow,
    state: State<'_, DiagnosticLogState>,
    path: String,
) -> Result<DiagnosticLogExportResult, String> {
    require_window(&window, "main")?;
    let path = path.trim();
    if path.is_empty() {
        return Err("diagnostic log export path is empty".into());
    }
    state.export(Path::new(path))
}

fn load_settings(path: &Path) -> PersistedDiagnosticSettings {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .or_else(|| {
            fs::read(settings_backup_path(path))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        })
        .unwrap_or_default()
}

fn persist_settings(path: &Path, enabled: bool) -> Result<(), String> {
    if path.is_dir() {
        return Err("diagnostic log settings path is a directory".into());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(&PersistedDiagnosticSettings { enabled })
        .map_err(|error| error.to_string())?;
    let temporary = path.with_extension("json.tmp");
    let backup = settings_backup_path(path);
    let mut file = File::create(&temporary).map_err(|error| error.to_string())?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.flush().map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())?;
    drop(file);
    match fs::rename(&temporary, path) {
        Ok(()) => {
            let _ = remove_if_exists(&backup);
            Ok(())
        }
        Err(_) if path.exists() => {
            remove_if_exists(&backup)?;
            fs::rename(path, &backup).map_err(|error| error.to_string())?;
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::rename(&backup, path);
                return Err(error.to_string());
            }
            remove_if_exists(&backup)?;
            Ok(())
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error.to_string())
        }
    }
}

fn settings_backup_path(path: &Path) -> PathBuf {
    path.with_extension("json.bak")
}

fn append_entry(
    log_path: &Path,
    rotated_path: &Path,
    entry: &DiagnosticLogEntry,
) -> Result<(), String> {
    let mut encoded = serde_json::to_vec(entry).map_err(|error| error.to_string())?;
    encoded.push(b'\n');
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let current_size = fs::metadata(log_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if current_size > 0 && current_size.saturating_add(encoded.len() as u64) > MAX_LOG_BYTES {
        remove_if_exists(rotated_path)?;
        fs::rename(log_path, rotated_path).map_err(|error| error.to_string())?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut file| file.write_all(&encoded))
        .map_err(|error| error.to_string())
}

fn read_entries(path: &Path) -> Result<Vec<DiagnosticLogEntry>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut entries = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|error| error.to_string())?;
        if let Ok(entry) = serde_json::from_str(&line) {
            entries.push(entry);
        }
    }
    Ok(entries)
}

fn remove_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn sanitize_field(value: &str, max_chars: usize) -> String {
    let flattened = value.split_whitespace().collect::<Vec<_>>().join(" ");
    // Redact labelled credentials before URL parsing so quoted query values cannot escape when a
    // malformed or unusually formatted URL is replaced below.
    let without_auth = auth_value_regex().replace_all(&flattened, "${prefix}<redacted>");
    let without_secrets = secret_value_regex().replace_all(&without_auth, "${prefix}<redacted>");
    let without_urls = url_regex().replace_all(&without_secrets, |captures: &Captures<'_>| {
        redact_url(captures.get(0).map_or("", |matched| matched.as_str()))
    });
    let without_paths = quoted_path_regex().replace_all(&without_urls, "<path>");
    let without_paths = unc_path_regex().replace_all(&without_paths, "<path>${suffix}");
    let without_paths = windows_path_regex().replace_all(&without_paths, "<path>${suffix}");
    let redacted = unix_path_regex().replace_all(&without_paths, "${prefix}<path>${suffix}");
    redacted.chars().take(max_chars).collect()
}

fn redact_url(value: &str) -> String {
    let Ok(url) = Url::parse(value) else {
        return "<url>".to_owned();
    };
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return "<url>".to_owned();
    }
    url.origin().ascii_serialization()
}

fn url_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    // Quotes and angle brackets may legitimately appear in malformed, unescaped query values or
    // in the redaction placeholder inserted above. Capture through the next whitespace boundary;
    // `redact_url` returns `<url>` instead of raw text whenever the full candidate cannot parse.
    REGEX.get_or_init(|| Regex::new(r#"(?i)https?://\S+"#).unwrap())
}

fn quoted_path_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r#"(?i)[\"'](?:[a-z]:[\\/]|\\\\|/)[^\"']+[\"']"#).unwrap())
}

fn windows_path_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)\b[a-z]:[\\/].*?(?P<suffix>:\s+(?:error|failed|failure|access|permission|not found|the system|os error)\b|\s+\(os error\b|$)"#,
        )
        .unwrap()
    })
}

fn unc_path_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)\\\\.*?(?P<suffix>:\s+(?:error|failed|failure|access|permission|not found|the system|os error)\b|\s+\(os error\b|$)"#,
        )
        .unwrap()
    })
}

fn unix_path_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<prefix>^|[\s(=:'\"])/[^/\s].*?(?P<suffix>:\s+(?:error|failed|failure|access|permission|not found|the system|os error)\b|\s+\(os error\b|$)"#,
        )
        .unwrap()
    })
}

fn auth_value_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r#"(?i)(?P<prefix>[\"']?(?:authorization|auth|cookie)[\"']?\s*[:=]\s*)[^\r\n]+"#)
            .unwrap()
    })
}

fn secret_value_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)(?P<prefix>[\"']?(?:access[_-]?token|refresh[_-]?token|token|api[_-]?key|secret|password|passwd|pass|key)[\"']?\s*[:=]\s*)(?:\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|[^\s,;}\]]+)"#,
        )
        .unwrap()
    })
}

fn unix_time_ms() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "gxplayer-diagnostic-log-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn defaults_enabled_and_persists_manual_override() {
        let root = test_root("settings");
        let state = DiagnosticLogState::open(&root);
        assert!(state.status().enabled);
        assert!(!state.set_enabled(false).unwrap().enabled);
        assert!(!state.record("source", None, "not written").unwrap());
        assert!(!state.log_path.exists());
        assert!(!state.settings_path.with_extension("json.tmp").exists());
        drop(state);

        let settings_path = root.join("diagnostic-log-settings.json");
        fs::copy(&settings_path, settings_backup_path(&settings_path)).unwrap();
        fs::write(&settings_path, b"not json").unwrap();
        let reopened = DiagnosticLogState::open(&root);
        assert!(!reopened.status().enabled);
        reopened.set_enabled(true).unwrap();
        drop(reopened);
        assert!(DiagnosticLogState::open(&root).status().enabled);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_redacts_urls_credentials_and_sensitive_values() {
        let root = test_root("redaction");
        let state = DiagnosticLogState::open(&root);
        state
            .record(
                "source\nrequest",
                Some("https://user:password@example.com/source?key=source-secret#fragment"),
                "failed https://user:pw@example.com/play?token=url-secret#part\n\
                 C:\\Users\\private-user\\AppData\\cache \\\\server\\private-share\\file\n\
                 /home/private-user/.cache/file '/Users/private-user/Music/file.mp3'\n\
                 token=plain-secret password: \"json-secret\" Authorization: Bearer auth-secret",
            )
            .unwrap();

        let entry = state.recent(None).unwrap().pop().unwrap();
        let serialized = fs::read_to_string(&state.log_path).unwrap();
        assert_eq!(entry.category, "source request");
        assert_eq!(entry.source.as_deref(), Some("https://example.com"));
        assert!(entry.summary.contains("https://example.com"));
        for secret in [
            "password@example",
            "source-secret",
            "url-secret",
            "plain-secret",
            "json-secret",
            "auth-secret",
            "private-user",
            "private-share",
        ] {
            assert!(!serialized.contains(secret));
        }
        assert!(!entry.summary.contains('\n'));
        assert!(entry.summary.contains("<path>"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writer_redacts_quoted_query_secrets_url_paths_and_paths_with_spaces() {
        let root = test_root("redaction-edge-cases");
        let state = DiagnosticLogState::open(&root);
        state
            .record(
                "redaction",
                None,
                "token='QUERY-SECRET-A' password=\"QUERY-SECRET-B\" \
                 https://user:URL-SECRET@example.com/URL-PATH-SECRET/file?token='URL-QUERY-SECRET' \
                 https://[MALFORMED-URL-SECRET \
                 C:\\Users\\John Doe\\Private Cache\\credential.bin: Access is denied \
                 \\\\private-server\\Private Share\\credential.bin: error opening file \
                 /home/John Doe/Private Music/credential.bin: permission denied",
            )
            .unwrap();

        let entry = state.recent(None).unwrap().pop().unwrap();
        let serialized = fs::read_to_string(&state.log_path).unwrap();
        assert!(entry.summary.contains("token=<redacted>"));
        assert!(entry.summary.contains("password=<redacted>"));
        assert!(entry.summary.contains("https://example.com"));
        assert!(entry.summary.contains("<url>"));
        assert!(entry.summary.contains("<path>: Access is denied"));
        assert!(entry.summary.contains("<path>: error opening file"));
        assert!(entry.summary.contains("<path>: permission denied"));
        for sensitive in [
            "QUERY-SECRET-A",
            "QUERY-SECRET-B",
            "URL-SECRET",
            "URL-PATH-SECRET",
            "URL-QUERY-SECRET",
            "MALFORMED-URL-SECRET",
            "John Doe",
            "Private Cache",
            "private-server",
            "Private Share",
            "Private Music",
            "credential.bin",
        ] {
            assert!(!serialized.contains(sensitive), "leaked {sensitive}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotates_at_one_mebibyte_and_keeps_one_previous_file() {
        let root = test_root("rotation");
        let state = DiagnosticLogState::open(&root);
        let payload = "x".repeat(MAX_SUMMARY_CHARS);
        for index in 0..700 {
            state
                .record("rotation", Some("test"), &format!("{index:04} {payload}"))
                .unwrap();
        }
        assert!(state.log_path.exists());
        assert!(state.rotated_path.exists());
        assert!(fs::metadata(&state.log_path).unwrap().len() <= MAX_LOG_BYTES);
        assert!(fs::metadata(&state.rotated_path).unwrap().len() <= MAX_LOG_BYTES);
        assert!(
            fs::metadata(&state.log_path).unwrap().len()
                + fs::metadata(&state.rotated_path).unwrap().len()
                <= MAX_LOG_BYTES * 2
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recent_is_chronological_and_capped_at_one_thousand() {
        let root = test_root("recent");
        let state = DiagnosticLogState::open(&root);
        for index in 0..1105 {
            state
                .record("recent", None, &format!("event-{index:04}"))
                .unwrap();
        }
        let recent = state.recent(Some(5000)).unwrap();
        assert_eq!(recent.len(), MAX_RECENT_ENTRIES);
        assert_eq!(recent.first().unwrap().summary, "event-0105");
        assert_eq!(recent.last().unwrap().summary, "event-1104");
        assert!(state.recent(Some(0)).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exports_combined_jsonl_and_clear_removes_both_files() {
        let root = test_root("export-clear");
        let state = DiagnosticLogState::open(&root);
        state.record("first", None, "one").unwrap();
        fs::rename(&state.log_path, &state.rotated_path).unwrap();
        state.record("second", Some("worker"), "two").unwrap();
        let export_path = root.join("exports").join("diagnostics.jsonl");

        let result = state.export(&export_path).unwrap();
        assert_eq!(result.entry_count, 2);
        assert_eq!(read_entries(&export_path).unwrap().len(), 2);
        state.clear().unwrap();
        assert!(!state.log_path.exists());
        assert!(!state.rotated_path.exists());
        assert!(state.recent(None).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}
