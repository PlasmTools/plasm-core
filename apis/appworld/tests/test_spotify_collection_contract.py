"""Pinned AppWorld producer contracts, independent of the language matrix.

Spotify Song/Album/Playlist._to_humanized_dict enumerate all members without
pagination; UserDownloadedSong delegates to Song(shortened). These assertions
apply to every CGS entity exposing those producer arrays, not one task or alias.
Run: python3 -m unittest discover -s plasm-oss/apis/appworld/tests
"""
from pathlib import Path
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[1]


class SpotifyCollectionContract(unittest.TestCase):
    def test_full_producer_arrays_are_consumable_as_whole_collections(self):
        cgs = yaml.safe_load((ROOT / 'spotify/domain.yaml').read_text())
        # Independent producer inventory from AppWorld 0.2.0 Spotify serializers.
        producers = {
            'Song': {'artists': 'artists'},
            'LikedSong': {'artists': 'artists'},
            'Album': {'artists': 'artists', 'songs': 'songs'},
            'LikedAlbum': {'artists': 'artists', 'songs': 'song_ids'},
            'Playlist': {'songs': 'songs'},
            'LikedPlaylist': {'songs': 'song_ids'},
            'DownloadedSong': {'artists': 'artists'},
        }
        for entity, relations in producers.items():
            for relation, wire_array in relations.items():
                with self.subTest(entity=entity, relation=relation):
                    materialize = cgs['entities'][entity]['relations'][relation]['materialize']
                    self.assertEqual(materialize['path'][0], {'key': wire_array})
                    self.assertEqual(materialize.get('collection_coverage'), 'complete',
                                     'full producer membership must not default to Unknown')


if __name__ == '__main__':
    unittest.main()
