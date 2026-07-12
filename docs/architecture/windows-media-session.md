# Windows media-session contract

GXPlayer exposes one Windows System Media Transport Controls (SMTC) session
backed by the Rust audio engine. It remains active while the main window is
hidden to the system tray and appears in the Windows media flyout and media-key
controls. The main window also installs an independent `ITaskbarList3`
thumbnail toolbar with Previous, Play/Pause, and Next buttons.

The session publishes the current title, artist, album, duration, artwork when
available, playback state, and position. Metadata updates are tied to the audio
engine generation so late artwork or lyric work from an older track cannot
replace the current session.

Windows Play, Pause, Next, and Previous commands use the same queue authority as
the in-app controls. SMTC and the thumbnail toolbar emit a shared typed transport
action to the frontend because mixed local/online queues require on-demand source
resolution. The frontend also publishes navigation capabilities so the native
toolbar never infers queue boundaries from the engine's resolved-item queue.

The thumbnail toolbar subclasses the main HWND before its first show, adds its
buttons after `TaskbarButtonCreated`, and recreates them when Explorer rebuilds
the taskbar. COM calls and icon ownership remain on the Tauri UI thread. The
process AppUserModelID is set before Tauri creates any windows.

The frontend also accepts throttled `gx-player-snapshot` events, with a slower
IPC poll as a compatibility fallback. A poll response that began before or too
close to a newer push is discarded, so the fallback cannot move the UI
backwards. This keeps the UI and Windows media state in sync without rebuilding
the complete React tree every 150 milliseconds.
