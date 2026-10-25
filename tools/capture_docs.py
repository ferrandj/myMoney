"""pytest plugin: keep a copy of every document an adapter is given while the tests run, to compare readers.

    MYMONEY_CAPTURE=/some/dir python -m pytest -p tools.capture_docs   (from python/, with the repo root on PYTHONPATH)

Each document lands in <dir>/<sha256 of name and content>/<its file name>.
"""

import hashlib
import os

from mymoney.adapters import base

_init = base.SourceDocument.__init__


def _capture(self, filename, data, *args, **kwargs):
    _init(self, filename, data, *args, **kwargs)
    out = os.environ.get("MYMONEY_CAPTURE")
    if out and data:
        key = hashlib.sha256(filename.encode() + b"\0" + data).hexdigest()[:16]
        folder = os.path.join(out, key)
        if not os.path.exists(folder):
            os.makedirs(folder, exist_ok=True)
            safe = os.path.basename(filename) or "document"
            with open(os.path.join(folder, safe), "wb") as fh:
                fh.write(data)


base.SourceDocument.__init__ = _capture
