use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use gx_source::safe_http::{SafeHttpRequest, execute, validate_and_resolve};
use reqwest::{Method, Url};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{State, WebviewWindow};

use crate::require_window;

const ARTWORK_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_ARTWORK_URL_BYTES: usize = 16 * 1024;
const MAX_ARTWORK_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ArtworkAsset {
    pub(crate) mime: &'static str,
    pub(crate) path: PathBuf,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtworkPayload {
    mime: &'static str,
    data_url: String,
}

#[derive(Debug, Clone, Copy)]
struct ImageKind {
    mime: &'static str,
    extension: &'static str,
}

#[derive(Default)]
struct FetchSlot {
    result: Mutex<Option<Result<ArtworkAsset, String>>>,
    ready: Condvar,
}

struct ArtworkCacheInner {
    root: PathBuf,
    in_flight: Mutex<HashMap<String, Arc<FetchSlot>>>,
}

#[derive(Clone)]
pub struct ArtworkCache {
    inner: Arc<ArtworkCacheInner>,
}

impl ArtworkCache {
    pub fn new(root: PathBuf) -> Self {
        Self {
            inner: Arc::new(ArtworkCacheInner {
                root,
                in_flight: Mutex::new(HashMap::new()),
            }),
        }
    }

    pub(crate) fn ensure(&self, raw_url: &str) -> Result<ArtworkAsset, String> {
        if raw_url.len() > MAX_ARTWORK_URL_BYTES {
            return Err("封面地址超过长度限制".into());
        }
        let url = Url::parse(raw_url).map_err(|error| format!("封面地址无效：{error}"))?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err("封面只允许使用 HTTP(S) 地址".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("封面地址不允许携带凭据".into());
        }
        if url.host_str().is_none() {
            return Err("封面地址缺少主机名".into());
        }
        // Apply the same SSRF policy on cache hits as on downloads. This prevents an old or
        // manually placed cache file from turning a private destination into a usable asset.
        validate_and_resolve(&url).map_err(|error| format!("封面地址被拒绝：{error}"))?;
        let key = cache_key(url.as_str());
        if let Some(asset) = find_cached(&self.inner.root, &key) {
            return Ok(asset);
        }

        let (slot, leader) = {
            let mut in_flight = self.inner.in_flight.lock().unwrap();
            if let Some(slot) = in_flight.get(&key) {
                (Arc::clone(slot), false)
            } else {
                let slot = Arc::new(FetchSlot::default());
                in_flight.insert(key.clone(), Arc::clone(&slot));
                (slot, true)
            }
        };

        if leader {
            let result = self.download(&url, &key);
            *slot.result.lock().unwrap() = Some(result.clone());
            slot.ready.notify_all();
            self.inner.in_flight.lock().unwrap().remove(&key);
            result
        } else {
            let mut result = slot.result.lock().unwrap();
            loop {
                if let Some(result) = result.as_ref() {
                    return result.clone();
                }
                result = slot.ready.wait(result).unwrap();
            }
        }
    }

    fn download(&self, url: &Url, key: &str) -> Result<ArtworkAsset, String> {
        let response = execute(SafeHttpRequest {
            url: url.clone(),
            method: Method::GET,
            headers: vec![(
                "accept".into(),
                "image/webp,image/png,image/jpeg,image/gif".into(),
            )],
            body: None,
            timeout: ARTWORK_TIMEOUT,
            max_response_bytes: MAX_ARTWORK_BYTES,
        })
        .map_err(|error| format!("封面下载失败：{error}"))?;
        if !(200..300).contains(&response.status) {
            return Err(format!("封面下载返回 HTTP {}", response.status));
        }
        let kind = detect_image_kind(&response.body)
            .ok_or_else(|| "封面响应不是受支持的栅格图片".to_owned())?;
        fs::create_dir_all(&self.inner.root).map_err(|error| error.to_string())?;
        let path = self.inner.root.join(format!("{key}.{}", kind.extension));
        write_cache_file(&path, &response.body)?;
        prune_cache(&self.inner.root, &path);
        Ok(ArtworkAsset {
            mime: kind.mime,
            path,
        })
    }
}

#[tauri::command]
pub async fn artwork_get(
    window: WebviewWindow,
    cache: State<'_, ArtworkCache>,
    url: String,
) -> Result<ArtworkPayload, String> {
    require_window(&window, "main")?;
    let cache = cache.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let asset = cache.ensure(&url)?;
        let bytes = fs::read(&asset.path).map_err(|error| error.to_string())?;
        if bytes.len() > MAX_ARTWORK_BYTES {
            return Err("缓存封面超过大小限制".into());
        }
        Ok(ArtworkPayload {
            mime: asset.mime,
            data_url: format!("data:{};base64,{}", asset.mime, B64.encode(bytes)),
        })
    })
    .await
    .map_err(|error| format!("封面读取任务失败：{error}"))?
}

