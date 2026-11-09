#![allow(clippy::unwrap_used)]

// After the first evaluation `evaluate` evaluates only the formulas that
// depend on what changed (see `Model::evaluate`). These tests check that it
// does, through `evaluation.partial`, and that the values are always those a
// full evaluation gives.

use std::collections::BTreeSet;

use crate::test::util::new_empty_model;
use crate::Model;

/// How many formulas the last evaluation evaluated, `None` if it evaluated
/// all of them.
fn evaluated(model: &Model) -> Option<usize> {
    model.evaluation.partial.as_ref().map(|dirty| dirty.len())
}

/// Every value of `model` against a copy of it evaluated in full.
fn assert_same_as_full(model: &Model) {
    assert_same_as_full_in(model, "");
}

fn assert_same_as_full_in(model: &Model, context: &str) {
    let mut full = Model::from_bytes(&model.to_bytes(), "en").unwrap();
    full.evaluate_all();
    // Under a depth limit set from outside (`IRONCALC_MAX_DEPTH`) a full
    // evaluation can tell cycles apart differently from one limit to another,
    // and so from a partial one: with a cycle around, values are not compared.
    let limited = std::env::var("IRONCALC_MAX_DEPTH").is_ok();
    let positions: BTreeSet<(u32, i32, i32)> = model
        .get_all_cells()
        .into_iter()
        .chain(full.get_all_cells())
        .map(|cell| (cell.index, cell.row, cell.column))
        .collect();
    let values: Vec<(String, String)> = positions
        .iter()
        .map(|&(sheet, row, column)| {
            (
                model._get_text_at(sheet, row, column),
                full._get_text_at(sheet, row, column),
            )
        })
        .collect();
    if limited && values.iter().any(|(a, b)| a == "#CIRC!" || b == "#CIRC!") {
        return;
    }
    for ((sheet, row, column), (value, expected)) in positions.into_iter().zip(values) {
        assert_eq!(
            value, expected,
            "{context} sheet {sheet}, row {row}, column {column}"
        );
    }
}

#[test]
fn an_edit_evaluates_only_its_dependents() {
    let mut model = new_empty_model();
    for row in 1..=100 {
        model._set(&format!("A{row}"), &row.to_string());
        model._set(&format!("B{row}"), &format!("=A{row}*2"));
    }
    model._set("C1", "=SUM(B1:B100)");
    model._set("D1", "=C1+1");
    model.evaluate();
    assert_eq!(evaluated(&model), None);
    assert_eq!(model._get_text("D1"), "10101");

    model._set("A7", "1007");
    model.evaluate();
    // B7, C1 and D1.
    assert_eq!(evaluated(&model), Some(3));
    assert_eq!(model._get_text("B7"), "2014");
    assert_eq!(model._get_text("D1"), "12101");
    assert_same_as_full(&model);

    // Nothing changed: nothing is evaluated.
    model.evaluate();
    assert_eq!(evaluated(&model), Some(0));
}

#[test]
fn a_new_formula_is_evaluated_and_followed() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("B1", "=A1+1");
    model._set("C1", "=B1*10");
    model.evaluate();
    model._set("B1", "=A1+5");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "60");
    // The new formula reads A1: an edit of A1 reaches it.
    model._set("A1", "2");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "70");
    // The formula is gone: an edit of A1 reaches nothing.
    model._set("B1", "3");
    model.evaluate();
    model._set("A1", "9");
    model.evaluate();
    assert_eq!(evaluated(&model), Some(0));
    assert_eq!(model._get_text("C1"), "30");
    assert_same_as_full(&model);
}

#[test]
fn ranges_whole_columns_and_clearing() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("A5000", "2");
    model._set("B1", "=SUM(A:A)");
    model._set("B2", "=COUNT(A1:A10)");
    model._set("B3", "=SUM(A4990:A6000)");
    model.evaluate();
    model._set("A5001", "3");
    model.evaluate();
    assert_eq!(model._get_text("B1"), "6");
    assert_eq!(model._get_text("B2"), "1");
    assert_eq!(model._get_text("B3"), "5");
    model._cell_clear_contents(0, 1, 1).unwrap();
    model.evaluate();
    assert_eq!(model._get_text("B1"), "5");
    assert_eq!(model._get_text("B2"), "0");
    assert_same_as_full(&model);
}

