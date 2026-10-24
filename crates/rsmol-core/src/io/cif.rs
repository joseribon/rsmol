//! Generic CIF 1.1 reader: data blocks, key-value pairs and `loop_` tables.
//!
//! Values borrow from the input text. Save frames (dictionary files) are not supported.
//! Spec: <https://www.iucr.org/resources/cif/spec/version1.1/cifsyntax>

use std::collections::HashMap;

use crate::io::ParseError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Value<'a> {
    Text(&'a str),
    /// `?`: the value is unknown.
    Missing,
    /// `.`: the item does not apply to this row.
    Inapplicable,
}

impl<'a> Value<'a> {
    pub fn as_str(self) -> Option<&'a str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct Document<'a> {
    pub blocks: Vec<Block<'a>>,
}

#[derive(Debug, Default)]
pub struct Block<'a> {
    pub name: &'a str,
    categories: Vec<Category<'a>>,
    index: HashMap<String, usize>,
}

/// A table of items, stored row-major. Single key-value pairs form a one-row category.
#[derive(Debug)]
pub struct Category<'a> {
    pub name: String,
    items: Vec<String>,
    values: Vec<Value<'a>>,
}

#[derive(Clone, Copy)]
pub struct Column<'c, 'a> {
    values: &'c [Value<'a>],
    stride: usize,
    offset: usize,
}

impl<'a> Block<'a> {
    /// Category by name, case-insensitive and without the leading underscore ("atom_site").
    pub fn category(&self, name: &str) -> Option<&Category<'a>> {
        self.index.get(&name.to_ascii_lowercase()).map(|&i| &self.categories[i])
    }

    fn category_mut(&mut self, name: String) -> &mut Category<'a> {
        let i = *self.index.entry(name.clone()).or_insert_with(|| {
            self.categories.push(Category { name, items: Vec::new(), values: Vec::new() });
            self.categories.len() - 1
        });
        &mut self.categories[i]
    }
}

impl<'a> Category<'a> {
    pub fn row_count(&self) -> usize {
        if self.items.is_empty() { 0 } else { self.values.len() / self.items.len() }
    }

    /// Column by item name, case-insensitive ("Cartn_x").
    pub fn column(&self, item: &str) -> Option<Column<'_, 'a>> {
        let offset = self.items.iter().position(|i| i.eq_ignore_ascii_case(item))?;
        Some(Column { values: &self.values, stride: self.items.len(), offset })
    }

    /// Value of `item` in the first row; convenient for key-value categories.
    pub fn get(&self, item: &str) -> Option<Value<'a>> {
        self.column(item).map(|c| c.get(0))
    }
}

impl<'a> Column<'_, 'a> {
    pub fn get(&self, row: usize) -> Value<'a> {
        self.values[row * self.stride + self.offset]
    }
}

pub fn parse(text: &str) -> Result<Document<'_>, ParseError> {
    let mut lexer = Lexer { src: text, pos: 0, line: 1, peeked: None };
    let mut doc = Document::default();

    while let Some(tok) = lexer.next()? {
        if let Some(name) = keyword(&tok, "data_") {
            doc.blocks.push(Block { name, ..Default::default() });
            continue;
        }
        let block = doc
            .blocks
            .last_mut()
            .ok_or_else(|| ParseError::at(tok.line, "content before first data_ block"))?;

        if keyword(&tok, "loop_") == Some("") {
            let mut tags = Vec::new();
            while let Some(t) = lexer.peek()?.filter(|t| is_tag(t)) {
                tags.push(split_tag(t.text));
                lexer.next()?;
            }
            let Some((category, _)) = tags.first().cloned() else {
                return Err(ParseError::at(tok.line, "loop_ without tags"));
            };
            if tags.iter().any(|(c, _)| *c != category) {
                return Err(ParseError::at(tok.line, "loop_ mixes categories"));
            }
            let cat = block.category_mut(category);
            if !cat.items.is_empty() {
                return Err(ParseError::at(tok.line, format!("category {} defined twice", cat.name)));
            }
            cat.items = tags.into_iter().map(|(_, item)| item).collect();
            while let Some(t) = lexer.peek()?.filter(|t| is_value(t)) {
                cat.values.push(t.value());
                lexer.next()?;
            }
            if cat.values.len() % cat.items.len() != 0 {
                return Err(ParseError::at(
                    tok.line,
                    format!("loop_ for {} has {} values, not a multiple of {} items", cat.name, cat.values.len(), cat.items.len()),
                ));
            }
        } else if is_tag(&tok) {
            let value = match lexer.next()? {
                Some(v) if is_value(&v) => v.value(),
                _ => return Err(ParseError::at(tok.line, format!("missing value for {}", tok.text))),
            };
            let (category, item) = split_tag(tok.text);
            let cat = block.category_mut(category);
            if cat.items.contains(&item) || cat.row_count() > 1 {
                return Err(ParseError::at(tok.line, format!("duplicate item {}", tok.text)));
            }
            cat.items.push(item);
            cat.values.push(value);
        } else if keyword(&tok, "save_").is_some() {
            return Err(ParseError::at(tok.line, "save frames are not supported"));
        } else {
            return Err(ParseError::at(tok.line, format!("unexpected value {:?}", tok.text)));
        }
    }
    Ok(doc)
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TokKind {
    Bare,
    Quoted,
}

#[derive(Clone, Copy, Debug)]
struct Tok<'a> {
    text: &'a str,
    kind: TokKind,
    line: usize,
}

impl<'a> Tok<'a> {
    fn value(&self) -> Value<'a> {
        match (self.kind, self.text) {
            (TokKind::Bare, "?") => Value::Missing,
            (TokKind::Bare, ".") => Value::Inapplicable,
            _ => Value::Text(self.text),
        }
    }
}