fn cache_key(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn find_cached(root: &Path, key: &str) -> Option<ArtworkAsset> {
    image_kinds().into_iter().find_map(|kind| {
        let path = root.join(format!("{key}.{}", kind.extension));
        let valid = cached_image_kind(&path).is_some_and(|detected| detected.mime == kind.mime);
        if valid {
            Some(ArtworkAsset {
                mime: kind.mime,
                path,
            })
        } else {
            let _ = fs::remove_file(path);
            None
        }
    })
}

fn cached_image_kind(path: &Path) -> Option<ImageKind> {
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ARTWORK_BYTES as u64 {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    (bytes.len() <= MAX_ARTWORK_BYTES)
        .then(|| detect_image_kind(&bytes))
        .flatten()
}

fn image_kinds() -> [ImageKind; 4] {
    [
        ImageKind {
            mime: "image/jpeg",
            extension: "jpg",
        },
        ImageKind {
            mime: "image/png",
            extension: "png",
        },
        ImageKind {
            mime: "image/gif",
            extension: "gif",
        },
        ImageKind {
            mime: "image/webp",
            extension: "webp",
        },
    ]
}

fn detect_image_kind(bytes: &[u8]) -> Option<ImageKind> {
    let kinds = image_kinds();
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(kinds[0])
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(kinds[1])
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(kinds[2])
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(kinds[3])
    } else {
        None
    }
}

fn write_cache_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    let temporary = path.with_extension(format!(
        "{}.tmp-{}-{sequence}",
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("image"),
        std::process::id()
    ));
    fs::write(&temporary, bytes).map_err(|error| error.to_string())?;
    match fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            let expected_extension = path.extension().and_then(|value| value.to_str());
            let existing_is_valid = cached_image_kind(path)
                .is_some_and(|kind| Some(kind.extension) == expected_extension);
            if existing_is_valid {
                let _ = fs::remove_file(temporary);
                Ok(())
            } else {
                fs::remove_file(path).map_err(|error| error.to_string())?;
                let result = fs::rename(&temporary, path).map_err(|error| error.to_string());
                if result.is_err() {
                    let _ = fs::remove_file(temporary);
                }
                result
            }
        }
        Err(error) => {
            let _ = fs::remove_file(temporary);
            Err(error.to_string())
        }
    }
}

fn prune_cache(root: &Path, keep: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let extension = path.extension().and_then(|value| value.to_str())?;
            if !image_kinds().iter().any(|kind| kind.extension == extension) {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then_some((
                path,
                metadata.len(),
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            ))
        })
        .collect::<Vec<_>>();
    let mut total = files.iter().map(|(_, length, _)| length).sum::<u64>();
    if total <= MAX_CACHE_BYTES {
        return;
    }
    files.sort_by_key(|(_, _, modified)| *modified);
    for (path, length, _) in files {
        if total <= MAX_CACHE_BYTES {
            break;
        }
        if path != keep && fs::remove_file(&path).is_ok() {
            total = total.saturating_sub(length);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_common_raster_signatures_and_rejects_active_content() {
        assert_eq!(
            detect_image_kind(&[0xff, 0xd8, 0xff]).unwrap().mime,
            "image/jpeg"
        );
        assert_eq!(
            detect_image_kind(b"\x89PNG\r\n\x1a\nrest").unwrap().mime,
            "image/png"
        );
        assert_eq!(detect_image_kind(b"GIF89arest").unwrap().mime, "image/gif");
        assert_eq!(
            detect_image_kind(b"RIFF0000WEBPrest").unwrap().mime,
            "image/webp"
        );
        assert!(detect_image_kind(b"<svg><script/></svg>").is_none());
        assert!(detect_image_kind(b"<html></html>").is_none());
    }

    #[test]
    fn cache_keys_do_not_expose_the_source_url() {
        let key = cache_key("https://example.invalid/cover.jpg?token=secret");
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|value| value.is_ascii_hexdigit()));
        assert!(!key.contains("secret"));
    }
}