#[test]
fn other_sheets_and_defined_names() {
    let mut model = new_empty_model();
    model.add_sheet("Data").unwrap();
    model._set("Data!A1", "4");
    model._set("A1", "=Data!A1*2");
    model.new_defined_name("price", None, "Data!$B$1").unwrap();
    model
        .new_defined_name("prices", None, "Data!$B$1:$B$3")
        .unwrap();
    model._set("A2", "=price+1");
    model._set("A3", "=SUM(prices)");
    model.evaluate();
    model._set("Data!A1", "5");
    model._set("Data!B1", "10");
    model._set("Data!B3", "1");
    model.evaluate();
    assert!(evaluated(&model).is_some());
    assert_eq!(model._get_text("A1"), "10");
    assert_eq!(model._get_text("A2"), "11");
    assert_eq!(model._get_text("A3"), "11");

    // A name that points elsewhere now: everything is evaluated.
    model
        .update_defined_name("price", None, "price", None, "Data!$B$3")
        .unwrap();
    model.evaluate();
    assert_eq!(model._get_text("A2"), "2");
    model._set("Data!B3", "7");
    model.evaluate();
    assert_eq!(model._get_text("A2"), "8");
    assert_eq!(model._get_text("A3"), "17");
    assert_same_as_full(&model);
}

#[test]
fn sheets_added_renamed_and_deleted() {
    let mut model = new_empty_model();
    model._set("A2", "1");
    model.evaluate();
    model.add_sheet("Other").unwrap();
    model._set("Other!A1", "1");
    model._set("A1", "=Other!A1+1");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "2");
    model.rename_sheet("Other", "Renamed").unwrap();
    model._set("Renamed!A1", "5");
    model.evaluate();
    assert_eq!(model._get_formula("A1"), "=Renamed!A1+1");
    assert_eq!(model._get_text("A1"), "6");
    model.delete_sheet(1).unwrap();
    model.evaluate();
    assert_eq!(model._get_text("A1"), "#REF!");
    assert_same_as_full(&model);
}

#[test]
fn rows_and_columns_inserted_and_deleted() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("A2", "2");
    model._set("B1", "=SUM(A1:A2)");
    model._set("C1", "=B1*A2");
    model.evaluate();
    model.insert_rows(0, 2, 1).unwrap();
    model.evaluate();
    model._set("A2", "10");
    model.evaluate();
    assert_eq!(model._get_formula("B1"), "=SUM(A1:A3)");
    assert_eq!(model._get_text("B1"), "13");
    assert_eq!(model._get_text("C1"), "26");
    model.delete_columns(0, 1, 1).unwrap();
    model.evaluate();
    model._set("A3", "4");
    model.evaluate();
    assert_same_as_full(&model);
}

#[test]
fn cycles_made_and_broken() {
    let mut model = new_empty_model();
    model._set("A1", "=B1+1");
    model._set("B1", "5");
    model._set("C1", "=A1*2");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "12");
    model._set("B1", "=A1+1");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "#CIRC!");
    assert_eq!(model._get_text("B1"), "#CIRC!");
    assert_eq!(model._get_text("C1"), "#CIRC!");
    model._set("B1", "1");
    model.evaluate();
    assert_eq!(model._get_text("A1"), "2");
    assert_eq!(model._get_text("C1"), "4");
    assert_same_as_full(&model);
}

