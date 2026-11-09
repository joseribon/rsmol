"""rsmol: fast, modern molecular visualization with a PyMOL-style scripting API.

`rsmol.cmd` mirrors PyMOL's `cmd` module. Run with plain Python it uses a headless session
(`rsmol.Session`); run from the rsmol app (`run script.py`) it drives the open window.
"""

try:
    from ._native import Session, __version__
except ImportError:
    # The app ships only the pure-Python package; scripts it runs talk to it over a socket.
    Session = None  # type: ignore[assignment,misc]
    __version__ = "0.3.0"

from . import cmd

__all__ = ["Session", "cmd", "__version__"]
