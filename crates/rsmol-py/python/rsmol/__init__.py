"""rsmol: fast, modern molecular visualization with a PyMOL-style scripting API.

`rsmol.cmd` mirrors PyMOL's `cmd` module on a headless session; `rsmol.Session` is the
underlying object for working with several independent sessions.
"""

from ._native import Session, __version__
from . import cmd

__all__ = ["Session", "cmd", "__version__"]
