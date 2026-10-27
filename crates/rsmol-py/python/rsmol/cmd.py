"""PyMOL-style commands on a module-level session.

Function names and argument order follow PyMOL's `cmd`, so most scripts port by changing the
import. Every command goes through rsmol's command language (see `do`), the same one the desktop
app's command bar uses. Selections use PyMOL syntax.

Run from the rsmol app (`run script.py`), the session is the app's own: the window updates as
the script runs.
"""

from __future__ import annotations

import os
from typing import Any, NamedTuple


def _new_session():
    socket_path = os.environ.get("RSMOL_APP_SOCKET")
    if socket_path:
        from ._remote import RemoteSession

        return RemoteSession(socket_path)
    from ._native import Session

    return Session()


_session = _new_session()


def in_app() -> bool:
    """Whether this script is driving the rsmol app (rather than a headless session)."""
    return "RSMOL_APP_SOCKET" in os.environ


class AlignResult(NamedTuple):
    """Result of `align`. The first three fields match PyMOL's return list."""

    rmsd: float
    aligned: int
    cycles: int
    paired: int


def get_session():
    """The session these functions act on (a native Session, or the app's)."""
    return _session


def reinitialize() -> None:
    """Discards everything: objects, selections, measurements and the view."""
    global _session
    if in_app():
        _session.do("delete all")
        _session.reset()
    else:
        _session = _new_session()


# Commands that touch files or the network, which the core command language leaves to front ends
# (the desktop app handles them in its command bar; here they map onto the functions below).
_FILE_COMMANDS = ("fetch", "load", "save", "png", "run")


def do(command: str) -> str:
    """Runs a command line (several commands may be separated by ';') and returns its message.

    Besides the core commands (select, show, color, align, …) this understands `fetch <id>`,
    `load <file>[, name]`, `save <file>[, selection]`, `png <file>[, width, height]` and
    `run <script.py>`, as the app's command bar does."""
    parts = [p.strip() for p in command.split(";") if p.strip()]
    if not any(p.split(maxsplit=1)[0].lower() in _FILE_COMMANDS for p in parts):
        # One call keeps a multi-command line a single undo step.
        return _session.do(command)
    return "\n".join(m for m in (_do_one(p) for p in parts) if m)


def _do_one(part: str) -> str:
    word, _, rest = part.partition(" ")
    args = [a.strip() for a in rest.split(",")] if rest.strip() else []
    verb = word.lower()
    if verb not in _FILE_COMMANDS:
        return _session.do(part)
    if not args:
        raise ValueError(f"{verb} needs a {'PDB ID' if verb == 'fetch' else 'file name'}")
    if verb == "fetch":
        return f"fetched {fetch(args[0], args[1] if len(args) > 1 else None)}"
    if verb == "load":
        return f"loaded {args[0]} as {load(args[0], args[1] if len(args) > 1 else None)}"
    if verb == "save":
        save(args[0], args[1] if len(args) > 1 else "all")
        return f"saved {args[0]}"
    if verb == "png":
        size = [int(a) for a in args[1:3]]
        png(args[0], *size)
        return f"wrote {args[0]}"
    import runpy

    runpy.run_path(os.path.expanduser(args[0]), run_name="__main__")
    return f"ran {args[0]}"


def _run(command: str) -> dict[str, Any]:
    return _session.run(command)


# --- loading and saving ----------------------------------------------------------------------


def load(filename: str, object: str | None = None) -> str:
    """Loads a structure (.cif, .pdb, optionally .gz) as a new object, or a .rsmol session.
    Returns the object name."""
    return _session.load(str(filename), object)


def fetch(code: str, name: str | None = None) -> str:
    """Downloads a PDB entry from RCSB (cached on disk) and loads it. Returns the object name."""
    return _session.fetch(code, name or None)


def save(filename: str, selection: str = "all") -> None:
    """Saves by extension: .rsmol (a session the rsmol app opens with display, selections and
    view), or .pdb / .cif with the selected atoms."""
    path = str(filename)
    if path.lower().endswith(".rsmol"):
        if selection != "all":
            raise ValueError("sessions always hold everything; omit the selection")
        _session.save(path)
    else:
        _session.export(path, selection)


def png(filename: str, width: int = 1600, height: int = 1000, dpi: float = -1, ray: int = 0) -> None:
    """Renders the current view to a PNG. `dpi` and `ray` are accepted for PyMOL compatibility;
    rsmol always renders with ambient occlusion and outlines."""
    _session.png(str(filename), int(width), int(height))


def get_names() -> list[str]:
    """Object names in load order."""
    return _session.get_names()


def delete(name: str) -> None:
    """Deletes an object, measurement or named selection; 'all' deletes everything."""
    do(f"delete {name}")


def enable(name: str = "all") -> None:
    do(f"enable {name}")


def disable(name: str = "all") -> None:
    do(f"disable {name}")


def dss(selection: str = "all") -> None:
    """Assigns secondary structure (helix, sheet, loop) from the backbone geometry, as PyMOL's
    dss does. Files without HELIX/SHEET records or struct_conf tables, such as most predicted
    models, get this automatically when loaded."""
    do("dss" if selection == "all" else f"dss {selection}")


# --- selections ------------------------------------------------------------------------------


