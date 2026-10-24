use std::fmt;
use std::ops::Range;

/// A parsed selection expression. `Display` prints canonical PyMOL syntax, fully parenthesized.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    All,
    None,
    /// A named selection or object; `span` locates it in the input for "no such name" errors.
    Named { name: String, span: Range<usize> },
    /// Text properties matched against `+`-separated patterns; `*` is a wildcard.
    Property { prop: Property, patterns: Vec<String> },
    Resi(Vec<ResiItem>),
    Index { kind: IndexKind, ranges: Vec<(i64, i64)> },
    Compare { field: NumericField, op: CmpOp, value: f64 },
    Class(Class),
    State(u32),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
    /// Atoms matching `expr` that also match `of` by identity (`in`) or by name/residue (`like`).
    Match { kind: MatchKind, expr: Box<Expr>, of: Box<Expr> },
    /// Expands a selection to complete residues, chains, …
    Group { by: Grouping, expr: Box<Expr> },
    /// Atoms of `expr` within (or beyond) `distance` Å of `of`.
    Distance { kind: DistanceKind, expr: Box<Expr>, distance: f32, of: Box<Expr> },
    /// `expr around N` / `expr expand N`: atoms within N Å of `expr`, excluding / including it.
    Around { expr: Box<Expr>, distance: f32, include_self: bool },
    /// `expr extend N`: grows the selection by N bonds.
    Extend { expr: Box<Expr>, bonds: u32 },
    /// Atoms bonded to `expr`, excluding (`neighbor`) or including (`bound_to`) it.
    Bonded { expr: Box<Expr>, include_self: bool },
    First(Box<Expr>),
    Last(Box<Expr>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    Name,
    Resn,
    Chain,
    Segi,
    Elem,
    Alt,
    Ss,
    Model,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexKind {
    /// File serial number.
    Id,
    /// 1-based position within the object.
    Index,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumericField {
    B,
    Q,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Lt,
    Le,
    Eq,
    Ge,
    Gt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Hetatm,
    Hydrogens,
    Polymer,
    Protein,
    Nucleic,
    Organic,
    Inorganic,
    Solvent,
    Metals,
    Backbone,
    Sidechain,
    Visible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchKind {
    In,
    Like,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grouping {
    Residue,
    Chain,
    Segment,
    Object,
    Molecule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DistanceKind {
    Within,
    Beyond,
    /// Like `within`, but excludes the atoms of `of` itself.
    NearTo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResId {
    pub seq: i32,
    pub ins: Option<char>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResiItem {
    Single(ResId),
    /// Inclusive range.
    Range(ResId, ResId),
}

impl Property {
    pub fn keyword(self) -> &'static str {
        match self {
            Property::Name => "name",
            Property::Resn => "resn",
            Property::Chain => "chain",
            Property::Segi => "segi",
            Property::Elem => "elem",
            Property::Alt => "alt",
            Property::Ss => "ss",
            Property::Model => "model",
        }
    }
}

impl Class {
    pub fn keyword(self) -> &'static str {
        match self {
            Class::Hetatm => "hetatm",
            Class::Hydrogens => "hydrogens",
            Class::Polymer => "polymer",
            Class::Protein => "polymer.protein",
            Class::Nucleic => "polymer.nucleic",
            Class::Organic => "organic",
            Class::Inorganic => "inorganic",
            Class::Solvent => "solvent",
            Class::Metals => "metals",
            Class::Backbone => "backbone",
            Class::Sidechain => "sidechain",
            Class::Visible => "visible",
        }
    }
}

impl Grouping {
    pub fn keyword(self) -> &'static str {
        match self {
            Grouping::Residue => "byres",
            Grouping::Chain => "bychain",
            Grouping::Segment => "bysegi",
            Grouping::Object => "byobject",
            Grouping::Molecule => "bymolecule",
        }
    }
}

impl fmt::Display for ResId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Negative numbers are escaped so they cannot be read back as a range.
        if self.seq < 0 {
            write!(f, "\\")?;
        }
        write!(f, "{}", self.seq)?;
        if let Some(c) = self.ins {
            write!(f, "{c}")?;
        }
        Ok(())
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let join = |f: &mut fmt::Formatter, items: &mut dyn Iterator<Item = String>| {
            f.write_str(&items.collect::<Vec<_>>().join("+"))
        };
        match self {
            Expr::All => f.write_str("all"),
            Expr::None => f.write_str("none"),
            Expr::Named { name, .. } => f.write_str(name),
            Expr::Property { prop, patterns } => {
                write!(f, "{} ", prop.keyword())?;
                let needs_quotes = |p: &str| p.is_empty() || p.contains(|c: char| c.is_whitespace() || "()&|!<>=+".contains(c));
                join(f, &mut patterns.iter().map(|p| if needs_quotes(p) { format!("\"{p}\"") } else { p.clone() }))
            }
            Expr::Resi(items) => {
                f.write_str("resi ")?;
                join(f, &mut items.iter().map(|i| match i {
                    ResiItem::Single(r) => r.to_string(),
                    ResiItem::Range(a, b) => format!("{a}-{b}"),
                }))
            }
            Expr::Index { kind, ranges } => {
                f.write_str(if *kind == IndexKind::Id { "id " } else { "index " })?;
                join(f, &mut ranges.iter().map(|&(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") }))
            }
            Expr::Compare { field, op, value } => {
                let field = if *field == NumericField::B { "b" } else { "q" };
                let op = match op {
                    CmpOp::Lt => "<",
                    CmpOp::Le => "<=",
                    CmpOp::Eq => "=",
                    CmpOp::Ge => ">=",
                    CmpOp::Gt => ">",
                };
                write!(f, "{field} {op} {value}")
            }
            Expr::Class(c) => f.write_str(c.keyword()),
            Expr::State(n) => write!(f, "state {n}"),
            Expr::Not(e) => write!(f, "not {e}"),
            Expr::And(a, b) => write!(f, "({a} and {b})"),
            Expr::Or(a, b) => write!(f, "({a} or {b})"),
            Expr::Match { kind, expr, of } => {
                write!(f, "({expr} {} {of})", if *kind == MatchKind::In { "in" } else { "like" })
            }
            Expr::Group { by, expr } => write!(f, "{} {expr}", by.keyword()),
            Expr::Distance { kind, expr, distance, of } => {
                let kw = match kind {
                    DistanceKind::Within => "within",
                    DistanceKind::Beyond => "beyond",
                    DistanceKind::NearTo => "near_to",
                };
                write!(f, "({expr} {kw} {distance} of {of})")
            }
            Expr::Around { expr, distance, include_self } => {
                write!(f, "({expr} {} {distance})", if *include_self { "expand" } else { "around" })
            }
            Expr::Extend { expr, bonds } => write!(f, "({expr} extend {bonds})"),
            Expr::Bonded { expr, include_self } => {
                write!(f, "{} {expr}", if *include_self { "bound_to" } else { "neighbor" })
            }
            Expr::First(e) => write!(f, "first {e}"),
            Expr::Last(e) => write!(f, "last {e}"),
        }
    }
}
