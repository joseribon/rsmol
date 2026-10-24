use std::fmt;

/// Chemical element, stored as its atomic number. `Element(0)` is "unknown".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Element(pub u8);

const SYMBOLS: [&str; 119] = [
    "X", "H", "He", "Li", "Be", "B", "C", "N", "O", "F", "Ne", "Na", "Mg", "Al", "Si", "P", "S",
    "Cl", "Ar", "K", "Ca", "Sc", "Ti", "V", "Cr", "Mn", "Fe", "Co", "Ni", "Cu", "Zn", "Ga", "Ge",
    "As", "Se", "Br", "Kr", "Rb", "Sr", "Y", "Zr", "Nb", "Mo", "Tc", "Ru", "Rh", "Pd", "Ag", "Cd",
    "In", "Sn", "Sb", "Te", "I", "Xe", "Cs", "Ba", "La", "Ce", "Pr", "Nd", "Pm", "Sm", "Eu", "Gd",
    "Tb", "Dy", "Ho", "Er", "Tm", "Yb", "Lu", "Hf", "Ta", "W", "Re", "Os", "Ir", "Pt", "Au", "Hg",
    "Tl", "Pb", "Bi", "Po", "At", "Rn", "Fr", "Ra", "Ac", "Th", "Pa", "U", "Np", "Pu", "Am", "Cm",
    "Bk", "Cf", "Es", "Fm", "Md", "No", "Lr", "Rf", "Db", "Sg", "Bh", "Hs", "Mt", "Ds", "Rg", "Cn",
    "Nh", "Fl", "Mc", "Lv", "Ts", "Og",
];

impl Element {
    pub const UNKNOWN: Element = Element(0);
    pub const H: Element = Element(1);
    pub const C: Element = Element(6);
    pub const N: Element = Element(7);
    pub const O: Element = Element(8);
    pub const S: Element = Element(16);

    /// Case-insensitive lookup by symbol ("FE", "fe" and "Fe" all match). Deuterium maps to H.
    pub fn from_symbol(symbol: &str) -> Option<Element> {
        let s = symbol.trim();
        if s.eq_ignore_ascii_case("D") {
            return Some(Element::H);
        }
        SYMBOLS
            .iter()
            .skip(1)
            .position(|sym| sym.eq_ignore_ascii_case(s))
            .map(|i| Element(i as u8 + 1))
    }

    pub fn symbol(self) -> &'static str {
        SYMBOLS.get(self.0 as usize).copied().unwrap_or("X")
    }

    pub fn atomic_number(self) -> u8 {
        self.0
    }
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.symbol())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_roundtrip() {
        for z in 1..=118u8 {
            let e = Element(z);
            assert_eq!(Element::from_symbol(e.symbol()), Some(e));
        }
    }

    #[test]
    fn case_insensitive_and_deuterium() {
        assert_eq!(Element::from_symbol("FE"), Some(Element(26)));
        assert_eq!(Element::from_symbol(" se"), Some(Element(34)));
        assert_eq!(Element::from_symbol("D"), Some(Element::H));
        assert_eq!(Element::from_symbol("Qq"), None);
    }
}
