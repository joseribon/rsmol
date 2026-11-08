# rsmol

A molecular viewer in the spirit of PyMOL, rewritten from scratch in Rust: PyMOL's selection and
command language, with a modern interface and a native GPU renderer (Metal, Vulkan or DirectX 12
through [wgpu](https://wgpu.rs)). One app, the same on macOS, Linux and Windows.

- Cartoon, sticks, spheres and molecular surfaces, with ambient occlusion, outlines and fog
- PyMOL selections (`byres (organic around 5) and polymer`) with live feedback as you type
- Measurements, alignment, undo/redo and self-contained `.rsmol` session files
- Secondary structure computed when files lack it (e.g. AlphaFold and other predicted models)
- Python scripting: `from rsmol import cmd`, against the open window or headless

## Install

On macOS (Apple silicon or Intel) and Linux (x86_64 or aarch64, glibc 2.35+):

```bash
curl -fsSL https://raw.githubusercontent.com/joseribon/rsmol/main/install.sh | sh
```

This puts `rsmol.app` in `/Applications` on macOS, and the app with its menu entry and icon under
`~/.local` on Linux, plus an `rsmol` command in `~/.local/bin`. Add `-s -- --version 0.2.1` after
`sh` for a specific release, or `-s -- --uninstall` to remove it. The archives are also on the
[releases page](https://github.com/joseribon/rsmol/releases). On Windows, build from source as
below.

## Build and run the desktop app

You need [Rust](https://rustup.rs) (stable) and a few system libraries, depending on the system.

### 1. Prerequisites

**macOS**

```bash
xcode-select --install                 # compiler and SDK, if not installed yet
brew install rustup && rustup-init -y  # or the installer from rustup.rs
```

**Fedora**

```bash
sudo dnf install gtk3-devel fontconfig-devel libxkbcommon-devel wayland-devel git
sudo dnf group install "c-development"
sudo dnf install rustup && rustup-init -y && source ~/.cargo/env
```

**Ubuntu / Debian**

```bash
sudo apt install build-essential pkg-config git \
  libgtk-3-dev libfontconfig1-dev libxkbcommon-dev libwayland-dev
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y && source ~/.cargo/env
```

**Arch / CachyOS**

```bash
sudo pacman -S --needed base-devel git gtk3 fontconfig libxkbcommon wayland rustup
rustup default stable
```

**Windows**

Install the [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
with the "Desktop development with C++" workload, then [rustup](https://rustup.rs) and
[Git](https://git-scm.com).

### 2. Build

```bash
git clone https://github.com/joseribon/rsmol
cd rsmol
cargo run --release -p rsmol-desktop
```

The first build downloads and compiles the dependencies and takes a few minutes; later builds
are quick. The binary ends up at `target/release/rsmol-desktop` (`.exe` on Windows) and can be
copied anywhere.

Open something at start by passing a structure, session or script:

```bash
cargo run --release -p rsmol-desktop -- path/to/1hsg.cif
./target/release/rsmol-desktop session.rsmol
```

## Using the app

Drop a file on the window, press ⌘O / Ctrl+O, or type in the command bar at the bottom.

| Action | How |
| --- | --- |
| Rotate / pan / zoom | drag / shift-drag or right-drag / scroll |
| Select a residue | click (shift-click adds) |
| Zoom to a residue | double-click (on empty space: reset the view) |
| Measure a distance | M, then click two atoms; Esc to finish |
| Undo / redo | ⌘Z / ⇧⌘Z (Ctrl on Linux and Windows) |
| Save the session | ⌘S / Ctrl+S |
| Previous command | ↑ in the command bar |

Every button runs a command and shows it in the history, so the panel doubles as a way to learn
the syntax. Commands follow PyMOL:

```text
fetch 1hsg                                  download from the RCSB PDB (cached)
select pocket, byres (organic around 5) and polymer
show sticks, pocket                         also: hide, as; spheres, cartoon, surface
color byelement, pocket                     schemes: bychain, byss, bfactor, default; or a name (teal, coral, …)
zoom organic
distance index 120, index 870               also: angle, dihedral
align 3phv, 1hsg                            superpose one object onto another
alter chain A, b=0
dss                                         recompute secondary structure
save pocket.pdb, pocket                     .rsmol (session), .pdb or .cif
run analysis.py                             a Python script against the open session
python print(cmd.count_atoms("organic"))    inline Python; `cmd` is imported
undo; redo
```

Separate several commands with `;`.

### Python scripting

`run script.py`, or opening a `.py` file, runs it with your Python 3 against the live window:
the viewport and panels update as it goes.

```python
from rsmol import cmd

cmd.fetch("1hsg")
cmd.select("pocket", "byres (organic around 5) and polymer")
cmd.show("sticks", "pocket")
print(cmd.count_atoms("pocket"))
```

The app finds Python in the usual places (Homebrew, `/usr/local/bin`, `/usr/bin`); set
`RSMOL_PYTHON` to use another interpreter. The `rsmol` package needs no install for this. In-app
scripting works on macOS and Linux for now.

The same API runs headless, or with its own viewer window, from the Python package:
`pip install rsmol` (see [crates/rsmol-py](crates/rsmol-py/README.md)).

## The repository

| Crate | What it is |
| --- | --- |
| [`rsmol-core`](crates/rsmol-core) | Data model, PDB/mmCIF reading and writing, selections, the command language, sessions. No GPU or UI. |
| [`rsmol-render`](crates/rsmol-render) | The wgpu renderer. |
| [`rsmol-desktop`](crates/rsmol-desktop) | The desktop app: a [WGPUI](https://github.com/Muktidaya/wgpui) interface around the renderer. |
| [`rsmol-viewer`](crates/rsmol-viewer) | A bare viewer window, also used to render screenshots offscreen. |
| [`rsmol-py`](crates/rsmol-py) | The Python package (`pip install rsmol`). |

Development commands:

```bash
cargo test --workspace                             # tests
cargo clippy --workspace --all-targets             # lints
cargo run --release -p rsmol-viewer -- 1hsg.cif --cmd "show surface, polymer" --screenshot out.png 1600x1000
RUST_LOG=info cargo run -p rsmol-desktop           # verbose logging
```

## Troubleshooting

- **`pkg-config` cannot find `gtk+-3.0`, `fontconfig` or `xkbcommon`** (Linux): the development
  packages from step 1 are missing. If they are installed, check that `which pkg-config` is the
  system one (`/usr/bin/pkg-config`) rather than one from Homebrew or conda.
- **`cannot find value ENOATTR in crate libc`** (Linux): the lock file's `libc` pin was
  bypassed; run `cargo update -p libc --precise 0.2.189`.
- **The window stays blank**: windows completely covered by others are not drawn; bring it to
  the front or move it. If it stays blank, run with `RUST_LOG=info` and open an issue with the
  output.
- **`could not start python3`** when running a script: install Python 3 or set `RSMOL_PYTHON`.

## Acknowledgements

rsmol owes its command and selection language, and much of its spirit, to
[PyMOL](https://pymol.org), created by Warren DeLano and maintained by Schrödinger.

rsmol is an independent project: it is not affiliated with, endorsed by or sponsored by
Schrödinger, and contains no PyMOL source code. PyMOL is a trademark of Schrödinger, LLC.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in
rsmol by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
