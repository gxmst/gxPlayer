# Windows media-session contract

GXPlayer exposes one Windows System Media Transport Controls (SMTC) session
backed by the Rust audio engine. It remains active while the main window is
hidden to the system tray and appears in the Windows taskbar media flyout and
media-key controls. This is the OS media session surface, not an
`ITaskbarList3` thumbnail-toolbar implementation.

The session publishes the current title, artist, album, duration, artwork when
available, playback state, and position. Metadata updates are tied to the audio
engine generation so late artwork or lyric work from an older track cannot
replace the current session.

Windows Play, Pause, Next, and Previous commands use the same queue authority as
the in-app controls. Next and Previous are emitted to the frontend because mixed
local/online queues require on-demand source resolution. Play and Pause may be
handled directly by the audio engine.

The frontend also accepts throttled `gx-player-snapshot` events, with a slower
IPC poll as a compatibility fallback. A poll response that began before or too
close to a newer push is discarded, so the fallback cannot move the UI
backwards. This keeps the UI and Windows media state in sync without rebuilding
the complete React tree every 150 milliseconds.
