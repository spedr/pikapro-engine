"""The extension's type stubs (``pikapro/_pikapro.pyi``) name what the
extension has: every class, function, constant and member the stubs
declare exists.
"""

import re
from pathlib import Path

import pikapro
from pikapro import _pikapro

STUB = Path(pikapro.__file__).with_name("_pikapro.pyi")


def _declared():
    """``(class or None, name)`` for each declaration of the stubs
    (type aliases and protocol methods left out: ``__getattr__`` says to a
    type checker what the extension's attribute lookup does)."""
    out = []
    cls = None
    in_doc = False
    for line in STUB.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        quotes = stripped.count('"""')
        if in_doc:
            in_doc = quotes % 2 == 0
            continue
        if quotes % 2 == 1:
            in_doc = True
            continue
        if quotes:
            continue
        indent = len(line) - len(line.lstrip())
        if indent == 0:
            m = re.match(r"(class|def) ([A-Za-z_]\w*)", line) or re.match(r"()([A-Za-z_]\w*): ", line)
            if m:
                cls = m.group(2) if m.group(1) == "class" else None
                out.append((None, m.group(2)))
        elif indent == 4 and cls is not None:
            m = re.match(r"\s+def ([A-Za-z_]\w*)\(", line) or re.match(r"\s+([A-Za-z_]\w*): ", line)
            if m and not m.group(1).startswith("__"):
                out.append((cls, m.group(1)))
    return out


def test_the_stubs_name_what_the_extension_has():
    entries = _declared()
    assert len(entries) > 150, len(entries)
    missing = []
    for cls, name in entries:
        owner = _pikapro if cls is None else getattr(_pikapro, cls, None)
        present = owner is not None and hasattr(owner, name)
        what = name if cls is None else f"{cls}.{name}"
        if not present:
            missing.append(what)
    assert not missing, f"declared in _pikapro.pyi, not in the extension: {missing}"
