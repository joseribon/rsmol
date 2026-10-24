"""Tests for rsmol.cmd, run against the core crate's fixture files (no network).

    python -m unittest discover -s crates/rsmol-py/tests
"""

import math
import os
import tempfile
import unittest

from rsmol import cmd

DATA = os.path.join(os.path.dirname(__file__), "..", "..", "rsmol-core", "tests", "data")


class CmdTest(unittest.TestCase):
    def setUp(self):
        cmd.reinitialize()
        self.name = cmd.load(os.path.join(DATA, "traces.pdb"))

    def test_load_and_count(self):
        self.assertEqual(self.name, "traces")
        self.assertEqual(cmd.get_names(), ["traces"])
        self.assertEqual(cmd.count_atoms(), 27)
        self.assertEqual(cmd.count_atoms("polymer.protein"), 20)

    def test_select_forms(self):
        self.assertEqual(cmd.select("cas", "name CA and polymer"), 5)
        self.assertEqual(cmd.select("name CA"), 6)  # one-argument form stores "sele"
        self.assertEqual(cmd.count_atoms("sele and not cas"), 1)

    def test_iterate_and_atoms(self):
        seen = []
        n = cmd.iterate("name CA and polymer", "seen.append((resn, resi, chain))", space={"seen": seen})
        self.assertEqual(n, 5)
        self.assertEqual(seen[0], ("ALA", "1", "A"))
        atom = cmd.get_atoms("index 2")[0]
        self.assertEqual((atom["name"], atom["object"], atom["index"]), ("CA", "traces", 2))

    def test_measurements(self):
        d = cmd.distance("d1", "index 2", "index 6")
        self.assertAlmostEqual(d, 3.8, places=4)
        a = cmd.angle(None, "index 2", "index 6", "index 10")
        self.assertAlmostEqual(a, 180.0, places=3)
        self.assertEqual([m["name"] for m in cmd.get_measurements()], ["d1", "angle01"])

    def test_align_returns_pymol_compatible_tuple(self):
        cmd.load(os.path.join(DATA, "traces.pdb"), "copy")
        cmd.do("disable copy")
        result = cmd.align("copy", "traces")
        self.assertAlmostEqual(result[0], 0.0, places=3)
        self.assertEqual(result.aligned, 7)

    def test_errors_show_the_offending_text(self):
        with self.assertRaises(ValueError) as ctx:
            cmd.select("x", "name CA and nonsense")
        self.assertIn("^^^^^^^^", str(ctx.exception))
        with self.assertRaises(ValueError):
            cmd.show("lines")

    def test_coords_shape(self):
        coords = cmd.get_coords("name CA and polymer")
        self.assertEqual(len(coords), 5)
        self.assertTrue(all(len(c) == 3 for c in coords))

    def test_alter_with_expressions(self):
        n = cmd.alter("polymer.protein", "b = 10.0 * len(name)")
        self.assertEqual(n, 20)
        self.assertEqual(cmd.get_atoms("index 2")[0]["b"], 20.0)  # "CA"
        cmd.alter("resi 1 and polymer.protein", "chain = 'Q'")
        self.assertEqual(cmd.count_atoms("chain Q"), 21)  # the whole chain segment, ion included
        # Unchanged atoms are not reported.
        self.assertEqual(cmd.alter("all", "b = b"), 0)
        cmd.undo()
        self.assertEqual(cmd.count_atoms("chain Q"), 0)
        with self.assertRaises(ValueError):
            cmd.alter("index 1", "elem = 'Qq'")

    def test_export_round_trip(self):
        with tempfile.TemporaryDirectory() as tmp:
            for ext in ("pdb", "cif"):
                path = os.path.join(tmp, f"out.{ext}")
                cmd.save(path, "traces and polymer.protein")
                cmd.load(path, f"copy_{ext}")
                self.assertEqual(cmd.count_atoms(f"copy_{ext}"), 20)
            with self.assertRaises(ValueError):
                cmd.save(os.path.join(tmp, "out.xyz"))

    def test_png_and_session_round_trip(self):
        cmd.show_as("sticks")
        cmd.color("coral", "chain A")
        with tempfile.TemporaryDirectory() as tmp:
            png = os.path.join(tmp, "out.png")
            cmd.png(png, 320, 200)
            with open(png, "rb") as f:
                self.assertEqual(f.read(8), b"\x89PNG\r\n\x1a\n")
            session = os.path.join(tmp, "s.rsmol")
            cmd.save(session)
            cmd.reinitialize()
            cmd.load(session)
            self.assertEqual(cmd.get_names(), ["traces"])
            self.assertEqual(cmd.count_atoms("visible"), 27)
        with self.assertRaises(ValueError):
            cmd.save("out.pse")
        with self.assertRaises(ValueError):
            cmd.save("x.rsmol", "chain A")


if __name__ == "__main__":
    unittest.main()
