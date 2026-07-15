use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use gx_library::{LibraryStore, LibraryTrack};
use serde::{Deserialize, Serialize};
use tauri::{Manager, WebviewWindow};

use crate::{
    LibraryImportFailure, import_local_files, local_path_key, new_library_track, require_window,
};

const MAX_FOLDER_ROOTS: usize = 128;
const MAX_FOLDER_IMPORT_FILES: usize = 10_000;
const MAX_SCANNED_ENTRIES: usize = 250_000;
const MAX_REMOVE_TRACKS: usize = 10_000;
const MAX_RELINK_TRACKS: usize = 1_000;
const MAX_LOCAL_PATH_BYTES: usize = 32 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryFolderImportResult {
    imported: Vec<LibraryTrack>,
    failures: Vec<LibraryImportFailure>,
    scanned_file_count: usize,
    skipped_file_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryRemoveResult {
    removed_track_ids: Vec<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryRelinkRequest {
    old_path: String,
    new_path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LibraryRelinkFailure {
    old_path: String,
    new_path: String,
    error: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibraryRelinkResult {
    relinked: Vec<LibraryTrack>,
    failures: Vec<LibraryRelinkFailure>,
}

#[derive(Debug)]
struct FolderDiscovery {
    paths: Vec<String>,
    failures: Vec<LibraryImportFailure>,
    scanned_file_count: usize,
    skipped_file_count: usize,
}

#[tauri::command]
pub(crate) async fn library_import_folders(
    window: WebviewWindow,
    folders: Vec<String>,
) -> Result<LibraryFolderImportResult, String> {
    require_window(&window, "main")?;
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let library = app.state::<LibraryStore>();
        import_local_folders(&library, folders)
    })
    .await
    .map_err(|error| format!("本地音乐文件夹导入任务失败: {error}"))?
}

#[tauri::command]
pub(crate) fn library_remove_tracks(
    window: WebviewWindow,
    library: tauri::State<'_, LibraryStore>,
    track_ids: Vec<i64>,
) -> Result<LibraryRemoveResult, String> {
    require_window(&window, "main")?;
    if track_ids.is_empty() {
        return Err("至少选择一首要移出曲库的歌曲".into());
    }
    if track_ids.len() > MAX_REMOVE_TRACKS {
        return Err(format!("单次最多移出 {MAX_REMOVE_TRACKS} 首歌曲"));
    }
    let removed_track_ids = library
        .remove_tracks(&track_ids)
        .map_err(|error| error.to_string())?;
    Ok(LibraryRemoveResult { removed_track_ids })
}

#[tauri::command]
pub(crate) async fn library_relink_tracks(
    window: WebviewWindow,
    relinks: Vec<LibraryRelinkRequest>,
) -> Result<LibraryRelinkResult, String> {
    require_window(&window, "main")?;
    if relinks.is_empty() {
        return Err("至少提供一首歌曲的原路径和新路径".into());
    }
    if relinks.len() > MAX_RELINK_TRACKS {
        return Err(format!("单次最多重新定位 {MAX_RELINK_TRACKS} 首歌曲"));
    }
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        relink_local_tracks(&app.state::<LibraryStore>(), relinks)
    })
    .await
    .map_err(|error| format!("本地音乐批量重新定位任务失败: {error}"))
}

fn import_local_folders(
    library: &LibraryStore,
    folders: Vec<String>,
) -> Result<LibraryFolderImportResult, String> {
    let discovery = discover_audio_files(folders)?;
    let imported = import_local_files(library, discovery.paths)?;
    let mut failures = discovery.failures;
    failures.extend(imported.failures);
    Ok(LibraryFolderImportResult {
        imported: imported.imported,
        failures,
        scanned_file_count: discovery.scanned_file_count,
        skipped_file_count: discovery.skipped_file_count,
    })
}

