"""Team pools: tournament teams from pokepaste links, export texts or a
directory, imported and validated against a format.

A pool is a :class:`pikapro.rl.TeamPool`: the teams that import and pass the
format's validator (Showdown's: championsvgc and gen9vgc their
Regulations, gen9ou OU, the random battles and custom games Anything Goes
with Species Clause), and the rejects with their problems in Showdown's words.
Pass it as a :class:`pikapro.rl.VecEnv`'s ``teams`` (both players draw from
it), or two pools (one per player)::

    pool = rl.teams.from_pokepaste_list("teams.txt", "~/.cache/pikapro-pastes")
    print(len(pool), "teams;", len(pool.rejected), "rejected")
    env = rl.VecEnv("championsvgc", n=1024, teams=pool)

The pokepaste list is a text file with one link per line
(``https://pokepast.es/<id>``); blank lines and ``#`` comments are
skipped. Each paste is fetched once, from its raw text endpoint
(``https://pokepast.es/<id>/raw``), into ``cache_dir``; later loads read
the cache and never touch the network.
"""

from __future__ import annotations

import os
import re
import tempfile
import urllib.error
import urllib.request
from pathlib import Path
from typing import Callable, List, Optional, Sequence, Union

from .._pikapro import TeamPool

__all__ = [
    "PokepasteError",
    "TeamPool",
    "fetch_paste",
    "from_dir",
    "from_export_texts",
    "from_pokepaste_list",
    "paste_id",
    "read_pokepaste_list",
    "split_teams",
]

PathLike = Union[str, os.PathLike]

_PASTE = re.compile(r"^(?:https?://)?(?:www\.)?pokepast\.es/([0-9A-Za-z]+)(?:/(?:raw|json)?)?/?(?:[?#].*)?$")


class PokepasteError(RuntimeError):
    """A pokepaste that couldn't be fetched (with the URL and why)."""


def paste_id(url: str) -> str:
    """The id of a pokepaste link (``https://pokepast.es/<id>``, with or
    without ``/raw``); raises ``ValueError`` for anything else."""
    m = _PASTE.match(url.strip())
    if not m:
        raise ValueError(f"not a pokepaste link: {url!r}")
    return m.group(1)


def read_pokepaste_list(path: PathLike) -> List[str]:
    """The links of a pokepaste list: one per line, blank lines and ``#``
    comments (whole-line or trailing) skipped."""
    out = []
    for line in Path(path).expanduser().read_text(encoding="utf-8").splitlines():
        line = line.split("#", 1)[0].strip()
        if line:
            out.append(line)
    return out


def _normalize(text: str) -> str:
    return text.replace("\r\n", "\n").replace("\r", "\n").strip() + "\n"


Opener = Callable[[str, float], bytes]

#: The largest paste fetched (a team is a few kilobytes).
MAX_PASTE = 1 << 20


def _urlopen(url: str, timeout: float) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "pikapro (pikapro team pool loader)"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        body = r.read(MAX_PASTE + 1)
    if len(body) > MAX_PASTE:
        raise PokepasteError(f"{url}: larger than {MAX_PASTE} bytes")
    return body


def fetch_paste(
    url: str,
    cache_dir: PathLike,
    *,
    timeout: float = 10.0,
    fetch: bool = True,
    opener: Optional[Opener] = None,
) -> str:
    """A paste's team text: from ``cache_dir/<id>.txt`` if cached, else
    fetched from ``https://pokepast.es/<id>/raw`` (with ``timeout``
    seconds) and cached. With ``fetch=False`` a missing paste is an error
    instead. ``opener(url, timeout) -> bytes`` replaces the HTTP client (for
    tests and proxies). Raises :class:`PokepasteError` (also for a line
    that isn't a pokepaste link)."""
    try:
        pid = paste_id(url)
    except ValueError as e:
        raise PokepasteError(str(e)) from None
    cache = Path(cache_dir).expanduser()
    path = cache / f"{pid}.txt"
    if path.exists():
        return path.read_text(encoding="utf-8")
    if not fetch:
        raise PokepasteError(f"{url}: not in the cache ({path}) and fetching is off")
    raw = f"https://pokepast.es/{pid}/raw"
    try:
        body = (opener or _urlopen)(raw, timeout)
    except PokepasteError:
        raise
    except (urllib.error.URLError, OSError, TimeoutError, ValueError) as e:
        raise PokepasteError(f"{url}: fetching {raw} failed ({e}); check the link and your connection") from e
    if len(body) > MAX_PASTE:
        raise PokepasteError(f"{url}: larger than {MAX_PASTE} bytes")
    text = _normalize(body.decode("utf-8", errors="replace"))
    if not text.strip() or text.lstrip().startswith("<"):
        raise PokepasteError(f"{url}: {raw} returned no team text")
    cache.mkdir(parents=True, exist_ok=True)
    # A temporary file of its own, so that loaders running at once don't
    # write into each other's.
    fd, tmp = tempfile.mkstemp(prefix=f"{pid}.", suffix=".tmp", dir=cache)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
        os.replace(tmp, path)
    except BaseException:
        Path(tmp).unlink(missing_ok=True)
        raise
    return text


def from_pokepaste_list(
    path: PathLike,
    cache_dir: PathLike,
    format: str = "championsvgc",
    *,
    timeout: float = 10.0,
    fetch: bool = True,
    opener: Optional[Opener] = None,
) -> TeamPool:
    """A tournament team pool from a pokepaste list (see the module docs):
    each paste fetched once into ``cache_dir``, then imported and validated
    for ``format``. The pool's ``labels`` are the links; ``rejected`` lists
    ``(index, link, problems)`` with problems in Showdown's words."""
    urls = read_pokepaste_list(path)
    texts = []
    for k, u in enumerate(urls, 1):
        try:
            texts.append(fetch_paste(u, cache_dir, timeout=timeout, fetch=fetch, opener=opener))
        except PokepasteError as e:
            raise PokepasteError(f"{path}, link {k}: {e}") from e.__cause__
    return TeamPool(texts, format, urls)


def from_export_texts(texts: Sequence[str], format: str, labels: Optional[Sequence[str]] = None) -> TeamPool:
    """A pool from team texts (Showdown's export or packed format)."""
    return TeamPool([_normalize(t) for t in texts], format, None if labels is None else list(labels))


def from_dir(directory: PathLike, format: str, pattern: str = "*.txt") -> TeamPool:
    """A pool from the files matching ``pattern`` in ``directory`` (one team
    per file, in name order; labels are the file names)."""
    files = sorted(Path(directory).expanduser().glob(pattern))
    return TeamPool([_normalize(f.read_text(encoding="utf-8")) for f in files], format, [f.name for f in files])


def split_teams(text: str) -> List[str]:
    """Several export teams in one text, split on lines ``=== ... ===``
    (Showdown's teambuilder backup format); one team if there are none."""
    parts: List[List[str]] = []
    for line in text.splitlines():
        if line.startswith("===") and line.rstrip().endswith("==="):
            parts.append([])
        elif parts:
            parts[-1].append(line)
    if not parts:
        return [text]
    return ["\n".join(lines).strip() + "\n" for lines in parts if "".join(lines).strip()]