/// For a bare token starting with `prefix` (case-insensitive), the rest of it.
fn keyword<'a>(tok: &Tok<'a>, prefix: &str) -> Option<&'a str> {
    let head = tok.text.get(..prefix.len())?;
    (tok.kind == TokKind::Bare && head.eq_ignore_ascii_case(prefix)).then(|| &tok.text[prefix.len()..])
}

fn is_tag(tok: &Tok) -> bool {
    tok.kind == TokKind::Bare && tok.text.starts_with('_')
}

fn is_value(tok: &Tok) -> bool {
    !is_tag(tok) && ["data_", "loop_", "save_", "global_", "stop_"].iter().all(|k| keyword(tok, k).is_none())
}

/// "_atom_site.Cartn_x" -> ("atom_site", "cartn_x").
fn split_tag(tag: &str) -> (String, String) {
    let tag = tag[1..].to_ascii_lowercase();
    match tag.split_once('.') {
        Some((c, i)) => (c.to_owned(), i.to_owned()),
        None => (tag, String::new()),
    }
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    peeked: Option<Option<Tok<'a>>>,
}

impl<'a> Lexer<'a> {
    fn peek(&mut self) -> Result<Option<Tok<'a>>, ParseError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.scan()?);
        }
        Ok(self.peeked.unwrap())
    }

    fn next(&mut self) -> Result<Option<Tok<'a>>, ParseError> {
        match self.peeked.take() {
            Some(t) => Ok(t),
            None => self.scan(),
        }
    }

    fn scan(&mut self) -> Result<Option<Tok<'a>>, ParseError> {
        let bytes = self.src.as_bytes();
        loop {
            while self.pos < bytes.len() && bytes[self.pos].is_ascii_whitespace() {
                if bytes[self.pos] == b'\n' {
                    self.line += 1;
                }
                self.pos += 1;
            }
            if self.pos < bytes.len() && bytes[self.pos] == b'#' {
                self.pos = memchr(b'\n', bytes, self.pos).unwrap_or(bytes.len());
                continue;
            }
            break;
        }
        if self.pos >= bytes.len() {
            return Ok(None);
        }

        let start = self.pos;
        let line = self.line;
        let at_line_start = start == 0 || bytes[start - 1] == b'\n';
        match bytes[start] {
            b';' if at_line_start => {
                // Text field: runs until a line that starts with ';'.
                let mut search = start + 1;
                let end = loop {
                    let Some(nl) = memchr(b'\n', bytes, search) else {
                        return Err(ParseError::at(line, "unterminated text field"));
                    };
                    self.line += 1;
                    if bytes.get(nl + 1) == Some(&b';') {
                        break nl;
                    }
                    search = nl + 1;
                };
                self.pos = end + 2;
                let text = self.src[start + 1..end].trim_end_matches('\r');
                Ok(Some(Tok { text, kind: TokKind::Quoted, line }))
            }
            q @ (b'\'' | b'"') => {
                // A quote only closes when followed by whitespace, so `'O5' sugar'` is one value.
                let mut i = start + 1;
                loop {
                    match bytes.get(i) {
                        None | Some(b'\n') => return Err(ParseError::at(line, "unterminated quoted string")),
                        Some(&c) if c == q && bytes.get(i + 1).is_none_or(u8::is_ascii_whitespace) => break,
                        _ => i += 1,
                    }
                }
                self.pos = i + 1;
                Ok(Some(Tok { text: &self.src[start + 1..i], kind: TokKind::Quoted, line }))
            }
            _ => {
                let end = bytes[start..]
                    .iter()
                    .position(u8::is_ascii_whitespace)
                    .map_or(bytes.len(), |n| start + n);
                self.pos = end;
                Ok(Some(Tok { text: &self.src[start..end], kind: TokKind::Bare, line }))
            }
        }
    }
}

fn memchr(needle: u8, haystack: &[u8], from: usize) -> Option<usize> {
    haystack[from..].iter().position(|&b| b == needle).map(|n| from + n)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
data_TEST
#
_entry.id   TEST
_struct.title 'A \"quoted\" title with it's apostrophe'
loop_
_atom_type.symbol
_atom_type.note
C  .
N  ?
O
;multi
line
;
#
";

    #[test]
    fn values_and_loops() {
        let doc = parse(SAMPLE).unwrap();
        let block = &doc.blocks[0];
        assert_eq!(block.name, "TEST");
        assert_eq!(block.category("entry").unwrap().get("id"), Some(Value::Text("TEST")));
        assert_eq!(
            block.category("struct").unwrap().get("title"),
            Some(Value::Text("A \"quoted\" title with it's apostrophe"))
        );

        let types = block.category("atom_type").unwrap();
        assert_eq!(types.row_count(), 3);
        let symbol = types.column("SYMBOL").unwrap();
        let note = types.column("note").unwrap();
        assert_eq!(symbol.get(2), Value::Text("O"));
        assert_eq!(note.get(0), Value::Inapplicable);
        assert_eq!(note.get(1), Value::Missing);
        assert_eq!(note.get(2), Value::Text("multi\nline"));
    }

    #[test]
    fn errors_carry_line_numbers() {
        let err = parse("data_X\nloop_\n_a.b\n_a.c\n1 2 3\n").unwrap_err();
        assert_eq!(err.line, Some(2));
        let err = parse("data_X\n_a.b 'open\n").unwrap_err();
        assert_eq!(err.line, Some(2));
        assert!(parse("_a.b 1\n").is_err());
    }
}
