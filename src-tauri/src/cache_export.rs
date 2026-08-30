//! Copy cached audio out to a user-chosen directory under readable names.
//!
//! The bytes are whatever the source served and are written through unchanged, so
//! any tags the file already carries are preserved. Nothing is re-encoded and no
//! tag is synthesised: a source that served an untagged file exports untagged.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

use gx_cache::{CacheExportPlan, CacheKey};

use crate::cache_commands::CacheExportOutcome;

/// Copy in chunks so a large FLAC never lands in memory whole.
const COPY_CHUNK_BYTES: usize = 256 * 1024;

/// Give up before the filesystem does when a name keeps colliding.
const MAX_NAME_ATTEMPTS: u32 = 999;

pub(crate) fn write_exports(
    directory: &Path,
    planned: Vec<(CacheKey, Option<CacheExportPlan>)>,
) -> Vec<CacheExportOutcome> {
    let mut outcomes = Vec::with_capacity(planned.len());
    // One check for the whole batch: if the directory is unusable, every entry
    // fails for the same reason and saying so once per track is still accurate.
    let directory_error = prepare_directory(directory).err();

    for (key, plan) in planned {
        let outcome = match (&directory_error, plan) {
            (Some(error), _) => failure(&key, error.clone()),
            (None, None) => failure(&key, "这首歌还没有完整缓存，播放一遍后再导出".to_owned()),
            (None, Some(plan)) => match copy_one(directory, &plan) {
                Ok(file_name) => CacheExportOutcome {
                    provider_id: key.provider_id,
                    provider_track_id: key.provider_track_id,
                    quality: key.quality,
                    file_name: Some(file_name),
                    error: None,
                },
                Err(error) => failure(&key, error),
            },
        };
        outcomes.push(outcome);
    }
    outcomes
}

fn failure(key: &CacheKey, error: String) -> CacheExportOutcome {
    CacheExportOutcome {
        provider_id: key.provider_id.clone(),
        provider_track_id: key.provider_track_id.clone(),
        quality: key.quality.clone(),
        file_name: None,
        error: Some(error),
    }
}

fn prepare_directory(directory: &Path) -> Result<(), String> {
    if directory.as_os_str().is_empty() {
        return Err("导出目录为空".into());
    }
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        // A symlink to a directory is fine to write into; a file is not.
        Ok(metadata) if metadata.file_type().is_symlink() => match fs::metadata(directory) {
            Ok(target) if target.is_dir() => Ok(()),
            Ok(_) => Err("导出目标不是文件夹".into()),
            Err(error) => Err(format!("无法读取导出目录: {error}")),
        },
        Ok(_) => Err("导出目标不是文件夹".into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(directory).map_err(|error| format!("无法创建导出目录: {error}"))
        }
        Err(error) => Err(format!("无法读取导出目录: {error}")),
    }
}

/// Write one payload, returning the basename that landed on disk.
///
/// The file is created with `create_new`, so an existing file is never truncated;
/// a collision picks the next ` (n)` suffix instead. A partial write is removed
/// rather than left behind looking like a complete export.
fn copy_one(directory: &Path, plan: &CacheExportPlan) -> Result<String, String> {
    let mut source = File::open(&plan.source_path)
        .map_err(|error| format!("无法读取缓存文件: {}", io_reason(&error)))?;

    let (mut file, file_name) = create_unique(directory, &plan.file_stem, &plan.extension)?;
    let destination = directory.join(&file_name);

    let mut buffer = vec![0u8; COPY_CHUNK_BYTES];
    let result = (|| -> io::Result<()> {
        loop {
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])?;
        }
        file.flush()?;
        file.sync_all()
    })();
    drop(file);

    match result {
        Ok(()) => Ok(file_name),
        Err(error) => {
            let _ = fs::remove_file(&destination);
            Err(format!("写入失败: {}", io_reason(&error)))
        }
    }
}

fn create_unique(directory: &Path, stem: &str, extension: &str) -> Result<(File, String), String> {
    for attempt in 0..=MAX_NAME_ATTEMPTS {
        let name = if attempt == 0 {
            format!("{stem}.{extension}")
        } else {
            format!("{stem} ({attempt}).{extension}")
        };
        let candidate = directory.join(&name);
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((file, name)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("无法创建文件: {}", io_reason(&error))),
        }
    }
    Err("同名文件过多，已跳过".into())
}

/// Keep the reason without echoing absolute paths back to the interface.
fn io_reason(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "没有写入权限".into(),
        io::ErrorKind::NotFound => "文件不存在".into(),
        io::ErrorKind::StorageFull => "磁盘空间不足".into(),
        _ => error.kind().to_string(),
    }
}

