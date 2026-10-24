// Swatch colors for the UI. Names and values mirror rsmol-core's color.rs so a swatch looks
// exactly like what `color <name>` produces.
export const SWATCHES: { name: string; hex: string }[] = [
  { name: "teal", hex: "#60baa8" },
  { name: "sky", hex: "#76b0d6" },
  { name: "lavender", hex: "#b4a0e6" },
  { name: "rose", hex: "#e26e8c" },
  { name: "coral", hex: "#f0806e" },
  { name: "amber", hex: "#f5b43c" },
  { name: "mint", hex: "#8cd6b4" },
  { name: "wheat", hex: "#ecd4a0" },
  { name: "slate", hex: "#7080a0" },
  { name: "white", hex: "#eceef2" },
];

export const SCHEMES: { label: string; command: string; hint: string }[] = [
  { label: "Chain", command: "bychain", hint: "One color per chain" },
  { label: "Element", command: "byelement", hint: "Heteroatoms by element, carbons unchanged" },
  { label: "Structure", command: "byss", hint: "Helix, sheet and loop" },
  { label: "B-factor", command: "bfactor", hint: "Blue (ordered) to red (mobile)" },
  { label: "Default", command: "default", hint: "Chain carbons, element heteroatoms" },
];
