//! Recursive-descent parser for PyMOL selection syntax.
//!
//! Precedence, loosest first: `or` < `and` < `in`/`like`/`within`/`beyond`/`near_to` <
//! `around`/`expand`/`extend` (postfix) < `not`/`by*`/`first`/`last`/`neighbor`/`bound_to`
//! (prefix) < terms. Operators of the same level associate to the left.
//!
//! Lexing is done on demand because PyMOL tokens are context-dependent: after `resi`, `10-50+60A`
//! is one value, and after `name`, `O5'` is an atom name rather than a quoted string.

use std::fmt;
use std::ops::Range;

use super::ast::*;

#[derive(Clone, Debug, PartialEq)]
pub struct SelectionError {
    /// Byte range of the offending input.
    pub span: Range<usize>,
    pub message: String,
}

impl SelectionError {
    /// The input with a caret line under the error, for terminals and the command line UI.
    pub fn render(&self, input: &str) -> String {
        let col = input[..self.span.start].chars().count();
        let width = input[self.span.clone()].chars().count().max(1);
        format!("{input}\n{}{} {}", " ".repeat(col), "^".repeat(width), self.message)
    }
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SelectionError {}

pub fn parse(input: &str) -> Result<Expr, SelectionError> {
    let mut p = Parser { src: input, pos: 0, last: None };
    let expr = p.or_expr()?;
    p.skip_ws();
    if let Some((text, span)) = p.peek_token() {
        let message = if text == ")" {
            "unmatched ')'".to_owned()
        } else {
            format!("unexpected '{text}': combine selections with 'and' or 'or'")
        };
        return Err(SelectionError { span, message });
    }
    Ok(expr)
}

/// Characters that end a bare word and form tokens of their own.
const SYMBOLS: &[char] = &['(', ')', '&', '|', '!', '<', '>', '='];

#[derive(Clone, Copy, PartialEq)]
enum Kw {
    And,
    Or,
    Not,
    Of,
    Match(MatchKind),
    Distance(DistanceKind),
    Around { include_self: bool },
    Extend,
    Group(Grouping),
    Bonded { include_self: bool },
    First,
    Last,
    All,
    None,
    Prop(Property),
    Resi,
    Index(IndexKind),
    Numeric(NumericField),
    Class(Class),
    State,
}

fn keyword(word: &str) -> Option<Kw> {
    Some(match word.to_ascii_lowercase().as_str() {
        "and" => Kw::And,
        "or" => Kw::Or,
        "not" => Kw::Not,
        "of" => Kw::Of,
        "in" => Kw::Match(MatchKind::In),
        "like" | "l." => Kw::Match(MatchKind::Like),
        "within" | "w." => Kw::Distance(DistanceKind::Within),
        "beyond" | "be." => Kw::Distance(DistanceKind::Beyond),
        "near_to" | "nto." => Kw::Distance(DistanceKind::NearTo),
        "around" | "a." => Kw::Around { include_self: false },
        "expand" | "x." => Kw::Around { include_self: true },
        "extend" | "xt." => Kw::Extend,
        "byres" | "br." => Kw::Group(Grouping::Residue),
        "bychain" | "bc." => Kw::Group(Grouping::Chain),
        "bysegi" | "bs." => Kw::Group(Grouping::Segment),
        "byobject" | "bo." => Kw::Group(Grouping::Object),
        "bymolecule" | "bm." => Kw::Group(Grouping::Molecule),
        "neighbor" | "nbr." => Kw::Bonded { include_self: false },
        "bound_to" | "bto." => Kw::Bonded { include_self: true },
        "first" => Kw::First,
        "last" => Kw::Last,
        "all" | "*" => Kw::All,
        "none" => Kw::None,
        "name" | "n." => Kw::Prop(Property::Name),
        "resn" | "r." => Kw::Prop(Property::Resn),
        "chain" | "c." => Kw::Prop(Property::Chain),
        "segi" | "s." => Kw::Prop(Property::Segi),
        "elem" | "e." => Kw::Prop(Property::Elem),
        "alt" => Kw::Prop(Property::Alt),
        "ss" => Kw::Prop(Property::Ss),
        "model" | "m." => Kw::Prop(Property::Model),
        "resi" | "i." => Kw::Resi,
        "id" => Kw::Index(IndexKind::Id),
        "index" | "idx." => Kw::Index(IndexKind::Index),
        "b" => Kw::Numeric(NumericField::B),
        "q" => Kw::Numeric(NumericField::Q),
        "hetatm" | "het" => Kw::Class(Class::Hetatm),
        "hydrogens" | "hydro" | "h." => Kw::Class(Class::Hydrogens),
        "polymer" | "pol." => Kw::Class(Class::Polymer),
        "polymer.protein" => Kw::Class(Class::Protein),
        "polymer.nucleic" => Kw::Class(Class::Nucleic),
        "organic" | "org." => Kw::Class(Class::Organic),
        "inorganic" | "ino." => Kw::Class(Class::Inorganic),
        "solvent" | "sol." => Kw::Class(Class::Solvent),
        "metals" => Kw::Class(Class::Metals),
        "backbone" | "bb." => Kw::Class(Class::Backbone),
        "sidechain" | "sc." => Kw::Class(Class::Sidechain),
        "visible" | "v." => Kw::Class(Class::Visible),
        "state" => Kw::State,
        _ => return Option::None,
    })
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
    /// Text of the last consumed token, for "expected X after Y" messages.
    last: Option<&'a str>,
}

impl<'a> Parser<'a> {
    fn or_expr(&mut self) -> Result<Expr, SelectionError> {
        let mut lhs = self.and_expr()?;
        while self.eat_symbol("|") || self.eat_kw(Kw::Or) {
            lhs = Expr::Or(Box::new(lhs), Box::new(self.and_expr()?));
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> Result<Expr, SelectionError> {
        let mut lhs = self.binary_expr()?;
        while self.eat_symbol("&") || self.eat_kw(Kw::And) {
            lhs = Expr::And(Box::new(lhs), Box::new(self.binary_expr()?));
        }
        Ok(lhs)
    }

    fn binary_expr(&mut self) -> Result<Expr, SelectionError> {
        let mut lhs = self.postfix_expr()?;
        loop {
            match self.peek_kw() {
                Some(Kw::Match(kind)) => {
                    self.bump();
                    let of = self.postfix_expr()?;
                    lhs = Expr::Match { kind, expr: Box::new(lhs), of: Box::new(of) };
                }
                Some(Kw::Distance(kind)) => {
                    self.bump();
                    let distance = self.number::<f32>("a distance in Å")?;
                    if !self.eat_kw(Kw::Of) {
                        return Err(self.error_here("expected 'of'"));
                    }
                    let of = self.postfix_expr()?;
                    lhs = Expr::Distance { kind, expr: Box::new(lhs), distance, of: Box::new(of) };
                }
                _ => return Ok(lhs),
            }
        }
    }

    fn postfix_expr(&mut self) -> Result<Expr, SelectionError> {
        let mut expr = self.prefix_expr()?;
        loop {
            match self.peek_kw() {
                Some(Kw::Around { include_self }) => {
                    self.bump();
                    let distance = self.number("a distance in Å")?;
                    expr = Expr::Around { expr: Box::new(expr), distance, include_self };
                }
                Some(Kw::Extend) => {
                    self.bump();
                    let bonds = self.number("a number of bonds")?;
                    expr = Expr::Extend { expr: Box::new(expr), bonds };
                }
                _ => return Ok(expr),
            }
        }
    }

    fn prefix_expr(&mut self) -> Result<Expr, SelectionError> {
        if self.eat_symbol("!") {
            return Ok(Expr::Not(Box::new(self.prefix_expr()?)));
        }
        let boxed = |p: &mut Self| p.prefix_expr().map(Box::new);
        Ok(match self.peek_kw() {
            Some(Kw::Not) => {
                self.bump();
                Expr::Not(boxed(self)?)
            }
            Some(Kw::Group(by)) => {
                self.bump();
                Expr::Group { by, expr: boxed(self)? }
            }
            Some(Kw::Bonded { include_self }) => {
                self.bump();
                Expr::Bonded { expr: boxed(self)?, include_self }
            }
            Some(Kw::First) => {
                self.bump();
                Expr::First(boxed(self)?)
            }
            Some(Kw::Last) => {
                self.bump();
                Expr::Last(boxed(self)?)
            }
            _ => self.term()?,
        })
    }

    fn term(&mut self) -> Result<Expr, SelectionError> {
        self.skip_ws();
        let Some((text, span)) = self.peek_token() else {
            return Err(self.error_at_end("a selection"));
        };
        if text == "(" {
            self.bump();
            let inner = self.or_expr()?;
            if !self.eat_symbol(")") {
                return Err(SelectionError { span, message: "unclosed '('".into() });
            }
            return Ok(inner);
        }
        if text.starts_with(SYMBOLS) {
            return Err(SelectionError { span, message: format!("expected a selection, found '{text}'") });
        }

        let Some(kw) = keyword(text) else {
            self.bump();
            // `%name` and `?name` are PyMOL's explicit selection/object references.
            let name = text.strip_prefix(['%', '?']).unwrap_or(text);
            return Ok(Expr::Named { name: name.to_owned(), span });
        };
        self.bump();
        Ok(match kw {
            Kw::All => Expr::All,
            Kw::None => Expr::None,
            Kw::Prop(prop) => Expr::Property { prop, patterns: self.patterns(prop)? },
            Kw::Resi => Expr::Resi(self.resi_items()?),
            Kw::Index(kind) => Expr::Index { kind, ranges: self.index_ranges()? },
            Kw::Numeric(field) => {
                self.skip_ws();
                let op = [("<=", CmpOp::Le), (">=", CmpOp::Ge), ("<", CmpOp::Lt), (">", CmpOp::Gt), ("=", CmpOp::Eq)]
                    .into_iter()
                    .find(|(sym, _)| self.eat_symbol(sym))
                    .map(|(_, op)| op)
                    .ok_or_else(|| self.error_here(&format!("expected <, <=, =, >= or > after '{text}'")))?;
                Expr::Compare { field, op, value: self.number("a number")? }
            }
            Kw::Class(class) => Expr::Class(class),
            Kw::State => Expr::State(self.number("a state number")?),
            _ => {
                return Err(SelectionError { span, message: format!("'{text}' needs a selection on its left") });
            }
        })
    }

    fn patterns(&mut self, prop: Property) -> Result<Vec<String>, SelectionError> {
        let what = match prop {
            Property::Name => "an atom name",
            Property::Resn => "a residue name",
            Property::Chain => "a chain ID",
            Property::Segi => "a segment ID",
            Property::Elem => "an element symbol",
            Property::Alt => "an alternate location ID",
            Property::Ss => "a secondary structure code (H, S or L)",
            Property::Model => "an object name",
        };
        let (word, span, quoted) = self.value(what)?;
        if quoted {
            return Ok(vec![word.to_owned()]);
        }
        split_plus(word, span, what, |s| Some(s.to_owned()))
    }

    fn resi_items(&mut self) -> Result<Vec<ResiItem>, SelectionError> {
        let (word, span, _) = self.value("a residue number")?;
        split_plus(word, span, "a residue number or range like 10-50", parse_resi_item)
    }

    fn index_ranges(&mut self) -> Result<Vec<(i64, i64)>, SelectionError> {
        let (word, span, _) = self.value("an atom number")?;
        split_plus(word, span, "an atom number or range like 1-100", |item| {
            match item.split_once(['-', ':']) {
                Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
                None => item.parse().ok().map(|n| (n, n)),
            }
        })
    }

    fn number<T: std::str::FromStr>(&mut self, what: &str) -> Result<T, SelectionError> {
        self.skip_ws();
        match self.peek_token() {
            Some((text, span)) if !text.starts_with(SYMBOLS) => {
                let n = text.parse().map_err(|_| SelectionError { span, message: format!("expected {what}, found '{text}'") })?;
                self.bump();
                Ok(n)
            }
            Some(_) => Err(self.error_here(&format!("expected {what}"))),
            None => Err(self.error_at_end(what)),
        }
    }

    /// A property value: a bare word or a quoted string. Returns (text, span of text, quoted).
    fn value(&mut self, what: &str) -> Result<(&'a str, Range<usize>, bool), SelectionError> {
        self.skip_ws();
        let rest = &self.src[self.pos..];
        if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
            let start = self.pos;
            let Some(len) = rest[1..].find(q) else {
                return Err(SelectionError { span: start..self.src.len(), message: "unterminated quote".into() });
            };
            self.pos = start + len + 2;
            self.last = Some(&self.src[start..self.pos]);
            return Ok((&rest[1..1 + len], start + 1..start + 1 + len, true));
        }
        match self.peek_token() {
            Some((text, span)) if !text.starts_with(SYMBOLS) => {
                self.bump();
                Ok((text, span, false))
            }
            Some(_) => Err(self.error_here(&format!("expected {what}"))),
            None => Err(self.error_at_end(what)),
        }
    }

    // --- lexing -------------------------------------------------------------------------------

    fn skip_ws(&mut self) {
        let rest = &self.src[self.pos..];
        self.pos += rest.len() - rest.trim_start().len();
    }

    /// Next token without consuming it: a symbol (`<=` and `>=` are one token) or a bare word.
    fn peek_token(&mut self) -> Option<(&'a str, Range<usize>)> {
        self.skip_ws();
        let rest = &self.src[self.pos..];
        let first = rest.chars().next()?;
        let len = if rest.starts_with("<=") || rest.starts_with(">=") {
            2
        } else if SYMBOLS.contains(&first) {
            1
        } else {
            rest.find(|c: char| c.is_whitespace() || SYMBOLS.contains(&c)).unwrap_or(rest.len())
        };
        Some((&rest[..len], self.pos..self.pos + len))
    }

    fn bump(&mut self) {
        if let Some((text, span)) = self.peek_token() {
            self.pos = span.end;
            self.last = Some(text);
        }
    }

    fn peek_kw(&mut self) -> Option<Kw> {
        self.peek_token().and_then(|(text, _)| keyword(text))
    }

    fn eat_kw(&mut self, kw: Kw) -> bool {
        let found = self.peek_kw() == Some(kw);
        if found {
            self.bump();
        }
        found
    }

    fn eat_symbol(&mut self, sym: &str) -> bool {
        let found = self.peek_token().is_some_and(|(text, _)| text == sym);
        if found {
            self.bump();
        }
        found
    }

    fn error_here(&mut self, message: &str) -> SelectionError {
        match self.peek_token() {
            Some((_, span)) => SelectionError { span, message: message.to_owned() },
            None => SelectionError { span: self.src.len()..self.src.len(), message: message.to_owned() },
        }
    }

    fn error_at_end(&self, what: &str) -> SelectionError {
        let end = self.src.len();
        let message = match self.last {
            Some(last) => format!("expected {what} after '{last}'"),
            None => format!("expected {what}"),
        };
        SelectionError { span: end..end, message }
    }
}

/// Splits a `+`-separated value list, reporting the span of the first item that fails `parse`.
fn split_plus<T>(
    word: &str,
    span: Range<usize>,
    what: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Vec<T>, SelectionError> {
    let mut out = Vec::new();
    let mut offset = span.start;
    for item in word.split('+') {
        let item_span = offset..offset + item.len();
        offset += item.len() + 1;
        match (!item.is_empty()).then(|| parse(item)).flatten() {
            Some(v) => out.push(v),
            None => {
                let shown = if item.is_empty() { "nothing".to_owned() } else { format!("'{item}'") };
                return Err(SelectionError { span: item_span, message: format!("expected {what}, found {shown}") });
            }
        }
    }
    Ok(out)
}

/// `10`, `10A`, `-5`, `\-5`, `10-50`, `10:50`, `-5--1`, `27A-29`.
fn parse_resi_item(item: &str) -> Option<ResiItem> {
    let (start, rest) = parse_resid(item)?;
    if rest.is_empty() {
        return Some(ResiItem::Single(start));
    }
    let (end, rest) = parse_resid(rest.strip_prefix(['-', ':'])?)?;
    rest.is_empty().then_some(ResiItem::Range(start, end))
}

fn parse_resid(s: &str) -> Option<(ResId, &str)> {
    let s = s.strip_prefix('\\').unwrap_or(s);
    let digits_start = usize::from(s.starts_with('-'));
    let digits_end = s[digits_start..].find(|c: char| !c.is_ascii_digit()).map_or(s.len(), |n| digits_start + n);
    if digits_end == digits_start {
        return None;
    }
    let seq = s[..digits_end].parse().ok()?;
    let rest = &s[digits_end..];
    match rest.chars().next() {
        Some(c) if c.is_ascii_alphabetic() => Some((ResId { seq, ins: Some(c) }, &rest[1..])),
        _ => Some((ResId { seq, ins: None }, rest)),
    }
}
