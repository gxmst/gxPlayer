use gx_library::{LibraryBackup, LibraryStore};
use gx_source::{SourceBackup, SourceStore};
use serde::{Deserialize, Serialize};
use tauri::{Manager, WebviewWindow};

use crate::source_commands::reload_runtime;
use crate::source_runtime::SourceRuntime;
use crate::{SANDBOX_LABEL, require_window};

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationBackup {
    version: u32,
    library: LibraryBackup,
    sources: SourceBackup,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupRestorePreview {
    track_count: usize,
    playlist_count: usize,
    source_count: usize,
}

fn validate_backup(backup: &ApplicationBackup) -> Result<BackupRestorePreview, String> {
    if !matches!(backup.version, 1 | 2) {
        return Err(format!("不支持的 GXPlayer 备份版本 {}", backup.version));
    }
    if backup.library.version != backup.version {
        return Err(format!(
            "GXPlayer 备份版本 {} 与曲库备份版本 {} 不匹配",
            backup.version, backup.library.version
        ));
    }
    LibraryStore::validate_backup(&backup.library)
        .map_err(|error| format!("曲库备份校验失败：{error}"))?;
    SourceStore::validate_backup(&backup.sources)
        .map_err(|error| format!("音源备份校验失败：{error}"))?;
    Ok(BackupRestorePreview {
        track_count: backup.library.tracks.len(),
        playlist_count: backup.library.playlists.len(),
        source_count: backup.sources.sources.len(),
    })
}

fn restore_backup_atomically(
    library: &LibraryStore,
    runtime: &SourceRuntime,
    backup: ApplicationBackup,
    mut reload: impl FnMut() -> Result<(), String>,
) -> Result<BackupRestorePreview, String> {
    // Validate both halves before taking snapshots or changing either store. The
    // command validates again even when the UI already requested a preview.
    let preview = validate_backup(&backup)?;
    runtime.serialized(|| {
        let rollback_library = library
            .export_backup()
            .map_err(|error| format!("无法创建曲库回滚快照：{error}"))?;
        let rollback_sources = runtime
            .export_backup()
            .map_err(|error| format!("无法创建音源回滚快照：{error}"))?;

        let restore_result = (|| {
            library
                .restore_backup(&backup.library)
                .map_err(|error| format!("恢复曲库失败：{error}"))?;
            runtime
                .restore_backup(backup.sources)
                .map_err(|error| format!("恢复音源失败：{error}"))?;
            reload().map_err(|error| format!("重载音源运行时失败：{error}"))?;
            Ok::<(), String>(())
        })();

        let Err(restore_error) = restore_result else {
            return Ok(preview);
        };

        // A source restore can fail after writing one or more scripts, so both
        // snapshots are always reapplied. Continue every rollback step even if
        // an earlier one fails, then surface all failures to the user.
        let mut rollback_errors = Vec::new();
        if let Err(error) = runtime.restore_backup(rollback_sources) {
            rollback_errors.push(format!("音源回滚失败：{error}"));
        }
        if let Err(error) = library.restore_backup(&rollback_library) {
            rollback_errors.push(format!("曲库回滚失败：{error}"));
        }
        if let Err(error) = reload() {
            rollback_errors.push(format!("旧音源运行时恢复失败：{error}"));
        }

        if rollback_errors.is_empty() {
            Err(format!("{restore_error}；已回滚到恢复前状态"))
        } else {
            Err(format!(
                "{restore_error}；回滚未完全成功：{}",
                rollback_errors.join("；")
            ))
        }
    })
}

#[tauri::command]
pub fn backup_preview_restore(
    window: WebviewWindow,
    backup: ApplicationBackup,
) -> Result<BackupRestorePreview, String> {
    require_window(&window, "main")?;
    validate_backup(&backup)
}

#[tauri::command]
pub fn backup_restore_atomic(
    window: WebviewWindow,
    library: tauri::State<LibraryStore>,
    runtime: tauri::State<SourceRuntime>,
    backup: ApplicationBackup,
) -> Result<BackupRestorePreview, String> {
    require_window(&window, "main")?;
    let sandbox = window.app_handle().get_webview_window(SANDBOX_LABEL);
    restore_backup_atomically(&library, &runtime, backup, || {
        reload_runtime(&sandbox, &runtime)
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use gx_library::{LibraryTrack, NewTrack, PlaylistBackup, PlaylistBackupItem};
    use gx_source::BackupSource;
    use serde_json::Value;

    use super::*;

    fn temporary_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("gx-backup-command-test-{nanos}"))
    }

    fn library_backup(path: &str, title: &str) -> LibraryBackup {
        LibraryBackup {
            version: 1,
            tracks: vec![LibraryTrack {
                id: 1,
                path: path.into(),
                title: title.into(),
                artist: "Tester".into(),
                album: String::new(),
                duration_seconds: Some(60.0),
                favorite: false,
                added_at_ms: 1,
                missing: false,
            }],
            playlists: vec![PlaylistBackup {
                name: "列表".into(),
                track_paths: vec![path.into()],
                items: Vec::new(),
            }],
        }
    }

    fn source_backup(script: &str, name: &str) -> SourceBackup {
        SourceBackup {
            version: 1,
            active_source_id: None,
            fallback_enabled: true,
            fallback_source_ids: None,
            source_order: None,
            sources: vec![BackupSource {
                origin: "test".into(),
                fallback_name: name.into(),
                enabled: true,
                updates_enabled: true,
                script: script.into(),
                config: Value::Object(Default::default()),
            }],
        }
    }

    fn version_two_library_backup(path: &str, title: &str) -> LibraryBackup {
        let mut backup = library_backup(path, title);
        backup.version = 2;
        backup.playlists[0].track_paths.clear();
        backup.playlists[0].items = vec![PlaylistBackupItem::Local {
            track_path: path.into(),
        }];
        backup
    }

    fn stores() -> (LibraryStore, SourceRuntime, std::path::PathBuf) {
        let root = temporary_root();
        let library = LibraryStore::open(root.join("library.db")).unwrap();
        library
            .upsert_tracks(&[NewTrack {
                path: "C:/Music/original.flac".into(),
                title: "Original".into(),
                artist: "Tester".into(),
                album: String::new(),
                duration_seconds: Some(90.0),
            }])
            .unwrap();
        let mut source_store = SourceStore::open(root.join("sources")).unwrap();
        source_store
            .import_script("lx.on('request', () => 'original')", "test", "Original")
            .unwrap();
        (library, SourceRuntime::new(source_store), root)
    }

    #[test]
    fn preview_rejects_bad_source_before_the_library_changes() {
        let (library, runtime, root) = stores();
        let before = library.export_backup().unwrap();
        let backup = ApplicationBackup {
            version: 1,
            library: library_backup("C:/Music/replacement.flac", "Replacement"),
            sources: source_backup("console.log('not an LX source')", "Broken"),
        };

        let error = restore_backup_atomically(&library, &runtime, backup, || {
            panic!("runtime reload must not run for an invalid backup")
        })
        .unwrap_err();

        assert!(error.contains("音源备份校验失败"));
        assert_eq!(library.export_backup().unwrap(), before);
        drop(runtime);
        drop(library);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn accepts_matching_legacy_and_current_envelopes_but_rejects_mixed_versions() {
        let legacy = ApplicationBackup {
            version: 1,
            library: library_backup("C:/Music/legacy.flac", "Legacy"),
            sources: source_backup("lx.on('request', () => 'legacy')", "Legacy"),
        };
        assert!(validate_backup(&legacy).is_ok());

        let current = ApplicationBackup {
            version: 2,
            library: version_two_library_backup("C:/Music/current.flac", "Current"),
            sources: source_backup("lx.on('request', () => 'current')", "Current"),
        };
        assert!(validate_backup(&current).is_ok());

        let mismatched = ApplicationBackup {
            version: 2,
            library: library_backup("C:/Music/mixed.flac", "Mixed"),
            sources: source_backup("lx.on('request', () => 'mixed')", "Mixed"),
        };
        assert!(
            validate_backup(&mismatched)
                .unwrap_err()
                .contains("版本 2 与曲库备份版本 1 不匹配")
        );
    }

    #[test]
    fn runtime_failure_restores_both_snapshots() {
        let (library, runtime, root) = stores();
        let before_library = library.export_backup().unwrap();
        let before_sources = runtime.export_backup().unwrap();
        let reload_count = AtomicUsize::new(0);
        let backup = ApplicationBackup {
            version: 1,
            library: library_backup("C:/Music/replacement.flac", "Replacement"),
            sources: source_backup("lx.on('request', () => 'replacement')", "Replacement"),
        };

        let error = restore_backup_atomically(&library, &runtime, backup, || {
            if reload_count.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("sandbox unavailable".into())
            } else {
                Ok(())
            }
        })
        .unwrap_err();

        assert!(error.contains("已回滚到恢复前状态"));
        assert_eq!(reload_count.load(Ordering::SeqCst), 2);
        assert_eq!(library.export_backup().unwrap(), before_library);
        assert_eq!(runtime.export_backup().unwrap(), before_sources);
        drop(runtime);
        drop(library);
        std::fs::remove_dir_all(root).unwrap();
    }
}