fn discover_audio_files(folders: Vec<String>) -> Result<FolderDiscovery, String> {
    if folders.is_empty() {
        return Err("至少选择一个本地音乐文件夹".into());
    }
    if folders.len() > MAX_FOLDER_ROOTS {
        return Err(format!("单次最多选择 {MAX_FOLDER_ROOTS} 个文件夹"));
    }

    let mut failures = Vec::new();
    let mut pending = VecDeque::new();
    let mut queued_roots = HashSet::new();
    for raw_folder in folders {
        if raw_folder.trim().is_empty() {
            failures.push(LibraryImportFailure {
                path: raw_folder,
                error: "文件夹路径为空".into(),
            });
            continue;
        }
        if raw_folder.len() > MAX_LOCAL_PATH_BYTES {
            failures.push(LibraryImportFailure {
                path: raw_folder,
                error: "文件夹路径过长".into(),
            });
            continue;
        }
        let folder = PathBuf::from(&raw_folder);
        match fs::metadata(&folder) {
            Ok(metadata) if metadata.is_dir() => {
                let key_path = fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone());
                if queued_roots.insert(path_key(&key_path)) {
                    pending.push_back(folder);
                }
            }
            Ok(_) => failures.push(LibraryImportFailure {
                path: raw_folder,
                error: "所选路径不是文件夹".into(),
            }),
            Err(error) => failures.push(LibraryImportFailure {
                path: raw_folder,
                error: error.to_string(),
            }),
        }
    }

    let mut paths = Vec::new();
    let mut seen_paths = HashSet::new();
    let mut visited_directories = HashSet::new();
    let mut scanned_entries = 0usize;
    let mut scanned_file_count = 0usize;
    let mut skipped_file_count = 0usize;

    while let Some(folder) = pending.pop_front() {
        let canonical = fs::canonicalize(&folder).unwrap_or_else(|_| folder.clone());
        if !visited_directories.insert(path_key(&canonical)) {
            continue;
        }

        let read_dir = match fs::read_dir(&folder) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                failures.push(LibraryImportFailure {
                    path: folder.display().to_string(),
                    error: error.to_string(),
                });
                continue;
            }
        };
        let mut entries = Vec::new();
        for entry in read_dir {
            scanned_entries = scanned_entries.saturating_add(1);
            if scanned_entries > MAX_SCANNED_ENTRIES {
                return Err(format!(
                    "文件夹内容超过 {MAX_SCANNED_ENTRIES} 项，请缩小范围后分批导入"
                ));
            }
            match entry {
                Ok(entry) => entries.push(entry),
                Err(error) => failures.push(LibraryImportFailure {
                    path: folder.display().to_string(),
                    error: error.to_string(),
                }),
            }
        }
        entries.sort_by_key(|entry| entry.path());

        for entry in entries {
            let entry_path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    failures.push(LibraryImportFailure {
                        path: entry_path.display().to_string(),
                        error: error.to_string(),
                    });
                    continue;
                }
            };
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                pending.push_back(entry_path);
                continue;
            }
            if !file_type.is_file() {
                continue;
            }

            scanned_file_count = scanned_file_count.saturating_add(1);
            if !is_supported_audio_path(&entry_path) {
                skipped_file_count = skipped_file_count.saturating_add(1);
                continue;
            }
            let Some(path) = entry_path.to_str() else {
                failures.push(LibraryImportFailure {
                    path: entry_path.to_string_lossy().into_owned(),
                    error: "文件路径不是有效的 Unicode 文本".into(),
                });
                continue;
            };
            if seen_paths.insert(local_path_key(path)) {
                paths.push(path.to_owned());
                if paths.len() > MAX_FOLDER_IMPORT_FILES {
                    return Err(format!(
                        "单次最多导入 {MAX_FOLDER_IMPORT_FILES} 个音频文件，请缩小范围后分批导入"
                    ));
                }
            }
        }
    }

    Ok(FolderDiscovery {
        paths,
        failures,
        scanned_file_count,
        skipped_file_count,
    })
}

fn relink_local_tracks(
    library: &LibraryStore,
    relinks: Vec<LibraryRelinkRequest>,
) -> LibraryRelinkResult {
    let mut relinked = Vec::new();
    let mut failures = Vec::new();
    let mut seen_old_paths = HashSet::new();

    for request in relinks {
        let LibraryRelinkRequest { old_path, new_path } = request;
        let validation_error = if old_path.trim().is_empty() || new_path.trim().is_empty() {
            Some("原路径和新路径不能为空".to_owned())
        } else if old_path.len() > MAX_LOCAL_PATH_BYTES || new_path.len() > MAX_LOCAL_PATH_BYTES {
            Some("本地音频路径过长".to_owned())
        } else if !seen_old_paths.insert(local_path_key(&old_path)) {
            Some("批次中包含重复的原路径".to_owned())
        } else {
            None
        };
        if let Some(error) = validation_error {
            failures.push(LibraryRelinkFailure {
                old_path,
                new_path,
                error,
            });
            continue;
        }

        let path = PathBuf::from(&new_path);
        let info = match gx_audio::probe_local_file(&path) {
            Ok(info) => info,
            Err(error) => {
                failures.push(LibraryRelinkFailure {
                    old_path,
                    new_path,
                    error: error.to_string(),
                });
                continue;
            }
        };
        let replacement = new_library_track(&path, info);
        match library.relink_track(&old_path, &replacement) {
            Ok(track) => relinked.push(track),
            Err(error) => failures.push(LibraryRelinkFailure {
                old_path,
                new_path,
                error: error.to_string(),
            }),
        }
    }

    LibraryRelinkResult { relinked, failures }
}

