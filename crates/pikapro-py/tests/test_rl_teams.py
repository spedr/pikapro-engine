"""Tournament team pools: pokepaste lists, export texts, directories.

No test touches the network: the pastes are local fixtures
(``tests/fixtures/pokepastes``, legal championsvgc teams written for the
tests), copied into a cache, and every fetch goes through a fake opener.
"""

import shutil
from pathlib import Path

import numpy as np
import pytest

import pikapro
import pikapro.rl as rl
from pikapro.rl import teams

FIXTURES = Path(__file__).parent / "fixtures"
PASTES = FIXTURES / "pokepastes"
LIST = FIXTURES / "pokepastes.txt"


def no_network(url, timeout):
    raise AssertionError(f"the test tried to fetch {url}")


@pytest.fixture
def cache(tmp_path):
    d = tmp_path / "cache"
    shutil.copytree(PASTES, d)
    return d


def test_paste_ids_and_lists():
    assert teams.paste_id("https://pokepast.es/aaaaaaaaaaaa0001") == "aaaaaaaaaaaa0001"
    assert teams.paste_id("https://pokepast.es/aaaaaaaaaaaa0001/raw") == "aaaaaaaaaaaa0001"
    assert teams.paste_id("pokepast.es/abc123/") == "abc123"
    with pytest.raises(ValueError):
        teams.paste_id("https://example.com/abc")
    urls = teams.read_pokepaste_list(LIST)
    assert len(urls) == 5 and not any(u.startswith("#") or " " in u for u in urls)


def test_pool_from_a_pokepaste_list_reads_the_cache(cache):
    pool = teams.from_pokepaste_list(LIST, cache, "championsvgc", opener=no_network)
    assert len(pool) == 3 and pool.format == "championsvgc"
    assert pool.labels[0] == "https://pokepast.es/aaaaaaaaaaaa0001"
    rejected = {label: problems for _, label, problems in pool.rejected}
    assert set(rejected) == {"https://pokepast.es/badbadbadbad0001", "https://pokepast.es/5e5e5e5e5e5e5e5e/"}
    assert any("Species Clause" in p for p in rejected["https://pokepast.es/badbadbadbad0001"])
    assert all(pikapro.validate(t, "championsvgc") == [] for t in pool.teams())
    # The pool plays: both players draw from it.
    env = rl.VecEnv("championsvgc", n=4, teams=pool, seed=4_620_000)
    out = env.reset(seed=4_620_000)
    assert out.needs_action[:, 0].all()
    # Two pools: one per player.
    env = rl.VecEnv("championsvgc", n=2, teams=[pool, pool])
    env.reset(seed=1)


def test_fetches_once_into_the_cache(tmp_path):
    calls = []

    def opener(url, timeout):
        calls.append((url, timeout))
        pid = url.split("/")[-2]
        return (PASTES / f"{pid}.txt").read_bytes().replace(b"\n", b"\r\n")

    cache = tmp_path / "c"
    pool = teams.from_pokepaste_list(LIST, cache, opener=opener, timeout=3.0)
    assert len(pool) == 3 and len(calls) == 5
    assert calls[0] == ("https://pokepast.es/aaaaaaaaaaaa0001/raw", 3.0)
    assert (cache / "aaaaaaaaaaaa0001.txt").exists()
    assert "\r" not in (cache / "aaaaaaaaaaaa0001.txt").read_text()
    # Cached now: no network, even with fetching off.
    again = teams.from_pokepaste_list(LIST, cache, opener=no_network, fetch=False)
    assert again.teams() == pool.teams()


def test_fetch_errors_are_clear(tmp_path):
    import urllib.error

    def down(url, timeout):
        raise urllib.error.URLError("timed out")

    with pytest.raises(teams.PokepasteError, match="pokepast.es/abc/raw"):
        teams.fetch_paste("https://pokepast.es/abc", tmp_path, opener=down)
    with pytest.raises(teams.PokepasteError, match="not in the cache"):
        teams.fetch_paste("https://pokepast.es/abc", tmp_path, fetch=False)
    with pytest.raises(teams.PokepasteError, match="no team text"):
        teams.fetch_paste("https://pokepast.es/abc", tmp_path, opener=lambda u, t: b"<html>404</html>")
    with pytest.raises(teams.PokepasteError, match="larger than"):
        teams.fetch_paste("https://pokepast.es/abc", tmp_path, opener=lambda u, t: b"x" * (teams.MAX_PASTE + 1))


def test_a_bad_list_line_names_itself(cache, tmp_path):
    lst = tmp_path / "list.txt"
    lst.write_text(LIST.read_text() + "\nhttps://example.com/not-a-paste\n")
    with pytest.raises(teams.PokepasteError, match=r"link \d+: not a pokepaste link"):
        teams.from_pokepaste_list(lst, cache, opener=no_network)


def test_fetches_leave_no_temporary_files(tmp_path):
    text = (PASTES / next(p.name for p in PASTES.iterdir())).read_bytes()
    teams.fetch_paste("https://pokepast.es/abc", tmp_path, opener=lambda u, t: text)
    assert [p.name for p in tmp_path.iterdir()] == ["abc.txt"]


def test_pools_from_texts_and_directories(tmp_path):
    texts = [p.read_text() for p in sorted(PASTES.glob("*.txt"))]
    pool = teams.from_export_texts(texts, "championsvgc")
    assert len(pool) == 3 and len(pool.rejected) == 2 and pool.rejected[0][1].startswith("#")
    d = teams.from_dir(PASTES, "championsvgc")
    assert len(d) == 3 and all(label.endswith(".txt") for label in d.labels)
    assert d.team(0, packed=True).count("]") == 5
    with pytest.raises(IndexError):
        d.team(3)
    # A pool for another format is refused.
    with pytest.raises(ValueError):
        rl.VecEnv("gen9vgc", teams=d)
    backup = "=== [gen9] One ===\n\n" + texts[0] + "\n=== [gen9] Two ===\n\n" + texts[1]
    assert len(teams.split_teams(backup)) == 2
    assert np.all([len(t) > 0 for t in teams.split_teams(texts[0])])


def test_listed_mega_formes_play_as_their_base_formes():
    """A paste that lists a Mega forme (``Charizard-Mega-Y @ Charizardite Y``)
    is played as Showdown's validator leaves it: in the forme it Mega
    Evolves from, with that forme's ability."""
    sample = pikapro.sample_team("championsvgc")
    listed = sample.replace("Charizard @ Charizardite Y", "Charizard-Mega-Y @ Charizardite Y")
    assert listed != sample
    pool = teams.from_export_texts([listed], "championsvgc")
    assert not pool.rejected
    assert "Charizard @ Charizardite Y" in pool.team(0)
    assert "Mega" not in pool.team(0).split("@")[0]
    assert pool.team(0) == teams.from_export_texts([sample], "championsvgc").team(0)