#[test]
fn volatile_functions_run_every_time() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("A2", "2");
    model._set("B1", "A2");
    model._set("C1", "=INDIRECT(B1)");
    model._set("C2", "=SUM(OFFSET(A1,0,0,2,1))");
    model._set("C3", "=C1*10");
    model._set("D1", "unrelated");
    model.evaluate();
    assert_eq!(model._get_text("C3"), "20");
    model._set("A2", "7");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "7");
    assert_eq!(model._get_text("C2"), "8");
    assert_eq!(model._get_text("C3"), "70");
    // Nothing they name changed, and they run all the same.
    model._set("D1", "still unrelated");
    model.evaluate();
    assert_eq!(evaluated(&model), Some(3));
    assert_same_as_full(&model);
}

#[test]
fn hidden_rows_reach_subtotal() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("A2", "2");
    model._set("B1", "=SUBTOTAL(109,A1:A2)");
    model.evaluate();
    model.set_row_hidden(0, 2, true).unwrap();
    model.evaluate();
    assert_eq!(model._get_text("B1"), "1");
    assert_same_as_full(&model);
}

#[test]
fn spills_that_grow_shrink_and_block() {
    let mut model = new_empty_model();
    model._set("A1", "3");
    model._set("B1", "=SEQUENCE(A1)");
    model._set("C1", "=SUM(B1:B5)");
    model._set("C2", "=B4");
    model._set("C3", "=SUM(B1#)");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "6");
    model._set("A1", "5");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "15");
    assert_eq!(model._get_text("C2"), "4");
    assert_eq!(model._get_text("C3"), "15");
    // Typed into the spill area: the array is blocked.
    model._set("B3", "x");
    model.evaluate();
    assert_eq!(model._get_text("B1"), "#SPILL!");
    assert_eq!(model._get_text("C1"), "#SPILL!");
    assert_eq!(model._get_text("C2"), "0");
    // And free again.
    model._cell_clear_contents(0, 3, 2).unwrap();
    model.evaluate();
    assert_eq!(model._get_text("C1"), "15");
    // Values change, the area does not.
    model._set("B1", "=SEQUENCE(A1,1,10)");
    model.evaluate();
    assert_eq!(model._get_text("C2"), "13");
    assert_eq!(model._get_text("C3"), "60");
    assert_same_as_full(&model);
}

#[test]
fn array_formulas_write_their_area() {
    let mut model = new_empty_model();
    model._set("A1", "1");
    model._set("A2", "2");
    model
        .set_user_array_formula(0, 1, 2, 1, 2, "=A1:A2*10")
        .unwrap();
    model._set("C1", "=B2+1");
    model.evaluate();
    assert_eq!(model._get_text("C1"), "21");
    model._set("A2", "5");
    model.evaluate();
    assert_eq!(model._get_text("B2"), "50");
    assert_eq!(model._get_text("C1"), "51");
    assert_same_as_full(&model);
}

#[test]
fn bulk_changes_evaluate_everything() {
    let mut model = new_empty_model();
    model._set("B1", "=SUM(A:A)");
    model.evaluate();
    for row in 1..=20_000 {
        model._set(&format!("A{row}"), "1");
    }
    model.evaluate();
    assert_eq!(evaluated(&model), None);
    assert_eq!(model._get_text("B1"), "20000");
}

// ── Random edits ────────────────────────────────────────────────────────────
//
// Two sheets of 8 rows. Columns A to C hold data: numbers, text, and formulas
// that read cells of A to C in rows above. Columns D to F hold the formulas
// that read ranges of A to C, on either sheet, and cells of D to F in rows
// above: arrays, lookups, sums. So the sheets have no cycle, except for the
// odd formula that reads anywhere, which makes one now and then.

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % n
    }

    fn pick<'a>(&mut self, choices: &[&'a str]) -> &'a str {
        choices[self.below(choices.len() as u64) as usize]
    }
}

const ROWS: u64 = 8;

fn column(rng: &mut Rng, columns: &str) -> char {
    columns.as_bytes()[rng.below(columns.len() as u64) as usize] as char
}

/// A cell of `columns` in a row above `row`, which is 2 or more.
fn above(rng: &mut Rng, columns: &str, row: u64) -> String {
    format!("{}{}", column(rng, columns), 1 + rng.below(row - 1))
}

