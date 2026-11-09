#![allow(clippy::unwrap_used)]

// Exact lookups search an index of their range (`functions::lookup_index`).
// The results must be those of the linear search, which an array literal in
// place of the range still uses.

use crate::test::util::new_empty_model;

use crate::Model;

fn lookups(model: &mut Model) -> Vec<String> {
    let keys = [
        "3", "apple", "TRUE", "", "Apple", "0", "x*y", "FALSE", "3", "=1/0",
    ];
    for (row, key) in keys.iter().enumerate() {
        let row = row + 1;
        model._set(&format!("A{row}"), key);
        model._set(&format!("B{row}"), &format!("r{row}"));
    }
    // The same keys in a row, for HLOOKUP and XMATCH over a row.
    for (column, key) in keys.iter().enumerate() {
        let column = (b'F' + column as u8) as char;
        model._set(&format!("{column}1"), key);
        model._set(&format!("{column}2"), &format!("c{column}"));
    }
    model._set("C1", "=A4");
    let targets = [
        "3",
        "3.0000000000000001",
        "\"APPLE\"",
        "TRUE",
        "FALSE",
        "0",
        "\"\"",
        "C1",
        "\"x*y\"",
        "\"x~*y\"",
        "\"x?y\"",
        "7",
        "\"pear\"",
        "\"3\"",
    ];
    let mut row = 20;
    for target in targets {
        for formula in [
            format!("=VLOOKUP({target},A1:B10,2,FALSE)"),
            format!("=HLOOKUP({target},F1:O2,2,FALSE)"),
            format!("=MATCH({target},A1:A10,0)"),
            format!("=XMATCH({target},A1:A10)"),
            format!("=XMATCH({target},F1:O1,0,-1)"),
            format!("=XMATCH({target},F1:O1,2)"),
            format!("=XLOOKUP({target},A1:A10,B1:B10,\"none\")"),
            format!("=XLOOKUP({target},A1:A10,B1:B10,\"none\",0,-1)"),
            format!("=XLOOKUP({target},A1:A10,B1:B10,\"none\",2)"),
        ] {
            model._set(&format!("Q{row}"), &formula);
            row += 1;
        }
    }
    model.evaluate();
    (20..row)
        .map(|r| model._get_text(&format!("Q{r}")))
        .collect()
}

#[test]
fn exact_lookups_find_what_linear_searches_find() {
    let mut indexed = new_empty_model();
    let mut linear = new_empty_model();
    linear.evaluation.without_lookup_index = true;
    let found = lookups(&mut indexed);
    assert_eq!(found, lookups(&mut linear));
    // VLOOKUP(3, ...) and the match of TRUE in a row.
    assert_eq!(found[0], "r1");
    assert!(found.iter().any(|value| value == "#N/A"));
}

#[test]
fn the_index_follows_edits_of_its_range() {
    let mut model = new_empty_model();
    for row in 1..=50 {
        model._set(&format!("A{row}"), &row.to_string());
        model._set(
            &format!("B{row}"),
            &format!("=VLOOKUP({row},A1:A50,1,FALSE)"),
        );
    }
    model.evaluate();
    assert_eq!(model._get_text("B7"), "7");
    model._set("A7", "70");
    model.evaluate();
    assert_eq!(model._get_text("B7"), "#N/A");
    model._set("A9", "7");
    model.evaluate();
    assert_eq!(model._get_text("B7"), "7");
    assert_eq!(model._get_text("B9"), "#N/A");
}
