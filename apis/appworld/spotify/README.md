# AppWorld spotify — music simulation.

Task-critical surface: song/album/artist/playlist search, playlist songs via `playlist_get` → `Playlist.songs`, downloaded songs for offline play, player/queue. Auth taught via `login`.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/spotify
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/spotify/openapi.json apis/appworld/spotify
```

OpenAPI: `openapi.json`. Backend: `http://127.0.0.1:9000` after `appworld serve apis`.

## Scope notes

- `*_search` and `downloaded_song_query` use `kind: search`.
- Playlist songs are embedded on `playlist_get` (`from_parent_get` on `songs[]`).
  The collection is declared exhaustive: AppWorld's `Playlist._to_humanized_dict`
  serializes every member of `self.songs`, without a limit or continuation.
  Retaining each song object preserves its fields and nested artist observations.
  This assertion concerns playlist membership, not completeness of song details
  or upstream playlist queries.
- Song artists are also exhaustive: `Song._to_humanized_dict` serializes every
  member of `self.artists` for every response variant. This contract also applies
  to LikedSong, DownloadedSong, Player and QueueEntry producers. Album artists
  are exhaustive for both Album and LikedAlbum. Album/playlist shortened
  responses enumerate all `song_ids`; LikedAlbum.songs and LikedPlaylist.songs
  preserve that same complete membership. Completeness concerns membership,
  not whether every child detail field was observed.
- Regression gate: `python3 -m unittest discover -s apis/appworld/tests`.
  The producer inventory is independent of the abstract language matrix.
- Library / liked / following lists: library shelves via `song_library_*` / `album_library_*` /
  `playlist_library_query` on `Song` / `Album` / `Playlist`; liked shelves via `liked_song_query` /
  `liked_album_query` / `liked_playlist_query` on distinct `LikedSong` / `LikedAlbum` / `LikedPlaylist`
  entities; following via `following_artist_query`.

## Unified saved library

`Library{access_token=...}.songs` traverses the distinct Song identities across
tracks saved directly and songs in saved albums and playlists. `Library.albums`
and `Library.playlists` expose the saved collections. The composed view follows
ordinary Album.songs and Playlist.songs relations; Song details supply genre and
play_count. Independently liked items and recommendations are not included.

`Song{shelf="library", access_token=...}` retains its narrower direct-save meaning.
Library is a read projection; saves and removals remain operations on the
corresponding Song, Album, or Playlist identities.
