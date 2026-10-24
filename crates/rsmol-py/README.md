# rsmol (Python)

Headless, scriptable rsmol with a PyMOL-style `cmd` API. Everything runs on the same engine as
the rsmol desktop app; `cmd.save("x.rsmol")` produces a session the app opens as is.

```python
from rsmol import cmd

cmd.fetch("1hsg")
cmd.select("pocket", "byres (organic around 5) and polymer")
cmd.show("sticks", "pocket and not name N+C+O")
cmd.color("byelement", "pocket")
d = cmd.distance("h1", "1hsg and chain A and resi 25 and name OD1", "resn MK1 and name O2")
cmd.zoom("organic")
cmd.png("pocket.png", 1600, 1000)
cmd.save("pocket.rsmol")
```

Build from source with `maturin develop --release` inside a virtual environment.
