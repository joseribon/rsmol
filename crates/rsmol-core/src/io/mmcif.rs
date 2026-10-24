//! PDBx/mmCIF reader.
//!
//! Atoms are named and numbered with the author (`auth_*`) fields, matching PDB files and
//! PyMOL; `label_asym_id` is kept as `Chain::label_id`.
//! Dictionary: <https://mmcif.wwpdb.org/dictionaries/mmcif_pdbx_v50.dic/Index/>

use std::collections::HashMap;
use std::path::Path;

use crate::io::builder::{AtomRecord, SsRange, StructureBuilder};
use crate::io::cif::{self, Block, Category, Column, Value};
use crate::io::ParseError;
use crate::structure::{BondKind, EntityKind, SecondaryStructure, Structure};
use crate::Element;

pub fn read(path: impl AsRef<Path>) -> Result<Structure, ParseError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|e| ParseError::io(path, e))?;
    parse(&text, super::file_stem(path))
}

pub fn parse(text: &str, name: impl Into<String>) -> Result<Structure, ParseError> {
    let doc = cif::parse(text)?;
    let block = doc.blocks.first().ok_or_else(|| ParseError::msg("no data block"))?;
    let mut b = StructureBuilder::new(name);

    if let Some(entity) = block.category("entity") {
        let (id, kind, desc) = (col(entity, "id"), col(entity, "type"), col(entity, "pdbx_description"));
        for row in 0..entity.row_count() {
            let Some(id) = str_at(id, row) else { continue };
            let kind = match str_at(kind, row) {
                Some("polymer") => EntityKind::Polymer,
                Some("non-polymer") => EntityKind::NonPolymer,
                Some("water") => EntityKind::Water,
                Some("branched") => EntityKind::Branched,
                _ => EntityKind::Other,
            };
            b.add_entity(id, kind, str_at(desc, row).unwrap_or(""));
        }
    }

    let atoms = read_atom_site(block, &mut b)?;
    read_secondary_structure(block, &mut b);
    read_struct_conn(block, &atoms, &mut b);
    b.finish().map_err(ParseError::msg)
}

/// Lookup key for struct_conn partners: (label_asym_id, auth_seq_id, ins_code, atom_name, alt_id).
type AtomKey<'a> = (&'a str, &'a str, Option<&'a str>, &'a str, Option<&'a str>);

fn read_atom_site<'a>(block: &Block<'a>, b: &mut StructureBuilder) -> Result<HashMap<AtomKey<'a>, u32>, ParseError> {
    let site = block.category("atom_site").ok_or_else(|| ParseError::msg("missing atom_site category"))?;
    let required = |item: &str| site.column(item).ok_or_else(|| ParseError::msg(format!("atom_site.{item} is missing")));
    let (x, y, z) = (required("Cartn_x")?, required("Cartn_y")?, required("Cartn_z")?);
    let either = |a: &str, b: &str| site.column(a).or_else(|| site.column(b));
    let atom_name = either("auth_atom_id", "label_atom_id");
    let res_name = either("auth_comp_id", "label_comp_id");
    let chain_id = either("auth_asym_id", "label_asym_id");
    let label_chain_id = either("label_asym_id", "auth_asym_id");
    let auth_seq = col(site, "auth_seq_id");
    let label_seq = col(site, "label_seq_id");
    let (group, serial, symbol) = (col(site, "group_PDB"), col(site, "id"), col(site, "type_symbol"));
    let (alt, ins, entity) = (col(site, "label_alt_id"), col(site, "pdbx_PDB_ins_code"), col(site, "label_entity_id"));
    let (occupancy, b_factor) = (col(site, "occupancy"), col(site, "B_iso_or_equiv"));
    let model = col(site, "pdbx_PDB_model_num");

    let mut keys = HashMap::new();
    let mut current_model = None;
    for row in 0..site.row_count() {
        let err = |m: String| ParseError::msg(format!("atom_site row {}: {m}", row + 1));
        let model_num = str_at(model, row);
        if current_model.is_some() && current_model != model_num {
            b.end_state().map_err(err)?;
        }
        current_model = model_num;

        let coord = |c: Column, axis| num(c.get(row)).ok_or_else(|| err(format!("invalid {axis} coordinate")));
        let name = str_at(atom_name, row).unwrap_or("");
        let seq_str = str_at(auth_seq, row).or_else(|| str_at(label_seq, row));
        let rec = AtomRecord {
            pos: [coord(x, "x")?, coord(y, "y")?, coord(z, "z")?],
            name,
            res_name: str_at(res_name, row).unwrap_or(""),
            chain_id: str_at(chain_id, row).unwrap_or(""),
            label_chain_id: str_at(label_chain_id, row).unwrap_or(""),
            entity_id: str_at(entity, row),
            seq_num: seq_str.and_then(|s| s.parse().ok()).unwrap_or(0),
            ins_code: str_at(ins, row).and_then(|s| s.chars().next()),
            alt_loc: str_at(alt, row).and_then(|s| s.chars().next()),
            element: str_at(symbol, row)
                .and_then(Element::from_symbol)
                .or_else(|| name.get(..1).and_then(Element::from_symbol))
                .unwrap_or(Element::UNKNOWN),
            occupancy: occupancy.and_then(|c| num(c.get(row))).unwrap_or(1.0),
            b_factor: b_factor.and_then(|c| num(c.get(row))).unwrap_or(0.0),
            serial: str_at(serial, row).and_then(|s| s.parse().ok()).unwrap_or(-1),
            hetero: str_at(group, row) == Some("HETATM"),
        };
        let key = (rec.label_chain_id, seq_str.unwrap_or(""), str_at(ins, row), name, str_at(alt, row));
        let before = b.atom_count();
        b.atom(rec).map_err(err)?;
        if b.atom_count() > before {
            keys.insert(key, before as u32);
        }
    }
    Ok(keys)
}

