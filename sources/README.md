# ChKSz LX source

`chksz-api.js` is an importable GXPlayer LX protocol-v2 source adapter for the
ChKSz API. It supports playback, one-provider search fallback, lyrics and
NetEase playlist reads. It does not contain an API key.

After importing the script in GXPlayer, open its source settings and use this
JSON configuration, replacing the placeholder with your own key:

```json
{
  "lsConfig": {
    "api": {
      "addr": "https://api.chksz.com",
      "pass": "chksz_your_personal_key",
      "searchProvider": "wy"
    }
  }
}
```

The adapter maps GXPlayer qualities to ChKSz values as follows:

- `128k` -> `128k`
- `320k` -> `320k`
- `flac` -> `flac`
- `flac24bit` -> `master` for QQ/Kugou and `jymaster` for NetEase

`searchProvider` may be `wy`, `tx` or `kg`. GXPlayer only uses this paid search
after its built-in metadata search returns no results, and only one configured
provider is called. Suggestions never use the paid fallback. Identical source
searches are cached for 10 minutes, lyrics for 24 hours and playlists for 10
minutes while the source realm remains active.

The key is stored in the source configuration and is only sent as the
ChKSz `apikey` query parameter. Do not commit a real key or paste it into a
public issue, screenshot, log, or URL.
