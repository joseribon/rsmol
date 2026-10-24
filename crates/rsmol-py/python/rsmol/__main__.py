"""The `rsmol` command: open structures in a viewer window, run scripts and commands.

    rsmol 1hsg.cif apo.pdb          open a window with these loaded
    rsmol session.rsmol             restore a saved session
    rsmol script.py                 open a window and run a script that drives it
    rsmol -d "fetch 1hsg; show sticks, organic"
    rsmol -c script.py              no window: run headless and exit (like `pymol -c`)

Structures and sessions load before the window opens; `-d` commands and scripts then run on a
background thread, so their effects appear live. The window closes the program.
"""

from __future__ import annotations

import argparse
import os
import runpy
import sys
import threading
import traceback

from . import __version__, cmd

SCRIPT_EXTENSIONS = (".py",)


def _parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="rsmol",
        description="Fast, modern molecular visualization with a PyMOL-style command language.",
        epilog="In the window: drag to rotate, right-drag to pan, scroll to zoom, click to select "
        "a residue, 1-5 for display presets, R to reset the view, Esc to deselect.",
    )
    p.add_argument("files", nargs="*", help="structures (.cif, .pdb, optionally .gz), .rsmol sessions or .py scripts")
    p.add_argument("-d", "--do", action="append", default=[], metavar="COMMANDS",
                   help="command line to run after loading, e.g. \"show sticks, organic\" (repeatable)")
    p.add_argument("-c", "--no-window", action="store_true", help="run headless and exit")
    p.add_argument("-q", "--quiet", action="store_true", help="print only errors")
    p.add_argument("--version", action="version", version=f"rsmol {__version__}")
    return p


def _say(quiet: bool, text: str) -> None:
    if not quiet:
        print(text, flush=True)


def _run_steps(steps: list[tuple[str, str]], quiet: bool) -> bool:
    """Runs (kind, value) steps in order; reports failures and keeps going. True if all worked."""
    ok = True
    for kind, value in steps:
        try:
            if kind == "script":
                _say(quiet, f"rsmol> run {value}")
                runpy.run_path(value, run_name="__main__")
            else:
                _say(quiet, f"rsmol> {value}")
                _say(quiet, "  " + cmd.do(value))
        except SystemExit as e:
            if e.code not in (None, 0):
                ok = False
        except Exception as e:  # report and continue, as PyMOL does
            ok = False
            if isinstance(e, (ValueError, OSError)) and kind == "commands":
                # Indent every line alike, so carets stay under the text they point at.
                print("  error:", file=sys.stderr)
                for line in str(e).splitlines():
                    print(f"    {line}", file=sys.stderr)
            else:
                traceback.print_exc()
    return ok


def main(argv: list[str] | None = None) -> int:
    args = _parser().parse_args(argv)
    steps: list[tuple[str, str]] = []
    load_ok = True
    for path in args.files:
        if path.lower().endswith(SCRIPT_EXTENSIONS):
            steps.append(("script", os.path.abspath(path)))
            continue
        try:
            name = cmd.load(path)
            _say(args.quiet, f"loaded {path} as {name}")
        except (ValueError, OSError) as e:
            print(f"rsmol: {e}", file=sys.stderr)
            load_ok = False
    steps += [("commands", line) for line in args.do]

    if args.no_window:
        return 0 if _run_steps(steps, args.quiet) and load_ok else 1

    from ._native import run_viewer

    # The window needs the main thread; scripts and commands run beside it and update it live.
    if steps:
        threading.Thread(target=_run_steps, args=(steps, args.quiet), daemon=True).start()
    names = cmd.get_names()
    title = "rsmol — " + " + ".join(names) if names else "rsmol"
    try:
        run_viewer(cmd.get_session(), title)
    except RuntimeError as e:
        print(f"rsmol: {e}", file=sys.stderr)
        return 1
    return 0 if load_ok else 1


if __name__ == "__main__":
    sys.exit(main())