fn path_key(path: &Path) -> String {
    local_path_key(&path.to_string_lossy())
}

fn is_supported_audio_path(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "mp3" | "flac" | "wav" | "m4a" | "aac" | "ogg" | "oga"
    )
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    use gx_library::NewTrack;

    use super::*;

    struct TemporaryDirectory(PathBuf);

    impl TemporaryDirectory {
        fn new(label: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "gxplayer-library-commands-{label}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TemporaryDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn folder_discovery_is_recursive_deduplicated_and_extension_filtered() {
        let root = TemporaryDirectory::new("discover");
        let nested = root.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.path().join("first.MP3"), b"not probed here").unwrap();
        fs::write(nested.join("second.flac"), b"not probed here").unwrap();
        fs::write(nested.join("notes.txt"), b"ignore").unwrap();
        let missing = root.path().join("missing");

        let discovery = discover_audio_files(vec![
            root.path().display().to_string(),
            nested.display().to_string(),
            missing.display().to_string(),
        ])
        .unwrap();

        assert_eq!(discovery.paths.len(), 2);
        assert_eq!(discovery.scanned_file_count, 3);
        assert_eq!(discovery.skipped_file_count, 1);
        assert_eq!(discovery.failures.len(), 1);
        assert!(
            discovery
                .paths
                .iter()
                .any(|path| path.ends_with("first.MP3"))
        );
        assert!(
            discovery
                .paths
                .iter()
                .any(|path| path.ends_with("second.flac"))
        );
    }

    #[test]
    fn folder_import_uses_media_probe_and_reports_bad_candidates() {
        let root = TemporaryDirectory::new("import");
        let good = root.path().join("Artist - Good.wav");
        write_test_wav(&good);
        fs::write(root.path().join("broken.mp3"), b"not audio").unwrap();
        fs::write(root.path().join("cover.jpg"), b"not audio").unwrap();
        let store = LibraryStore::open(":memory:").unwrap();

        let result = import_local_folders(&store, vec![root.path().display().to_string()]).unwrap();

        assert_eq!(result.imported.len(), 1);
        assert_eq!(result.imported[0].title, "Good");
        assert_eq!(result.imported[0].artist, "Artist");
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].path.ends_with("broken.mp3"));
        assert_eq!(result.scanned_file_count, 3);
        assert_eq!(result.skipped_file_count, 1);
    }

    #[test]
    fn batch_relink_keeps_successes_and_reports_individual_failures() {
        let root = TemporaryDirectory::new("relink");
        let replacement = root.path().join("replacement.wav");
        write_test_wav(&replacement);
        let invalid = root.path().join("invalid.wav");
        fs::write(&invalid, b"not audio").unwrap();
        let store = LibraryStore::open(":memory:").unwrap();
        store
            .upsert_tracks(&[NewTrack {
                path: "D:/Missing/old.wav".into(),
                title: "Original".into(),
                artist: String::new(),
                album: String::new(),
                duration_seconds: None,
            }])
            .unwrap();

        let result = relink_local_tracks(
            &store,
            vec![
                LibraryRelinkRequest {
                    old_path: "D:/Missing/old.wav".into(),
                    new_path: replacement.display().to_string(),
                },
                LibraryRelinkRequest {
                    old_path: "D:/Missing/bad.wav".into(),
                    new_path: invalid.display().to_string(),
                },
            ],
        );

        assert_eq!(result.relinked.len(), 1);
        assert_eq!(result.relinked[0].title, "Original");
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].old_path, "D:/Missing/bad.wav");
    }

    fn write_test_wav(path: &Path) {
        let sample_rate = 8_000u32;
        let channels = 1u16;
        let bits_per_sample = 16u16;
        let frames = sample_rate / 10;
        let block_align = channels * (bits_per_sample / 8);
        let byte_rate = sample_rate * u32::from(block_align);
        let data_size = frames * u32::from(block_align);
        let mut file = File::create(path).unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + data_size).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16u32.to_le_bytes()).unwrap();
        file.write_all(&1u16.to_le_bytes()).unwrap();
        file.write_all(&channels.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&byte_rate.to_le_bytes()).unwrap();
        file.write_all(&block_align.to_le_bytes()).unwrap();
        file.write_all(&bits_per_sample.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&data_size.to_le_bytes()).unwrap();
        file.write_all(&vec![0; data_size as usize]).unwrap();
    }
}
