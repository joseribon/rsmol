//! Structure writers: PDB and PDBx/mmCIF.
//!
//! Both write the given atoms of every state (PDB as MODEL blocks, mmCIF via
//! `pdbx_PDB_model_num`), renumber atom serials from 1, and include secondary structure so
//! cartoons survive a round trip. PDB output refuses what the format cannot hold (chain IDs
//! longer than one character, more than 99,999 atoms) instead of silently corrupting it.

use std::fmt::Write;

use crate::{BondKind, EntityKind, SecondaryStructure, Structure};

/// Residue runs of one secondary-structure type: (kind, first atom's residue, last residue).
fn ss_runs(s: &Structure, atoms: &[usize]) -> Vec<(SecondaryStructure, u32, u32)> {
    let mut residues: Vec<u32> = atoms.iter().map(|&i| s.atoms.residue[i]).collect();
    residues.dedup();
    let mut runs: Vec<(SecondaryStructure, u32, u32)> = Vec::new();
    for r in residues {
        let res = &s.residues[r as usize];
        if res.ss == SecondaryStructure::Loop {
            continue;
        }
        match runs.last_mut() {
            // Extend only across consecutive residues of the same chain segment.
            Some((kind, _, last)) if *kind == res.ss && *last + 1 == r && s.residues[*last as usize].chain == res.chain => {
                *last = r;
            }
            _ => runs.push((res.ss, r, r)),
        }
    }
    runs
}

/// PDB atom-name alignment: one-letter elements start in column 14, others in column 13.
fn pdb_atom_name(name: &str, element: &str) -> String {
    if name.len() >= 4 || element.len() == 2 { format!("{name:<4}") } else { format!(" {name:<3}") }
}

pub fn to_pdb(s: &Structure, atoms: &[usize]) -> Result<String, String> {
    if atoms.len() > 99_999 {
        return Err(format!("{} atoms exceed the PDB format's 99,999; save as .cif instead", atoms.len()));
    }
    if let Some(&i) = atoms.iter().find(|&&i| s.chain_of(i).id.chars().count() > 1) {
        let id = &s.chain_of(i).id;
        return Err(format!("chain ID '{id}' is longer than PDB's one character; save as .cif instead"));
    }
    let mut out = String::new();
    writeln!(out, "REMARK   1 WRITTEN BY RSMOL").unwrap();

    for (k, (kind, first, last)) in ss_runs(s, atoms).into_iter().enumerate() {
        let (a, b) = (&s.residues[first as usize], &s.residues[last as usize]);
        let chain = &s.chains[a.chain as usize].id;
        let ins = |r: &crate::Residue| r.ins_code.unwrap_or(' ');
        let n = k + 1;
        match kind {
            SecondaryStructure::Helix => writeln!(
                out,
                "HELIX  {n:>3} {n:>3} {:>3} {chain:1} {:>4}{} {:>3} {chain:1} {:>4}{} 1{:>36}",
                a.name, a.seq_num, ins(a), b.name, b.seq_num, ins(b), last - first + 1
            ),
            _ => writeln!(
                out,
                "SHEET  {n:>3} {n:>3} 1 {:>3} {chain:1}{:>4}{} {:>3} {chain:1}{:>4}{} 0",
                a.name, a.seq_num, ins(a), b.name, b.seq_num, ins(b)
            ),
        }
        .unwrap();
    }

    let multi_state = s.state_count() > 1;
    for (state, coords) in s.states.iter().enumerate() {
        if multi_state {
            writeln!(out, "MODEL     {:>4}", state + 1).unwrap();
        }
        for (serial, &i) in atoms.iter().enumerate() {
            let r = s.residue_of(i);
            let element = s.atoms.element[i].symbol();
            let [x, y, z] = coords[i];
            writeln!(
                out,
                "{:<6}{:>5} {}{}{:>3} {:1}{:>4}{}   {x:>8.3}{y:>8.3}{z:>8.3}{:>6.2}{:>6.2}          {:>2}",
                if s.atoms.hetero[i] { "HETATM" } else { "ATOM" },
                serial + 1,
                pdb_atom_name(&s.atoms.name[i], element),
                s.atoms.alt_loc[i].unwrap_or(' '),
                r.name,
                s.chain_of(i).id,
                r.seq_num,
                r.ins_code.unwrap_or(' '),
                s.atoms.occupancy[i],
                s.atoms.b_factor[i],
                element.to_ascii_uppercase(),
            )
            .unwrap();
        }
        if multi_state {
            writeln!(out, "ENDMDL").unwrap();
        }
    }

    // CONECT for bonds involving HETATMs, as the format expects; polymer bonds are implied.
    let serial: std::collections::HashMap<usize, usize> = atoms.iter().enumerate().map(|(k, &i)| (i, k + 1)).collect();
    let mut partners: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
    for b in &s.bonds {
        let [a, c] = b.atoms.map(|x| x as usize);
        if b.kind == BondKind::MetalCoordination || !(s.atoms.hetero[a] || s.atoms.hetero[c]) {
            continue;
        }
        if let (Some(&sa), Some(&sc)) = (serial.get(&a), serial.get(&c)) {
            partners.entry(sa).or_default().push(sc);
            partners.entry(sc).or_default().push(sa);
        }
    }
    for (atom, mut bonded) in partners {
        bonded.sort_unstable();
        for chunk in bonded.chunks(4) {
            let rest: String = chunk.iter().map(|p| format!("{p:>5}")).collect();
            writeln!(out, "CONECT{atom:>5}{rest}").unwrap();
        }
    }
    writeln!(out, "END").unwrap();
    Ok(out)
}

