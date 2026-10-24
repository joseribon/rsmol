//! PDB format reader (ATOM/HETATM/MODEL records).
//!
//! Spec: <https://www.wwpdb.org/documentation/file-format-content/format33/sect9.html>

use std::path::Path;

use crate::io::ParseError;
use crate::structure::{Chain, Residue, Structure, Vec3};
use crate::Element;

pub fn read(path: impl AsRef<Path>) -> Result<Structure, ParseError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| ParseError::io(path, e))?;
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    parse(&text, name)
}

pub fn parse(text: &str, name: impl Into<String>) -> Result<Structure, ParseError> {
    let mut s = Structure { name: name.into(), ..Default::default() };
    let mut coords: Vec<Vec3> = Vec::new();
    // `true` once the first MODEL is complete; later models only contribute coordinates.
    let mut topology_done = false;
    let mut in_model = false;

    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let record = col(line, 1, 6);
        match record.trim_end() {
            "ATOM" | "HETATM" => {
                let pos = parse_coords(line).map_err(|m| ParseError::at(line_no, m))?;
                if topology_done {
                    if coords.len() >= s.atom_count() {
                        return Err(ParseError::at(line_no, "model has more atoms than model 1"));
                    }
                } else {
                    push_atom(&mut s, line, record == "HETATM");
                }
                coords.push(pos);
            }
            "MODEL" => {
                if in_model {
                    return Err(ParseError::at(line_no, "MODEL without preceding ENDMDL"));
                }
                in_model = true;
            }
            "ENDMDL" => {
                finish_state(&mut s, &mut coords, line_no)?;
                topology_done = true;
                in_model = false;
            }
            "END" => break,
            _ => {}
        }
    }

    // Files without MODEL records, or whose last MODEL lacks ENDMDL.
    if !coords.is_empty() {
        finish_state(&mut s, &mut coords, text.lines().count())?;
    }
    Ok(s)
}

fn finish_state(s: &mut Structure, coords: &mut Vec<Vec3>, line_no: usize) -> Result<(), ParseError> {
    if coords.len() != s.atom_count() {
        return Err(ParseError::at(
            line_no,
            format!("model has {} atoms, model 1 has {}", coords.len(), s.atom_count()),
        ));
    }
    s.states.push(std::mem::take(coords));
    Ok(())
}

fn push_atom(s: &mut Structure, line: &str, hetero: bool) {
    let atom_index = s.atoms.len() as u32;
    let atom_name = col(line, 13, 16).trim().to_owned();
    let res_name = col(line, 18, 20).trim().to_owned();
    let chain_id = col(line, 22, 22).trim().to_owned();
    let seq_num = col(line, 23, 26).trim().parse().unwrap_or(0);
    let ins_code = to_char(col(line, 27, 27));

    let chain_changed = s.chains.last().is_none_or(|c| c.id != chain_id);
    if chain_changed {
        let start = s.residues.len() as u32;
        s.chains.push(Chain { id: chain_id, residues: start..start });
    }
    let chain = (s.chains.len() - 1) as u32;

    let residue_changed = chain_changed
        || s.residues.last().is_none_or(|r| {
            r.seq_num != seq_num || r.ins_code != ins_code || r.name != res_name
        });
    if residue_changed {
        s.residues.push(Residue {
            name: res_name,
            seq_num,
            ins_code,
            chain,
            atoms: atom_index..atom_index,
        });
        s.chains.last_mut().unwrap().residues.end += 1;
    }
    let residue = s.residues.last_mut().unwrap();
    residue.atoms.end += 1;

    let a = &mut s.atoms;
    a.element.push(parse_element(line, &atom_name));
    a.name.push(atom_name);
    a.alt_loc.push(to_char(col(line, 17, 17)));
    a.occupancy.push(col(line, 55, 60).trim().parse().unwrap_or(1.0));
    a.b_factor.push(col(line, 61, 66).trim().parse().unwrap_or(0.0));
    // Serials overflow 99999 in large files (hybrid-36 or "*****"); they are informational only.
    a.serial.push(col(line, 7, 11).trim().parse().unwrap_or(-1));
    a.hetero.push(hetero);
    a.residue.push((s.residues.len() - 1) as u32);
}

fn parse_coords(line: &str) -> Result<Vec3, String> {
    let field = |start, end, axis| {
        let raw = col(line, start, end).trim();
        raw.parse::<f32>().map_err(|_| format!("invalid {axis} coordinate {raw:?}"))
    };
    Ok([field(31, 38, "x")?, field(39, 46, "y")?, field(47, 54, "z")?])
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
}
