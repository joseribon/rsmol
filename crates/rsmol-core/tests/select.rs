use rsmol_core::select::ast::{ResId, ResiItem};
use rsmol_core::select::{Expr, parse};

/// Parses and prints in canonical form, which makes precedence visible through parentheses.
fn canon(input: &str) -> String {
    match parse(input) {
        Ok(e) => e.to_string(),
        Err(e) => panic!("{}", e.render(input)),
    }
}

fn error(input: &str) -> String {
    parse(input).expect_err(input).render(input)
}

#[test]
fn terms_and_aliases() {
    assert_eq!(canon("chain A"), "chain A");
    assert_eq!(canon("n. CA+CB"), "name CA+CB");
    assert_eq!(canon("NAME ca"), "name ca");
    assert_eq!(canon("r. ALA+GLY"), "resn ALA+GLY");
    assert_eq!(canon("name O5'+C1'"), "name O5'+C1'");
    assert_eq!(canon("e. Fe"), "elem Fe");
    assert_eq!(canon("chain ''"), "chain \"\"");
    assert_eq!(canon("name \"C A\""), "name \"C A\"");
    assert_eq!(canon("*"), "all");
    assert_eq!(canon("sol. or h. or pol.protein"), "((solvent or hydrogens) or pol.protein)");
    assert_eq!(canon("polymer.protein"), "polymer.protein");
    assert_eq!(canon("b>50"), "b > 50");
    assert_eq!(canon("q <= 0.5"), "q <= 0.5");
    assert_eq!(canon("id 1-100+200"), "id 1-100+200");
    assert_eq!(canon("state 2"), "state 2");
    assert_eq!(canon("%sele"), "sele");
}

#[test]
fn residue_numbers() {
    let resi = |input: &str| match parse(input).unwrap() {
        Expr::Resi(items) => items,
        other => panic!("{other:?}"),
    };
    let id = |seq, ins| ResId { seq, ins };
    assert_eq!(
        resi("resi 10-50+60A+-5--1+70:72"),
        [
            ResiItem::Range(id(10, None), id(50, None)),
            ResiItem::Single(id(60, Some('A'))),
            ResiItem::Range(id(-5, None), id(-1, None)),
            ResiItem::Range(id(70, None), id(72, None)),
        ]
    );
    assert_eq!(resi(r"i. \-5"), [ResiItem::Single(id(-5, None))]);
    assert_eq!(canon("resi -5--1"), r"resi \-5-\-1");
}

#[test]
fn precedence() {
    // `b` and `q` are keywords (B-factor, occupancy), so placeholders are x/y/z.
    assert_eq!(canon("x or y and z"), "(x or (y and z))");
    assert_eq!(canon("x & y | z"), "((x and y) or z)");
    assert_eq!(canon("not x and y"), "(not x and y)");
    assert_eq!(canon("!(x or y)"), "not (x or y)");
    assert_eq!(canon("byres chain A and resi 10"), "(byres chain A and resi 10)");
    assert_eq!(canon("resn HOH within 5 of chain A and b > 30"), "((resn HOH within 5 of chain A) and b > 30)");
    assert_eq!(canon("organic around 4.5"), "(organic around 4.5)");
    assert_eq!(canon("br. organic x. 4"), "(byres organic expand 4)");
    assert_eq!(canon("lig extend 2 or a"), "((lig extend 2) or a)");
    assert_eq!(canon("polymer and not (bb. or h.)"), "(polymer and not (backbone or hydrogens))");
    assert_eq!(canon("name CA in obj2"), "(name CA in obj2)");
}

#[test]
fn canonical_form_reparses_identically() {
    for input in [
        "byres (resn HOH beyond 3.2 of polymer) or not chain B+C",
        "first (bto. e. Zn) and alt A+''",
        "resi -5--1+27A-29 and name O5'",
    ] {
        let once = parse(input).unwrap();
        assert_eq!(parse(&once.to_string()).unwrap(), once, "{input}");
    }
}

#[test]
fn errors_point_at_the_problem() {
    assert_eq!(error("chain A and"), "chain A and\n           ^ expected a selection after 'and'");
    assert_eq!(
        error("chain A resi 5"),
        "chain A resi 5\n        ^^^^ unexpected 'resi': combine selections with 'and' or 'or'"
    );
    assert_eq!(error("(chain A or b > 3"), "(chain A or b > 3\n^ unclosed '('");
    assert_eq!(error("chain A)"), "chain A)\n       ^ unmatched ')'");
    assert_eq!(
        error("resi 10+abc+20"),
        "resi 10+abc+20\n        ^^^ expected a residue number or range like 10-50, found 'abc'"
    );
    assert_eq!(error("name CA+"), "name CA+\n        ^ expected an atom name, found nothing");
    assert_eq!(error("lig within 5 chain A"), "lig within 5 chain A\n             ^^^^^ expected 'of'");
    assert_eq!(error("lig within x of a"), "lig within x of a\n           ^ expected a distance in Å, found 'x'");
    assert_eq!(error("and chain A"), "and chain A\n^^^ 'and' needs a selection on its left");
    assert_eq!(error("b 50"), "b 50\n  ^^ expected <, <=, =, >= or > after 'b'");
    assert_eq!(error("resn"), "resn\n    ^ expected a residue name after 'resn'");
}
