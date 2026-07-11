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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryBackup {
    pub version: u32,
    pub tracks: Vec<LibraryTrack>,
    pub playlists: Vec<PlaylistBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
             );",
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn add_tracks(&self, tracks: &[NewTrack]) -> Result<Vec<LibraryTrack>> {
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
        drop(connection);
        self.list_tracks(10_000)
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

    pub fn restore_backup(&self, backup: &LibraryBackup) -> Result<()> {
        if backup.version != 1 || backup.tracks.len() > 10_000 || backup.playlists.len() > 1_000 {
            bail!("unsupported or oversized library backup");
        }
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
}
