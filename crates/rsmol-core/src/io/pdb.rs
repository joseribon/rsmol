//! PDB format reader (ATOM/HETATM/MODEL/HELIX/SHEET/CONECT records).
//!
//! Spec: <https://www.wwpdb.org/documentation/file-format-content/format33/v3.3.html>

use std::collections::HashMap;
use std::path::Path;

use crate::io::builder::{AtomRecord, SsRange, StructureBuilder};
use crate::io::ParseError;
use crate::structure::{BondKind, SecondaryStructure, Structure, Vec3};
use crate::Element;

pub fn read(path: impl AsRef<Path>) -> Result<Structure, ParseError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| ParseError::io(path, e))?;
    parse(&text, super::file_stem(path))
}

pub fn parse(text: &str, name: impl Into<String>) -> Result<Structure, ParseError> {
    let mut b = StructureBuilder::new(name);
    let mut in_model = false;
    let mut serial_to_index: HashMap<i32, u32> = HashMap::new();
    let mut conect: Vec<(i32, i32)> = Vec::new();
    let mut last_line = 0;

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        last_line = line_no;
        let record = col(line, 1, 6);
        match record.trim_end() {
            "ATOM" | "HETATM" => {
                let before = b.atom_count();
                let rec = parse_atom(line, record == "HETATM").map_err(|m| ParseError::at(line_no, m))?;
                let serial = rec.serial;
                b.atom(rec).map_err(|m| ParseError::at(line_no, m))?;
                if b.atom_count() > before && serial >= 0 {
                    serial_to_index.insert(serial, before as u32);
                }
            }
            "MODEL" => {
                if in_model {
                    return Err(ParseError::at(line_no, "MODEL without preceding ENDMDL"));
                }
                in_model = true;
            }
            "ENDMDL" => {
                b.end_state().map_err(|m| ParseError::at(line_no, m))?;
                in_model = false;
            }
            "HELIX" => b.add_secondary_structure(SsRange {
                kind: SecondaryStructure::Helix,
                chain_id: col(line, 20, 20).trim().to_owned(),
                start: residue_id(line, 22, 26),
                end: residue_id(line, 34, 38),
            }),
            "SHEET" => b.add_secondary_structure(SsRange {
                kind: SecondaryStructure::Sheet,
                chain_id: col(line, 22, 22).trim().to_owned(),
                start: residue_id(line, 23, 27),
                end: residue_id(line, 34, 38),
            }),
            "CONECT" => {
                let Ok(from) = col(line, 7, 11).trim().parse() else { continue };
                for start in [12, 17, 22, 27] {
                    if let Ok(to) = col(line, start, start + 4).trim().parse() {
                        conect.push((from, to));
                    }
                }
            }
            "END" => break,
            _ => {}
        }
    }

    for (from, to) in conect {
        if let (Some(&a), Some(&c)) = (serial_to_index.get(&from), serial_to_index.get(&to)) {
            b.add_bond(a, c, BondKind::Covalent);
        }
    }
    b.finish().map_err(|m| ParseError::at(last_line, m))
}

fn parse_atom(line: &str, hetero: bool) -> Result<AtomRecord<'_>, String> {
    let name = col(line, 13, 16).trim();
    let chain_id = col(line, 22, 22).trim();
    Ok(AtomRecord {
        pos: parse_coords(line)?,
        name,
        res_name: col(line, 18, 20).trim(),
        chain_id,
        label_chain_id: chain_id,
        entity_id: None,
        seq_num: col(line, 23, 26).trim().parse().unwrap_or(0),
        ins_code: to_char(col(line, 27, 27)),
        alt_loc: to_char(col(line, 17, 17)),
        element: parse_element(line, name),
        occupancy: col(line, 55, 60).trim().parse().unwrap_or(1.0),
        b_factor: col(line, 61, 66).trim().parse().unwrap_or(0.0),
        // Serials overflow 99999 in large files (hybrid-36 or "*****"); they are informational only.
        serial: col(line, 7, 11).trim().parse().unwrap_or(-1),
        hetero,
    })
}

fn parse_coords(line: &str) -> Result<Vec3, String> {
    let field = |start, end, axis| {
        let raw = col(line, start, end).trim();
        raw.parse::<f32>().map_err(|_| format!("invalid {axis} coordinate {raw:?}"))
    };
    Ok([field(31, 38, "x")?, field(39, 46, "y")?, field(47, 54, "z")?])
}

/// Sequence number in columns `start..start+3` followed by an insertion code at `ins`.
fn residue_id(line: &str, start: usize, ins: usize) -> (i32, Option<char>) {
    (col(line, start, ins - 1).trim().parse().unwrap_or(i32::MIN), to_char(col(line, ins, ins)))
}

/// Element from columns 77-78, falling back to the atom-name convention: the element symbol is
/// right-justified in columns 13-14, so " CA " is carbon while "CA  " is calcium.
fn parse_element(line: &str, atom_name: &str) -> Element {
    let symbol = col(line, 77, 78).trim();
    if let Some(e) = Element::from_symbol(symbol).filter(|_| !symbol.is_empty()) {
        return e;
    }
    let field = col(line, 13, 14);
    let lead = field.chars().next().unwrap_or(' ');
    if lead == ' ' || lead.is_ascii_digit() {
        return field.get(1..).and_then(Element::from_symbol).unwrap_or(Element::UNKNOWN);
    }
    if let Some(e) = Element::from_symbol(field) {
        return e;
    }
    atom_name
        .chars()
        .find(char::is_ascii_alphabetic)
        .and_then(|c| Element::from_symbol(c.encode_utf8(&mut [0; 4])))
        .unwrap_or(Element::UNKNOWN)
}

/// 1-based inclusive column range, tolerant of short lines.
fn col(line: &str, start: usize, end: usize) -> &str {
    let end = end.min(line.len());
    if start > end {
        return "";
    }
    line.get(start - 1..end).unwrap_or("")
}

fn to_char(field: &str) -> Option<char> {
    field.chars().next().filter(|c| !c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn element_fallback_from_atom_name() {
        let ca_carbon = "ATOM      2  CA  ALA A   1      11.639   6.071  -5.147  1.00 13.79";
        let ca_calcium = "HETATM    3 CA    CA A 101      11.639   6.071  -5.147  1.00 13.79";
        let hydrogen = "ATOM      4 1HB  ALA A   1      11.639   6.071  -5.147  1.00 13.79";
        assert_eq!(parse_element(ca_carbon, "CA"), Element::C);
        assert_eq!(parse_element(ca_calcium, "CA"), Element(20));
        assert_eq!(parse_element(hydrogen, "1HB"), Element::H);
    }

    #[test]
    fn short_lines_do_not_panic() {
        assert_eq!(col("ATOM", 13, 16), "");
        assert_eq!(col("ATOM  ", 1, 6), "ATOM  ");
        assert!(parse("ATOM      1  N", "x").is_err());
    }

    #[test]
    fn helix_and_sheet_records() {
        let helix = "HELIX    1  H1 ILE A    7  PRO A   19  1                                  13";
        let sheet = "SHEET    1  S1 2 THR A   1  CYS A   4  0";
        assert_eq!(col(helix, 20, 20), "A");
        assert_eq!(residue_id(helix, 22, 26), (7, None));
        assert_eq!(residue_id(helix, 34, 38), (19, None));
        assert_eq!(col(sheet, 22, 22), "A");
        assert_eq!(residue_id(sheet, 23, 27), (1, None));
        assert_eq!(residue_id(sheet, 34, 38), (4, None));
    }
}