def select(name: str, selection: str | None = None) -> int:
    """`select(name, selection)` stores a named selection; `select(selection)` stores it as
    'sele'. Returns the number of atoms selected."""
    if selection is None:
        name, selection = "sele", name
    return _run(f"select {name}, {selection}")["atoms"]


def deselect() -> None:
    do("deselect")


def count_atoms(selection: str = "all") -> int:
    return _session.count_atoms(selection)


def get_coords(selection: str = "all"):
    """Coordinates of the selected atoms as an (n, 3) array: numpy if installed, else lists."""
    coords = _session.get_coords(selection)
    try:
        import numpy

        return numpy.array(coords, dtype=numpy.float32).reshape(-1, 3)
    except ImportError:
        return coords


def get_atoms(selection: str = "all") -> list[dict[str, Any]]:
    """One dict per atom with index, object, chain, segi, resn, resi, name, elem, alt, b, q,
    x, y, z and hetatm."""
    return _session.get_atoms(selection)


_ALTERABLE = ("name", "resn", "resi", "chain", "segi", "elem", "alt", "b", "q")


def alter(selection: str, expression: str, space: dict[str, Any] | None = None) -> int:
    """Runs `expression` per atom, as `iterate` does, and stores any changed name, resn, resi,
    chain, segi, elem, alt, b or q — e.g. `alter("chain A", "chain = 'B'")` or
    `alter("all", "b = 0.0")`. Residue and chain fields apply to the whole residue/chain.
    One undo step. Returns the number of atoms changed."""
    code = compile(expression, "<alter>", "exec")
    namespace = space if space is not None else {}
    changes: list[tuple[int, str, str]] = []
    changed_atoms = 0
    for atom in get_atoms(selection):
        local = dict(atom)
        local["model"] = atom["object"]
        exec(code, namespace, local)
        before = len(changes)
        for field in _ALTERABLE:
            if local[field] != atom[field]:
                changes.append((atom["index"], field, str(local[field])))
        changed_atoms += len(changes) > before
    if changes:
        _session.alter(changes)
    return changed_atoms


def iterate(selection: str, expression: str, space: dict[str, Any] | None = None) -> int:
    """Runs `expression` once per atom with the atom's properties as variables (name, resn,
    resi, chain, segi, elem, alt, b, q, x, y, z, index, model, hetatm), as PyMOL does.
    `space` is the global namespace (for collecting results). Returns the atom count."""
    code = compile(expression, "<iterate>", "exec")
    namespace = space if space is not None else {}
    atoms = get_atoms(selection)
    for atom in atoms:
        local = dict(atom)
        local["model"] = atom["object"]
        exec(code, namespace, local)
    return len(atoms)


# --- display ---------------------------------------------------------------------------------


def show(representation: str = "everything", selection: str = "all") -> None:
    do(f"show {representation}, {selection}")


def hide(representation: str = "everything", selection: str = "all") -> None:
    do(f"hide {representation}, {selection}")


def show_as(representation: str, selection: str = "all") -> None:
    do(f"as {representation}, {selection}")


def color(color: str, selection: str = "all") -> None:
    """A color name, #rrggbb, or a scheme: bychain, byelement, byss, bfactor, default."""
    do(f"color {color}, {selection}")


def zoom(selection: str = "all") -> None:
    do("zoom" if selection == "all" else f"zoom {selection}")


def turn(axis: str, angle: float) -> None:
    """Rotates the view about the screen 'x', 'y' or 'z' axis by `angle` degrees."""
    _session.turn(axis, float(angle))


def reset() -> None:
    """Back to the default view of everything."""
    _session.reset()


# --- measurement and superposition -----------------------------------------------------------


def _measure(kind: str, name: str | None, selections: tuple[str, ...]) -> float:
    args = ", ".join(([name] if name else []) + list(selections))
    return _run(f"{kind} {args}")["value"]


def distance(name: str | None = None, selection1: str = "", selection2: str = "") -> float:
    """Distance in Å between two single-atom selections; `name` may be None for an automatic one."""
    return _measure("distance", name, (selection1, selection2))


def angle(name: str | None = None, selection1: str = "", selection2: str = "", selection3: str = "") -> float:
    """Angle in degrees at `selection2`."""
    return _measure("angle", name, (selection1, selection2, selection3))


def dihedral(
    name: str | None = None, selection1: str = "", selection2: str = "", selection3: str = "", selection4: str = ""
) -> float:
    """Torsion in degrees about the `selection2`–`selection3` bond."""
    return _measure("dihedral", name, (selection1, selection2, selection3, selection4))


def get_measurements() -> list[dict[str, Any]]:
    return _session.get_measurements()


def align(mobile: str, target: str) -> AlignResult:
    """Superposes the object containing `mobile` onto `target` by sequence alignment and an
    iterative fit with outlier rejection. Moves the mobile object."""
    r = _run(f"align {mobile}, {target}")
    return AlignResult(rmsd=r["rmsd"], aligned=int(r["aligned"]), cycles=int(r["cycles"]), paired=int(r["paired"]))


super = align  # noqa: A001 - PyMOL's name for structure-based superposition


def quit() -> None:  # noqa: A001 - PyMOL's name
    """Closes the rsmol window (ending the `rsmol` command), or exits a headless script."""
    if not _session.quit():
        raise SystemExit(0)


def undo() -> None:
    do("undo")


def redo() -> None:
    do("redo")