/// Absolute destination for one plan, used by tests to assert placement.
#[cfg(test)]
fn destination_for(directory: &Path, plan: &CacheExportPlan) -> PathBuf {
    directory.join(format!("{}.{}", plan.file_stem, plan.extension))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(label: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "gx-export-test-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn plan(source: &Path, stem: &str, extension: &str, bytes: u64) -> CacheExportPlan {
        CacheExportPlan {
            source_path: source.to_path_buf(),
            file_stem: stem.to_owned(),
            extension: extension.to_owned(),
            byte_len: bytes,
        }
    }

    fn cache_key(track: &str) -> CacheKey {
        CacheKey {
            provider_id: "kg".into(),
            provider_track_id: track.into(),
            quality: "320k".into(),
        }
    }

    #[test]
    fn copies_bytes_verbatim_under_a_readable_name() {
        let root = temp_dir("verbatim");
        let source = root.join("payload.media");
        // An ID3 header, to show tags ride along untouched.
        let bytes = b"ID3\x03\x00\x00\x00original audio".to_vec();
        fs::write(&source, &bytes).unwrap();
        let out = temp_dir("verbatim-out");

        let outcomes = write_exports(
            &out,
            vec![(
                cache_key("a"),
                Some(plan(&source, "周杰伦 - 花海", "mp3", bytes.len() as u64)),
            )],
        );

        assert_eq!(outcomes[0].file_name.as_deref(), Some("周杰伦 - 花海.mp3"));
        assert!(outcomes[0].error.is_none());
        assert_eq!(fs::read(out.join("周杰伦 - 花海.mp3")).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let root = temp_dir("collide");
        let source = root.join("payload.media");
        fs::write(&source, b"new bytes").unwrap();
        let out = temp_dir("collide-out");
        fs::write(out.join("Band - Song.mp3"), b"PRECIOUS").unwrap();

        let outcomes = write_exports(
            &out,
            vec![
                (cache_key("a"), Some(plan(&source, "Band - Song", "mp3", 9))),
                (cache_key("b"), Some(plan(&source, "Band - Song", "mp3", 9))),
            ],
        );

        // The original is intact and each export took the next free name.
        assert_eq!(fs::read(out.join("Band - Song.mp3")).unwrap(), b"PRECIOUS");
        assert_eq!(
            outcomes[0].file_name.as_deref(),
            Some("Band - Song (1).mp3")
        );
        assert_eq!(
            outcomes[1].file_name.as_deref(),
            Some("Band - Song (2).mp3")
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn an_uncached_entry_is_reported_without_touching_the_network() {
        let out = temp_dir("uncached");

        let outcomes = write_exports(&out, vec![(cache_key("missing"), None)]);

        assert!(outcomes[0].file_name.is_none());
        assert!(
            outcomes[0].error.as_deref().unwrap().contains("完整缓存"),
            "{:?}",
            outcomes[0].error
        );
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn one_failure_does_not_abandon_the_rest_of_the_batch() {
        let root = temp_dir("partial");
        let good = root.join("good.media");
        fs::write(&good, b"fine").unwrap();
        let missing = root.join("gone.media");
        let out = temp_dir("partial-out");

        let outcomes = write_exports(
            &out,
            vec![
                (cache_key("gone"), Some(plan(&missing, "Missing", "mp3", 4))),
                (cache_key("good"), Some(plan(&good, "Present", "mp3", 4))),
            ],
        );

        assert!(outcomes[0].error.is_some());
        assert_eq!(outcomes[1].file_name.as_deref(), Some("Present.mp3"));
        assert!(out.join("Present.mp3").is_file());
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(out).unwrap();
    }

    #[test]
    fn a_file_where_the_directory_should_be_fails_every_entry_clearly() {
        let root = temp_dir("notdir");
        let source = root.join("payload.media");
        fs::write(&source, b"bytes").unwrap();
        let blocker = root.join("not-a-directory");
        fs::write(&blocker, b"i am a file").unwrap();

        let outcomes = write_exports(
            &blocker,
            vec![(cache_key("a"), Some(plan(&source, "Song", "mp3", 5)))],
        );

        assert!(
            outcomes[0].error.as_deref().unwrap().contains("不是文件夹"),
            "{:?}",
            outcomes[0].error
        );
        // The blocking file was not replaced.
        assert_eq!(fs::read(&blocker).unwrap(), b"i am a file");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_missing_directory_is_created_once_for_the_batch() {
        let root = temp_dir("create");
        let source = root.join("payload.media");
        fs::write(&source, b"bytes").unwrap();
        let out = root.join("nested").join("export target");

        let outcomes = write_exports(
            &out,
            vec![(cache_key("a"), Some(plan(&source, "Song", "mp3", 5)))],
        );

        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0].error);
        assert!(destination_for(&out, &plan(&source, "Song", "mp3", 5)).is_file());
        fs::remove_dir_all(root).unwrap();
    }
}
