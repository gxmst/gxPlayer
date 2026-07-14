use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryTrack {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_seconds: Option<f64>,
    pub favorite: bool,
    pub added_at_ms: i64,
    /// True when the file is missing on disk (filled by scan, not stored).
    #[serde(default)]
    pub missing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: i64,
    pub played_at_ms: i64,
    pub kind: String,
    pub title: String,
    pub artist: String,
    pub path: Option<String>,
    pub provider_id: Option<String>,
    pub provider_track_id: Option<String>,
    pub quality: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewHistoryEntry<'a> {
    pub kind: &'a str,
    pub title: &'a str,
    pub artist: &'a str,
    pub path: Option<&'a str>,
    pub provider_id: Option<&'a str>,
    pub provider_track_id: Option<&'a str>,
    pub quality: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct NewTrack {
    pub path: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistSummary {
    pub id: i64,
    pub name: String,
    pub track_count: usize,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibraryBackup {
    pub version: u32,
    pub tracks: Vec<LibraryTrack>,
    pub playlists: Vec<PlaylistBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PlaylistBackup {
    pub name: String,
    pub track_paths: Vec<String>,
}

pub struct LibraryStore {
    connection: Mutex<Connection>,
}

impl LibraryStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create library directory {}", parent.display())
            })?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("failed to open library database {}", path.display()))?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS tracks (
               id INTEGER PRIMARY KEY,
               path TEXT NOT NULL UNIQUE,
               title TEXT NOT NULL,
               artist TEXT NOT NULL DEFAULT '',
               album TEXT NOT NULL DEFAULT '',
               duration_seconds REAL,
               added_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS favorites (
               track_id INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS playlists (
               id INTEGER PRIMARY KEY,
               name TEXT NOT NULL COLLATE NOCASE UNIQUE,
               created_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS playlist_tracks (
               playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
               track_id INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
               position INTEGER NOT NULL,
               PRIMARY KEY (playlist_id, track_id)
             );
             CREATE TABLE IF NOT EXISTS play_history (
               id INTEGER PRIMARY KEY,
               played_at_ms INTEGER NOT NULL,
               kind TEXT NOT NULL,
               title TEXT NOT NULL,
               artist TEXT NOT NULL DEFAULT '',
               path TEXT,
               provider_id TEXT,
               provider_track_id TEXT,
               quality TEXT
             );
             CREATE INDEX IF NOT EXISTS idx_play_history_played_at ON play_history(played_at_ms DESC);",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn add_tracks(&self, tracks: &[NewTrack]) -> Result<Vec<LibraryTrack>> {
        self.upsert_tracks(tracks)?;
        self.list_tracks(10_000)
    }

    /// Insert or refresh track metadata without allocating and returning the
    /// entire library. Playback/import command paths should prefer this method;
    /// `add_tracks` remains for callers that explicitly need a refreshed list.
    pub fn upsert_tracks(&self, tracks: &[NewTrack]) -> Result<()> {
        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        let now = now_ms();
        for track in tracks {
            if track.path.trim().is_empty() || track.title.trim().is_empty() {
                bail!("track path and title are required");
            }
            transaction.execute(
                "INSERT INTO tracks(path, title, artist, album, duration_seconds, added_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET
                   title=excluded.title,
                   artist=excluded.artist,
                   album=excluded.album,
                   duration_seconds=excluded.duration_seconds",
                params![
                    track.path,
                    track.title,
                    track.artist,
                    track.album,
                    track.duration_seconds,
                    now
                ],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn list_tracks(&self, limit: usize) -> Result<Vec<LibraryTrack>> {
        let connection = self.connection.lock().unwrap();
        query_tracks(
            &connection,
            "SELECT t.id, t.path, t.title, t.artist, t.album, t.duration_seconds,
                    EXISTS(SELECT 1 FROM favorites f WHERE f.track_id=t.id), t.added_at_ms
             FROM tracks t ORDER BY t.added_at_ms DESC, t.id DESC LIMIT ?1",
            params![limit.min(10_000) as i64],
        )
    }

    pub fn track_by_path(&self, path: &str) -> Result<Option<LibraryTrack>> {
        let connection = self.connection.lock().unwrap();
        connection
            .query_row(
                "SELECT t.id, t.path, t.title, t.artist, t.album, t.duration_seconds,
                        EXISTS(SELECT 1 FROM favorites f WHERE f.track_id=t.id), t.added_at_ms
                 FROM tracks t WHERE t.path=?1",
                [path],
                |row| {
                    Ok(LibraryTrack {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        title: row.get(2)?,
                        artist: row.get(3)?,
                        album: row.get(4)?,
                        duration_seconds: row.get(5)?,
                        favorite: row.get(6)?,
                        added_at_ms: row.get(7)?,
                        missing: false,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_favorites(&self) -> Result<Vec<LibraryTrack>> {
        let connection = self.connection.lock().unwrap();
        query_tracks(
            &connection,
            "SELECT t.id, t.path, t.title, t.artist, t.album, t.duration_seconds, 1, t.added_at_ms
             FROM tracks t JOIN favorites f ON f.track_id=t.id ORDER BY t.title COLLATE NOCASE",
            [],
        )
    }

    pub fn set_favorite(&self, track_id: i64, favorite: bool) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        ensure_track(&connection, track_id)?;
        if favorite {
            connection.execute(
                "INSERT OR IGNORE INTO favorites(track_id) VALUES (?1)",
                [track_id],
            )?;
        } else {
            connection.execute("DELETE FROM favorites WHERE track_id=?1", [track_id])?;
        }
        Ok(())
    }

    pub fn create_playlist(&self, name: &str) -> Result<PlaylistSummary> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 80 {
            bail!("playlist name must contain 1 to 80 characters");
        }
        let connection = self.connection.lock().unwrap();
        let created_at_ms = now_ms();
        connection.execute(
            "INSERT INTO playlists(name, created_at_ms) VALUES (?1, ?2)",
            params![name, created_at_ms],
        )?;
        Ok(PlaylistSummary {
            id: connection.last_insert_rowid(),
            name: name.to_owned(),
            track_count: 0,
            created_at_ms,
        })
    }

    pub fn list_playlists(&self) -> Result<Vec<PlaylistSummary>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection.prepare(
            "SELECT p.id, p.name, COUNT(pt.track_id), p.created_at_ms
             FROM playlists p LEFT JOIN playlist_tracks pt ON pt.playlist_id=p.id
             GROUP BY p.id ORDER BY p.created_at_ms DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PlaylistSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                track_count: row.get::<_, i64>(2)? as usize,
                created_at_ms: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn delete_playlist(&self, playlist_id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        if connection.execute("DELETE FROM playlists WHERE id=?1", [playlist_id])? == 0 {
            bail!("playlist does not exist");
        }
        Ok(())
    }

    pub fn add_to_playlist(&self, playlist_id: i64, track_id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        ensure_track(&connection, track_id)?;
        let exists = connection
            .query_row("SELECT 1 FROM playlists WHERE id=?1", [playlist_id], |_| {
                Ok(())
            })
            .optional()?
            .is_some();
        if !exists {
            bail!("playlist does not exist");
        }
        connection.execute(
            "INSERT OR IGNORE INTO playlist_tracks(playlist_id, track_id, position)
             VALUES (?1, ?2, COALESCE((SELECT MAX(position)+1 FROM playlist_tracks WHERE playlist_id=?1), 0))",
            params![playlist_id, track_id],
        )?;
        Ok(())
    }

    pub fn remove_from_playlist(&self, playlist_id: i64, track_id: i64) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "DELETE FROM playlist_tracks WHERE playlist_id=?1 AND track_id=?2",
            params![playlist_id, track_id],
        )?;
        Ok(())
    }

    pub fn playlist_tracks(&self, playlist_id: i64) -> Result<Vec<LibraryTrack>> {
        let connection = self.connection.lock().unwrap();
        query_tracks(
            &connection,
            "SELECT t.id, t.path, t.title, t.artist, t.album, t.duration_seconds,
                    EXISTS(SELECT 1 FROM favorites f WHERE f.track_id=t.id), t.added_at_ms
             FROM playlist_tracks pt JOIN tracks t ON t.id=pt.track_id
             WHERE pt.playlist_id=?1 ORDER BY pt.position",
            [playlist_id],
        )
    }

    /// Mark library tracks whose files no longer exist on disk.
    pub fn scan_missing(&self) -> Result<Vec<LibraryTrack>> {
        let mut tracks = self.list_tracks(10_000)?;
        for track in &mut tracks {
            track.missing = !Path::new(&track.path).is_file();
        }
        Ok(tracks)
    }

    /// Move a library record to a replacement path without losing its identity or relationships.
    ///
    /// If the replacement path is already present, its favorite and playlist memberships are
    /// merged into the old record before the duplicate is removed. When the old path was only
    /// present in a restored playback queue (and not in the library), the replacement is upserted
    /// as a normal library track instead.
    pub fn relink_track(&self, old_path: &str, replacement: &NewTrack) -> Result<LibraryTrack> {
        if old_path.trim().is_empty()
            || replacement.path.trim().is_empty()
            || replacement.title.trim().is_empty()
        {
            bail!("old path, replacement path, and title are required");
        }

        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        let old_id = transaction
            .query_row("SELECT id FROM tracks WHERE path=?1", [old_path], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;

        if let Some(old_id) = old_id {
            let replacement_id = transaction
                .query_row(
                    "SELECT id FROM tracks WHERE path=?1",
                    [&replacement.path],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;

            if let Some(replacement_id) = replacement_id.filter(|id| *id != old_id) {
                transaction.execute(
                    "INSERT OR IGNORE INTO favorites(track_id)
                     SELECT ?1 WHERE EXISTS(SELECT 1 FROM favorites WHERE track_id=?2)",
                    params![old_id, replacement_id],
                )?;
                transaction.execute(
                    "INSERT OR IGNORE INTO playlist_tracks(playlist_id, track_id, position)
                     SELECT playlist_id, ?1, position
                     FROM playlist_tracks WHERE track_id=?2",
                    params![old_id, replacement_id],
                )?;
                transaction.execute("DELETE FROM tracks WHERE id=?1", [replacement_id])?;
            }

            // Only the path changes: the old row remains the canonical identity, preserving its
            // metadata, favorite, playlist memberships, and original added_at timestamp.
            transaction.execute(
                "UPDATE tracks SET path=?1 WHERE id=?2",
                params![replacement.path, old_id],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO tracks(path, title, artist, album, duration_seconds, added_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(path) DO UPDATE SET
                   title=excluded.title,
                   artist=excluded.artist,
                   album=excluded.album,
                   duration_seconds=excluded.duration_seconds",
                params![
                    replacement.path,
                    replacement.title,
                    replacement.artist,
                    replacement.album,
                    replacement.duration_seconds,
                    now_ms()
                ],
            )?;
        }

        transaction.commit()?;
        drop(connection);
        self.track_by_path(&replacement.path)?
            .context("replacement track disappeared after relink")
    }

    pub fn record_history(&self, entry: NewHistoryEntry<'_>) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        connection.execute(
            "INSERT INTO play_history(played_at_ms, kind, title, artist, path, provider_id, provider_track_id, quality)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                now_ms(),
                entry.kind,
                entry.title,
                entry.artist,
                entry.path,
                entry.provider_id,
                entry.provider_track_id,
                entry.quality
            ],
        )?;
        // Keep the latest 500 entries.
        connection.execute(
            "DELETE FROM play_history WHERE id NOT IN (
               SELECT id FROM play_history ORDER BY played_at_ms DESC LIMIT 500
             )",
            [],
        )?;
        Ok(())
    }

    pub fn list_history(&self, limit: usize) -> Result<Vec<HistoryEntry>> {
        let connection = self.connection.lock().unwrap();
        let mut statement = connection.prepare(
            "SELECT id, played_at_ms, kind, title, artist, path, provider_id, provider_track_id, quality
             FROM play_history ORDER BY played_at_ms DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit.min(500) as i64], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                played_at_ms: row.get(1)?,
                kind: row.get(2)?,
                title: row.get(3)?,
                artist: row.get(4)?,
                path: row.get(5)?,
                provider_id: row.get(6)?,
                provider_track_id: row.get(7)?,
                quality: row.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    pub fn clear_history(&self) -> Result<()> {
        let connection = self.connection.lock().unwrap();
        connection.execute("DELETE FROM play_history", [])?;
        Ok(())
    }

    pub fn export_backup(&self) -> Result<LibraryBackup> {
        let tracks = self.list_tracks(10_000)?;
        let playlists = self
            .list_playlists()?
            .into_iter()
            .map(|playlist| {
                Ok(PlaylistBackup {
                    name: playlist.name,
                    track_paths: self
                        .playlist_tracks(playlist.id)?
                        .into_iter()
                        .map(|track| track.path)
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LibraryBackup {
            version: 1,
            tracks,
            playlists,
        })
    }

    /// Validate every constraint used by backup restoration without changing the database.
    pub fn validate_backup(backup: &LibraryBackup) -> Result<()> {
        if backup.version != 1 || backup.tracks.len() > 10_000 || backup.playlists.len() > 1_000 {
            bail!("unsupported or oversized library backup");
        }

        let mut track_paths = HashSet::with_capacity(backup.tracks.len());
        for track in &backup.tracks {
            if track.path.trim().is_empty() || track.title.trim().is_empty() {
                bail!("library backup contains a track without a path or title");
            }
            if !track_paths.insert(track.path.as_str()) {
                bail!(
                    "library backup contains duplicate track path '{}'",
                    track.path
                );
            }
        }

        let mut playlist_names = HashSet::with_capacity(backup.playlists.len());
        for playlist in &backup.playlists {
            let name = playlist.name.trim();
            if name.is_empty() || name.chars().count() > 80 {
                bail!("library backup contains an invalid playlist name");
            }
            // SQLite's NOCASE collation is ASCII case-insensitive.
            if !playlist_names.insert(name.to_ascii_lowercase()) {
                bail!("library backup contains duplicate playlist name '{name}'");
            }
            let mut playlist_paths = HashSet::with_capacity(playlist.track_paths.len());
            for path in &playlist.track_paths {
                if !track_paths.contains(path.as_str()) {
                    bail!("playlist '{name}' references a track missing from the backup");
                }
                if !playlist_paths.insert(path.as_str()) {
                    bail!("playlist '{name}' contains a duplicate track");
                }
            }
        }
        Ok(())
    }

    pub fn restore_backup(&self, backup: &LibraryBackup) -> Result<()> {
        Self::validate_backup(backup)?;
        let mut connection = self.connection.lock().unwrap();
        let transaction = connection.transaction()?;
        transaction.execute_batch(
            "DELETE FROM playlist_tracks; DELETE FROM playlists; DELETE FROM favorites; DELETE FROM tracks;",
        )?;
        for track in &backup.tracks {
            transaction.execute(
                "INSERT INTO tracks(path, title, artist, album, duration_seconds, added_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    track.path,
                    track.title,
                    track.artist,
                    track.album,
                    track.duration_seconds,
                    track.added_at_ms
                ],
            )?;
            if track.favorite {
                let id = transaction.last_insert_rowid();
                transaction.execute("INSERT INTO favorites(track_id) VALUES (?1)", [id])?;
            }
        }
        for playlist in &backup.playlists {
            transaction.execute(
                "INSERT INTO playlists(name, created_at_ms) VALUES (?1, ?2)",
                params![playlist.name, now_ms()],
            )?;
            let playlist_id = transaction.last_insert_rowid();
            for (position, path) in playlist.track_paths.iter().enumerate() {
                let track_id: Option<i64> = transaction
                    .query_row("SELECT id FROM tracks WHERE path=?1", [path], |row| {
                        row.get(0)
                    })
                    .optional()?;
                if let Some(track_id) = track_id {
                    transaction.execute(
                        "INSERT INTO playlist_tracks(playlist_id, track_id, position) VALUES (?1, ?2, ?3)",
                        params![playlist_id, track_id, position as i64],
                    )?;
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }
}

fn query_tracks<P: rusqlite::Params>(
    connection: &Connection,
    sql: &str,
    params: P,
) -> Result<Vec<LibraryTrack>> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params, |row| {
        Ok(LibraryTrack {
            id: row.get(0)?,
            path: row.get(1)?,
            title: row.get(2)?,
            artist: row.get(3)?,
            album: row.get(4)?,
            duration_seconds: row.get(5)?,
            favorite: row.get(6)?,
            added_at_ms: row.get(7)?,
            missing: false,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(Into::into)
}

fn ensure_track(connection: &Connection, track_id: i64) -> Result<()> {
    if connection
        .query_row("SELECT 1 FROM tracks WHERE id=?1", [track_id], |_| Ok(()))
        .optional()?
        .is_none()
    {
        bail!("track does not exist");
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_favorites_playlists_and_backup() {
        let store = LibraryStore::open(":memory:").unwrap();
        let tracks = store
            .add_tracks(&[
                NewTrack {
                    path: "C:/Music/one.flac".into(),
                    title: "One".into(),
                    artist: "Artist".into(),
                    album: "Album".into(),
                    duration_seconds: Some(120.0),
                },
                NewTrack {
                    path: "C:/Music/two.mp3".into(),
                    title: "Two".into(),
                    artist: String::new(),
                    album: String::new(),
                    duration_seconds: None,
                },
            ])
            .unwrap();
        let one = tracks.iter().find(|track| track.title == "One").unwrap();
        assert_eq!(
            store
                .track_by_path("C:/Music/one.flac")
                .unwrap()
                .unwrap()
                .album,
            "Album"
        );
        store
            .upsert_tracks(&[NewTrack {
                path: "C:/Music/one.flac".into(),
                title: "One (Remastered)".into(),
                artist: "Artist".into(),
                album: "Album".into(),
                duration_seconds: Some(121.0),
            }])
            .unwrap();
        assert_eq!(store.list_tracks(100).unwrap().len(), 2);
        assert_eq!(
            store
                .track_by_path("C:/Music/one.flac")
                .unwrap()
                .unwrap()
                .title,
            "One (Remastered)"
        );
        store.set_favorite(one.id, true).unwrap();
        let playlist = store.create_playlist("夜间").unwrap();
        store.add_to_playlist(playlist.id, one.id).unwrap();
        assert_eq!(store.list_favorites().unwrap().len(), 1);
        assert_eq!(store.playlist_tracks(playlist.id).unwrap().len(), 1);

        let backup = store.export_backup().unwrap();
        let restored = LibraryStore::open(":memory:").unwrap();
        restored.restore_backup(&backup).unwrap();
        assert_eq!(restored.list_tracks(100).unwrap().len(), 2);
        assert_eq!(restored.list_favorites().unwrap().len(), 1);
        assert_eq!(restored.list_playlists().unwrap()[0].name, "夜间");
    }

    #[test]
    fn relink_preserves_old_identity_metadata_and_memberships() {
        let store = LibraryStore::open(":memory:").unwrap();
        let old = store
            .add_tracks(&[NewTrack {
                path: "D:/Offline/old.flac".into(),
                title: "Original title".into(),
                artist: "Original artist".into(),
                album: "Original album".into(),
                duration_seconds: Some(180.0),
            }])
            .unwrap()
            .pop()
            .unwrap();
        store.set_favorite(old.id, true).unwrap();
        let playlist = store.create_playlist("Moved songs").unwrap();
        store.add_to_playlist(playlist.id, old.id).unwrap();

        let relinked = store
            .relink_track(
                &old.path,
                &NewTrack {
                    path: "E:/Music/new.flac".into(),
                    title: "Probed title".into(),
                    artist: "Probed artist".into(),
                    album: "Probed album".into(),
                    duration_seconds: Some(181.0),
                },
            )
            .unwrap();

        assert_eq!(relinked.id, old.id);
        assert_eq!(relinked.added_at_ms, old.added_at_ms);
        assert_eq!(relinked.title, "Original title");
        assert_eq!(relinked.path, "E:/Music/new.flac");
        assert!(relinked.favorite);
        assert!(store.track_by_path(&old.path).unwrap().is_none());
        assert_eq!(store.playlist_tracks(playlist.id).unwrap(), vec![relinked]);
    }

    #[test]
    fn relink_merges_an_existing_target_into_the_old_record() {
        let store = LibraryStore::open(":memory:").unwrap();
        let tracks = store
            .add_tracks(&[
                NewTrack {
                    path: "D:/Offline/old.flac".into(),
                    title: "Old".into(),
                    artist: String::new(),
                    album: String::new(),
                    duration_seconds: None,
                },
                NewTrack {
                    path: "E:/Music/existing.flac".into(),
                    title: "Existing".into(),
                    artist: String::new(),
                    album: String::new(),
                    duration_seconds: None,
                },
            ])
            .unwrap();
        let old = tracks.iter().find(|track| track.title == "Old").unwrap();
        let existing = tracks
            .iter()
            .find(|track| track.title == "Existing")
            .unwrap();
        store.set_favorite(existing.id, true).unwrap();
        let old_playlist = store.create_playlist("Old membership").unwrap();
        let target_playlist = store.create_playlist("Target membership").unwrap();
        store.add_to_playlist(old_playlist.id, old.id).unwrap();
        store
            .add_to_playlist(target_playlist.id, existing.id)
            .unwrap();

        let relinked = store
            .relink_track(
                &old.path,
                &NewTrack {
                    path: existing.path.clone(),
                    title: "Ignored probe metadata".into(),
                    artist: String::new(),
                    album: String::new(),
                    duration_seconds: None,
                },
            )
            .unwrap();

        assert_eq!(relinked.id, old.id);
        assert_eq!(relinked.added_at_ms, old.added_at_ms);
        assert_eq!(relinked.title, "Old");
        assert!(relinked.favorite);
        assert_eq!(store.list_tracks(10).unwrap().len(), 1);
        assert_eq!(
            store.playlist_tracks(old_playlist.id).unwrap()[0].id,
            old.id
        );
        assert_eq!(
            store.playlist_tracks(target_playlist.id).unwrap()[0].id,
            old.id
        );
    }

    #[test]
    fn relink_upserts_when_the_old_path_is_not_in_the_library() {
        let store = LibraryStore::open(":memory:").unwrap();
        let relinked = store
            .relink_track(
                "D:/Restored/missing.flac",
                &NewTrack {
                    path: "E:/Music/found.flac".into(),
                    title: "Found".into(),
                    artist: "Artist".into(),
                    album: String::new(),
                    duration_seconds: Some(99.0),
                },
            )
            .unwrap();

        assert_eq!(relinked.path, "E:/Music/found.flac");
        assert_eq!(relinked.title, "Found");
        assert_eq!(store.list_tracks(10).unwrap(), vec![relinked]);
    }
}
