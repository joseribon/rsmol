fn main() {
    let s = rsmol_core::io::pdb::read(std::env::args().nth(1).unwrap()).unwrap();
    for i in 0..s.atom_count() {
        let r = s.residue_of(i);
        println!("{i:>3} {:<4} {:<2} {} {} {}", s.atoms.name[i], s.atoms.element[i], r.name, r.seq_num, s.chain_of(i).id);
    }
}