/// Any data cell.
fn data_cell(rng: &mut Rng) -> String {
    above(rng, "ABC", ROWS + 1)
}

fn sheet(rng: &mut Rng) -> &'static str {
    if rng.below(4) == 0 {
        "Sheet2!"
    } else {
        ""
    }
}

/// What to write at `row` of a data column.
fn data(rng: &mut Rng, row: u64) -> String {
    match rng.below(6) {
        _ if row == 1 => rng.below(5).to_string(),
        0..=2 => rng.below(5).to_string(),
        3 => rng.pick(&["apple", "Pear", "PEAR", ""]).to_string(),
        4 => format!("={}+1", above(rng, "ABC", row)),
        _ => format!("={}*{}", above(rng, "ABC", row), above(rng, "ABC", row)),
    }
}

/// What to write at `row` of a formula column: it reads data, and formulas
/// in rows above.
fn formula(rng: &mut Rng, row: u64) -> String {
    let data = sheet(rng);
    let key = if row > 1 && rng.below(2) == 0 {
        above(rng, "DEF", row)
    } else {
        format!("{data}{}", data_cell(rng))
    };
    match rng.below(12) {
        0 => format!("=SUM({data}A1:C{ROWS})+{key}"),
        1 => format!("=VLOOKUP({key},{data}A1:C{ROWS},2,FALSE)"),
        2 => format!("=MATCH({key},{data}B1:B{ROWS},0)"),
        3 => format!("=XLOOKUP({key},{data}C1:C{ROWS},{data}A1:A{ROWS},\"none\",0,-1)"),
        4 => format!("=XMATCH({key},{data}A1:C1)"),
        5 => format!("=COUNTIF({data}A1:C{ROWS},{key})"),
        6 => format!("=HLOOKUP({key},{data}A1:C3,3,FALSE)"),
        7 => format!("=SEQUENCE({})", 1 + rng.below(3)),
        8 => format!("=SEQUENCE(MIN(3,MAX(1,{data}{})))", data_cell(rng)),
        9 => format!("=IF({key}>2,{data}{},{key})", data_cell(rng)),
        10 => format!("={key}&\"!\""),
        // Anywhere, now and then: a cycle, sometimes.
        _ if rng.below(4) == 0 => format!("={}+1", above(rng, "ABCDEF", ROWS + 1)),
        _ => format!("={key}*2"),
    }
}

/// Writes something random into a random cell, or clears it.
fn edit(model: &mut Model, rng: &mut Rng) {
    let sheet = rng.below(2) as u32;
    let row = 1 + rng.below(ROWS);
    let in_data = rng.below(2) == 0;
    let column = if in_data {
        column(rng, "ABC")
    } else {
        column(rng, "DEF")
    };
    if rng.below(6) == 0 {
        let column = (column as u8 - b'A' + 1) as i32;
        model
            ._cell_clear_contents(sheet, row as i32, column)
            .unwrap();
        return;
    }
    let content = if in_data {
        data(rng, row)
    } else {
        formula(rng, row)
    };
    let prefix = if sheet == 1 { "Sheet2!" } else { "" };
    model._set(&format!("{prefix}{column}{row}"), &content);
}

#[test]
fn random_edits_match_a_full_evaluation() {
    let seeds = 400u64;
    let steps = 25;
    let mut partial_evaluations = 0;
    for seed in 1..=seeds {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut model = new_empty_model();
        model.add_sheet("Sheet2").unwrap();
        for _ in 0..40 {
            edit(&mut model, &mut rng);
        }
        model.evaluate();
        for step in 0..steps {
            for _ in 0..1 + rng.below(3) {
                edit(&mut model, &mut rng);
            }
            model.evaluate();
            if evaluated(&model).is_some() {
                partial_evaluations += 1;
            }
            assert_same_as_full_in(&model, &format!("seed {seed}, step {step}:"));
        }
    }
    // Most evaluations were partial: the test exercised them.
    let all = seeds * steps;
    assert!(
        partial_evaluations * 2 > all,
        "{partial_evaluations} of {all}"
    );
}
