# AppWorld spotify — music simulation.

Task-critical surface: song/album/artist/playlist search, playlist songs via `playlist_get` → `Playlist.songs`, downloaded songs for offline play, player/queue. Auth taught via `login`.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/spotify
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/spotify/openapi.json apis/appworld/spotify
```

OpenAPI: `openapi.json`. Backend: `http://127.0.0.1:9000` after `appworld serve apis`.

## Scope notes

- `*_search` and `downloaded_song_query` use `kind: search`.
- Playlist songs are embedded on `playlist_get` (`from_parent_get` on `songs[].id`) — not a separate list endpoint and not a `views:` composition.
- Library / liked / following lists (`liked_song_query`, `song_library_*`, `album_library_*`, `playlist_library_query`, `liked_playlist_query`, `liked_album_query`, `following_artist_query`).