fn read_secondary_structure(block: &Block, b: &mut StructureBuilder) {
    let sources = [
        ("struct_conf", Some("conf_type_id")),
        ("struct_sheet_range", None),
    ];
    for (name, type_item) in sources {
        let Some(cat) = block.category(name) else { continue };
        let conf_type = type_item.and_then(|t| cat.column(t));
        let (beg_chain, beg_seq, beg_ins) =
            (col(cat, "beg_auth_asym_id"), col(cat, "beg_auth_seq_id"), col(cat, "pdbx_beg_PDB_ins_code"));
        let (end_chain, end_seq, end_ins) =
            (col(cat, "end_auth_asym_id"), col(cat, "end_auth_seq_id"), col(cat, "pdbx_end_PDB_ins_code"));
        for row in 0..cat.row_count() {
            let kind = match conf_type.map(|c| c.get(row).as_str()) {
                None => SecondaryStructure::Sheet,
                Some(Some(t)) if t.starts_with("HELX") => SecondaryStructure::Helix,
                Some(Some(t)) if t.starts_with("STRN") => SecondaryStructure::Sheet,
                _ => continue, // turns and unknown types
            };
            let residue = |seq, ins| {
                let seq = str_at(seq, row)?.parse().ok()?;
                Some((seq, str_at(ins, row).and_then(|s: &str| s.chars().next())))
            };
            let (Some(chain), Some(start), Some(end)) =
                (str_at(beg_chain, row), residue(beg_seq, beg_ins), residue(end_seq, end_ins))
            else {
                continue;
            };
            if str_at(end_chain, row).is_some_and(|c| c != chain) {
                continue;
            }
            b.add_secondary_structure(SsRange { kind, chain_id: chain.to_owned(), start, end });
        }
    }
}

fn read_struct_conn(block: &Block, atoms: &HashMap<AtomKey, u32>, b: &mut StructureBuilder) {
    let Some(conn) = block.category("struct_conn") else { return };
    let conn_type = col(conn, "conn_type_id");
    let partner = |n: u8| {
        (
            col(conn, &format!("ptnr{n}_label_asym_id")),
            col(conn, &format!("ptnr{n}_auth_seq_id")),
            col(conn, &format!("pdbx_ptnr{n}_PDB_ins_code")),
            col(conn, &format!("ptnr{n}_label_atom_id")),
            col(conn, &format!("pdbx_ptnr{n}_label_alt_id")),
            col(conn, &format!("ptnr{n}_symmetry")),
        )
    };
    let partners = [partner(1), partner(2)];

    for row in 0..conn.row_count() {
        let kind = match str_at(conn_type, row) {
            Some(t) if t.starts_with("covale") => BondKind::Covalent,
            Some("disulf") => BondKind::Disulfide,
            Some("metalc") => BondKind::MetalCoordination,
            _ => continue, // hydrogen bonds and mismatched base pairs are not bonds
        };
        let mut ends = [None; 2];
        for (end, &(asym, seq, ins, atom, alt, sym)) in ends.iter_mut().zip(&partners) {
            // Bonds to symmetry mates would need the crystal operators applied.
            if str_at(sym, row).is_some_and(|s| s != "1_555") {
                break;
            }
            let (Some(asym), Some(seq), Some(atom)) = (str_at(asym, row), str_at(seq, row), str_at(atom, row)) else {
                break;
            };
            let key = (asym, seq, str_at(ins, row), atom, str_at(alt, row));
            // Partners without an alt id still bond to the first conformer of a disordered atom.
            *end = atoms.get(&key).or_else(|| atoms.get(&(key.0, key.1, key.2, key.3, Some("A")))).copied();
        }
        if let [Some(a), Some(c)] = ends {
            b.add_bond(a, c, kind);
        }
    }
}

fn col<'c, 'a>(cat: &'c Category<'a>, item: &str) -> Option<Column<'c, 'a>> {
    cat.column(item)
}

fn str_at<'a>(column: Option<Column<'_, 'a>>, row: usize) -> Option<&'a str> {
    column.and_then(|c| c.get(row).as_str())
}

/// Parses a CIF number, dropping a standard uncertainty suffix: "12.345(6)" -> 12.345.
fn num(value: Value) -> Option<f32> {
    let s = value.as_str()?;
    s.split_once('(').map_or(s, |(n, _)| n).parse().ok()
}