/// A CIF value: bare when safe, quoted when it contains spaces or quote characters, `.` or `?`
/// when empty.
fn cif_value(v: &str, empty: &str) -> String {
    if v.is_empty() {
        return empty.to_owned();
    }
    let needs_quotes = v.contains(char::is_whitespace)
        || v.starts_with(['_', '#', '$', '\'', '"', ';', '['])
        || matches!(v, "." | "?")
        || v.contains('\'') && v.contains('"');
    match () {
        _ if !needs_quotes && !v.contains('\'') && !v.contains('"') => v.to_owned(),
        _ if !v.contains('"') => format!("\"{v}\""),
        _ => format!("'{v}'"),
    }
}

pub fn to_mmcif(s: &Structure, atoms: &[usize]) -> String {
    let name: String = s.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    let mut out = String::new();
    writeln!(out, "data_{}", if name.is_empty() { "rsmol" } else { &name }).unwrap();
    writeln!(out, "#\n_audit_conform.dict_name mmcif_pdbx.dic\n#").unwrap();
    if !s.entities.is_empty() {
        writeln!(out, "loop_\n_entity.id\n_entity.type\n_entity.pdbx_description").unwrap();
        for e in &s.entities {
            let kind = match e.kind {
                EntityKind::Polymer => "polymer",
                EntityKind::NonPolymer => "non-polymer",
                EntityKind::Water => "water",
                EntityKind::Branched => "branched",
                EntityKind::Other => "?",
            };
            writeln!(out, "{} {kind} {}", cif_value(&e.id, "?"), cif_value(&e.description, "?")).unwrap();
        }
        writeln!(out, "#").unwrap();
    }

    let runs = ss_runs(s, atoms);
    let helices: Vec<_> = runs.iter().filter(|r| r.0 == SecondaryStructure::Helix).collect();
    let strands: Vec<_> = runs.iter().filter(|r| r.0 == SecondaryStructure::Sheet).collect();
    let ins = |r: &crate::Residue| r.ins_code.map(String::from).unwrap_or_else(|| "?".into());
    if !helices.is_empty() {
        writeln!(out, "loop_\n_struct_conf.conf_type_id\n_struct_conf.id\n_struct_conf.beg_auth_asym_id\n_struct_conf.beg_auth_seq_id\n_struct_conf.pdbx_beg_PDB_ins_code\n_struct_conf.end_auth_asym_id\n_struct_conf.end_auth_seq_id\n_struct_conf.pdbx_end_PDB_ins_code").unwrap();
        for (k, (_, a, b)) in helices.iter().enumerate() {
            let (a, b) = (&s.residues[*a as usize], &s.residues[*b as usize]);
            let chain = cif_value(&s.chains[a.chain as usize].id, ".");
            writeln!(out, "HELX_P HELX_P{} {chain} {} {} {chain} {} {}", k + 1, a.seq_num, ins(a), b.seq_num, ins(b)).unwrap();
        }
        writeln!(out, "#").unwrap();
    }
    if !strands.is_empty() {
        writeln!(out, "loop_\n_struct_sheet_range.sheet_id\n_struct_sheet_range.id\n_struct_sheet_range.beg_auth_asym_id\n_struct_sheet_range.beg_auth_seq_id\n_struct_sheet_range.pdbx_beg_PDB_ins_code\n_struct_sheet_range.end_auth_asym_id\n_struct_sheet_range.end_auth_seq_id\n_struct_sheet_range.pdbx_end_PDB_ins_code").unwrap();
        for (k, (_, a, b)) in strands.iter().enumerate() {
            let (a, b) = (&s.residues[*a as usize], &s.residues[*b as usize]);
            let chain = cif_value(&s.chains[a.chain as usize].id, ".");
            writeln!(out, "S{} {} {chain} {} {} {chain} {} {}", k + 1, k + 1, a.seq_num, ins(a), b.seq_num, ins(b)).unwrap();
        }
        writeln!(out, "#").unwrap();
    }

    let columns = [
        "group_PDB", "id", "type_symbol", "label_atom_id", "label_alt_id", "label_comp_id", "label_asym_id",
        "label_entity_id", "label_seq_id", "pdbx_PDB_ins_code", "Cartn_x", "Cartn_y", "Cartn_z", "occupancy",
        "B_iso_or_equiv", "auth_seq_id", "auth_comp_id", "auth_asym_id", "auth_atom_id", "pdbx_PDB_model_num",
    ];
    writeln!(out, "loop_").unwrap();
    for c in columns {
        writeln!(out, "_atom_site.{c}").unwrap();
    }
    for (state, coords) in s.states.iter().enumerate() {
        for (serial, &i) in atoms.iter().enumerate() {
            let r = s.residue_of(i);
            let chain = s.chain_of(i);
            let entity = chain.entity.map(|e| s.entities[e as usize].id.clone()).unwrap_or_default();
            let atom_name = cif_value(&s.atoms.name[i], ".");
            let res_name = cif_value(&r.name, ".");
            let [x, y, z] = coords[i];
            writeln!(
                out,
                "{} {} {} {atom_name} {} {res_name} {} {} . {} {x:.3} {y:.3} {z:.3} {:.2} {:.2} {} {res_name} {} {atom_name} {}",
                if s.atoms.hetero[i] { "HETATM" } else { "ATOM" },
                serial + 1,
                s.atoms.element[i].symbol(),
                s.atoms.alt_loc[i].map(String::from).unwrap_or_else(|| ".".into()),
                cif_value(&chain.label_id, "."),
                cif_value(&entity, "?"),
                r.ins_code.map(String::from).unwrap_or_else(|| "?".into()),
                s.atoms.occupancy[i],
                s.atoms.b_factor[i],
                r.seq_num,
                cif_value(&chain.id, "."),
                state + 1,
            )
            .unwrap();
        }
    }
    writeln!(out, "#").unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom_name_alignment() {
        assert_eq!(pdb_atom_name("CA", "C"), " CA ");
        assert_eq!(pdb_atom_name("FE", "Fe"), "FE  ");
        assert_eq!(pdb_atom_name("HD21", "H"), "HD21");
        assert_eq!(pdb_atom_name("O5'", "O"), " O5'");
    }

    #[test]
    fn cif_quoting() {
        assert_eq!(cif_value("CA", "."), "CA");
        assert_eq!(cif_value("O5'", "."), "\"O5'\"");
        assert_eq!(cif_value("", "?"), "?");
        assert_eq!(cif_value("a b", "."), "\"a b\"");
        assert_eq!(cif_value(".", "."), "\".\"");
    }
}
