//! Which formulas read which cells, so that after an edit only the formulas
//! that depend on it are evaluated again (see `Model::evaluate`).
//!
//! What a formula reads is found in its parsed form, before it runs: every
//! reference and range in it, resolved against its cell, and the reference of
//! every defined name it uses. A formula whose reads cannot be known that way
//! (INDIRECT, OFFSET, a name that holds a LAMBDA, ...) or whose value changes
//! on its own (NOW, RAND, ...) is volatile: it runs on every evaluation.
//!
//! The reads of single cells are kept in a map from the cell to its readers.
//! A range is kept once, with all its readers, and listed in every block of
//! the sheet it overlaps (`BLOCK_ROWS` by `BLOCK_COLUMNS` cells), so that the
//! ranges holding a cell are among the few listed in that cell's block. A
//! range over more than `MAX_BLOCKS` blocks, a whole column say, is listed
//! once in `wide` and checked on every search.

use std::collections::{HashMap, HashSet};

use crate::constants::{LAST_COLUMN, LAST_ROW};
use crate::evaluation::{CellKey, KeyBuild};
use crate::expressions::parser::Node;
use crate::expressions::types::CellReferenceIndex;
use crate::functions::Function;
use crate::model::{Model, ParsedDefinedName};

const BLOCK_ROWS: i32 = 64;
const BLOCK_COLUMNS: i32 = 16;
const MAX_BLOCKS: i64 = 64;

/// A rectangle of cells of one sheet, corners included.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Rect {
    pub(crate) sheet: u32,
    pub(crate) row1: i32,
    pub(crate) column1: i32,
    pub(crate) row2: i32,
    pub(crate) column2: i32,
}

impl Rect {
    pub(crate) fn cell((sheet, row, column): CellKey) -> Rect {
        Rect {
            sheet,
            row1: row,
            column1: column,
            row2: row,
            column2: column,
        }
    }

    pub(crate) fn contains(&self, (sheet, row, column): CellKey) -> bool {
        sheet == self.sheet
            && (self.row1..=self.row2).contains(&row)
            && (self.column1..=self.column2).contains(&column)
    }

    fn intersects(&self, other: &Rect) -> bool {
        self.sheet == other.sheet
            && self.row1 <= other.row2
            && other.row1 <= self.row2
            && self.column1 <= other.column2
            && other.column1 <= self.column2
    }

    fn is_cell(&self) -> bool {
        self.row1 == self.row2 && self.column1 == self.column2
    }

    fn cell_count(&self) -> i64 {
        (self.row2 - self.row1 + 1) as i64 * (self.column2 - self.column1 + 1) as i64
    }

    /// The blocks the rectangle overlaps, as `(first, last)` block row and
    /// block column.
    fn blocks(&self) -> ((i32, i32), (i32, i32)) {
        (
            (self.row1 / BLOCK_ROWS, self.row2 / BLOCK_ROWS),
            (self.column1 / BLOCK_COLUMNS, self.column2 / BLOCK_COLUMNS),
        )
    }

    fn block_count(&self) -> i64 {
        let ((r1, r2), (c1, c2)) = self.blocks();
        (r2 - r1 + 1) as i64 * (c2 - c1 + 1) as i64
    }
}

/// What one formula reads.
#[derive(Default, Debug, PartialEq)]
pub(crate) struct Reads {
    cells: Vec<CellKey>,
    ranges: Vec<Rect>,
    volatile: bool,
}

/// A range some formulas read.
struct RangeEntry {
    rect: Rect,
    readers: Vec<CellKey>,
    /// The search that last went through this range. A range whose readers
    /// were already given in a search is not given again.
    visited: u32,
}

/// The formulas of a workbook and what each of them reads.
#[derive(Default)]
pub(crate) struct Dependencies {
    reads: HashMap<CellKey, Reads, KeyBuild>,
    cell_readers: HashMap<CellKey, Vec<CellKey>, KeyBuild>,
    /// Ranges by id. A range no formula reads any more leaves a free slot.
    ranges: Vec<RangeEntry>,
    free: Vec<usize>,
    range_ids: HashMap<Rect, usize>,
    /// `(sheet, block row, block column)` to the ranges overlapping it.
    blocks: HashMap<(u32, i32, i32), Vec<usize>, KeyBuild>,
    wide: Vec<usize>,
    volatile: HashSet<CellKey, KeyBuild>,
    search: u32,
}

impl Dependencies {
    /// The dependencies of every formula of the workbook.
    pub(crate) fn of(model: &Model) -> Dependencies {
        let mut dependencies = Dependencies::default();
        for (sheet, worksheet) in model.workbook.worksheets.iter().enumerate() {
            if worksheet.shared_formulas.is_empty() {
                continue;
            }
            let sheet = sheet as u32;
            for (row, column, cell) in worksheet.sheet_data.cells() {
                if let Some(f) = cell.get_formula() {
                    let reads =
                        reads_of_formula(model, CellReferenceIndex { sheet, row, column }, f);
                    dependencies.insert((sheet, row, column), reads);
                }
            }
        }
        dependencies
    }

    /// How many formulas there are.
    pub(crate) fn len(&self) -> usize {
        self.reads.len()
    }

    /// The volatile formulas.
    pub(crate) fn volatile(&self) -> impl Iterator<Item = &CellKey> {
        self.volatile.iter()
    }

    /// Brings the formula at `cell` up to date: `formula` is the index of its
    /// parsed formula, `None` if the cell holds no formula now.
    pub(crate) fn update(&mut self, model: &Model, cell: CellKey, formula: Option<i32>) {
        let (sheet, row, column) = cell;
        let reads =
            formula.map(|f| reads_of_formula(model, CellReferenceIndex { sheet, row, column }, f));
        if self.reads.get(&cell) == reads.as_ref() {
            return;
        }
        self.remove(cell);
        if let Some(reads) = reads {
            self.insert(cell, reads);
        }
    }

    fn insert(&mut self, cell: CellKey, reads: Reads) {
        for &read in &reads.cells {
            self.cell_readers.entry(read).or_default().push(cell);
        }
        for &rect in &reads.ranges {
            let id = self.range_id(rect);
            self.ranges[id].readers.push(cell);
        }
        if reads.volatile {
            self.volatile.insert(cell);
        }
        self.reads.insert(cell, reads);
    }

    fn remove(&mut self, cell: CellKey) {
        let Some(reads) = self.reads.remove(&cell) else {
            return;
        };
        for read in &reads.cells {
            if let Some(readers) = self.cell_readers.get_mut(read) {
                if let Some(i) = readers.iter().position(|r| *r == cell) {
                    readers.swap_remove(i);
                }
                if readers.is_empty() {
                    self.cell_readers.remove(read);
                }
            }
        }
        for rect in &reads.ranges {
            let Some(&id) = self.range_ids.get(rect) else {
                continue;
            };
            let readers = &mut self.ranges[id].readers;
            if let Some(i) = readers.iter().position(|r| *r == cell) {
                readers.swap_remove(i);
            }
            if readers.is_empty() {
                self.drop_range(id);
            }
        }
        self.volatile.remove(&cell);
    }

    /// The id of a range, listed in its blocks if it is new.
    fn range_id(&mut self, rect: Rect) -> usize {
        if let Some(&id) = self.range_ids.get(&rect) {
            return id;
        }
        let entry = RangeEntry {
            rect,
            readers: Vec::new(),
            visited: 0,
        };
        let id = match self.free.pop() {
            Some(id) => {
                self.ranges[id] = entry;
                id
            }
            None => {
                self.ranges.push(entry);
                self.ranges.len() - 1
            }
        };
        self.range_ids.insert(rect, id);
        if rect.block_count() > MAX_BLOCKS {
            self.wide.push(id);
        } else {
            let ((r1, r2), (c1, c2)) = rect.blocks();
            for block_row in r1..=r2 {
                for block_column in c1..=c2 {
                    self.blocks
                        .entry((rect.sheet, block_row, block_column))
                        .or_default()
                        .push(id);
                }
            }
        }
        id
    }

    fn drop_range(&mut self, id: usize) {
        let rect = self.ranges[id].rect;
        self.range_ids.remove(&rect);
        if rect.block_count() > MAX_BLOCKS {
            self.wide.retain(|wide| *wide != id);
        } else {
            let ((r1, r2), (c1, c2)) = rect.blocks();
            for block_row in r1..=r2 {
                for block_column in c1..=c2 {
                    let key = (rect.sheet, block_row, block_column);
                    if let Some(ids) = self.blocks.get_mut(&key) {
                        ids.retain(|listed| *listed != id);
                        if ids.is_empty() {
                            self.blocks.remove(&key);
                        }
                    }
                }
            }
        }
        self.free.push(id);
    }

    /// Starts a search: the ranges gone through by earlier searches can give
    /// their readers again.
    pub(crate) fn start_search(&mut self) {
        self.search = self.search.wrapping_add(1);
        if self.search == 0 {
            for entry in &mut self.ranges {
                entry.visited = 0;
            }
            self.search = 1;
        }
    }

    /// Gives every formula that reads a cell of `area` to `found`. Within one
    /// search, the readers of a range are given only the first time.
    pub(crate) fn readers_of(&mut self, area: Rect, found: &mut impl FnMut(CellKey)) {
        if area.is_cell() {
            let key = (area.sheet, area.row1, area.column1);
            if let Some(readers) = self.cell_readers.get(&key) {
                readers.iter().copied().for_each(&mut *found);
            }
        } else if area.cell_count() <= self.cell_readers.len() as i64 {
            for row in area.row1..=area.row2 {
                for column in area.column1..=area.column2 {
                    if let Some(readers) = self.cell_readers.get(&(area.sheet, row, column)) {
                        readers.iter().copied().for_each(&mut *found);
                    }
                }
            }
        } else {
            for (key, readers) in &self.cell_readers {
                if area.contains(*key) {
                    readers.iter().copied().for_each(&mut *found);
                }
            }
        }
        let search = self.search;
        let mut visit = |entry: &mut RangeEntry| {
            if entry.visited != search && entry.rect.intersects(&area) {
                entry.visited = search;
                entry.readers.iter().copied().for_each(&mut *found);
            }
        };
        for &id in &self.wide {
            visit(&mut self.ranges[id]);
        }
        if area.block_count() > self.blocks.len() as i64 {
            for ids in self.blocks.values() {
                for &id in ids {
                    visit(&mut self.ranges[id]);
                }
            }
        } else {
            let ((r1, r2), (c1, c2)) = area.blocks();
            for block_row in r1..=r2 {
                for block_column in c1..=c2 {
                    if let Some(ids) = self.blocks.get(&(area.sheet, block_row, block_column)) {
                        for &id in ids {
                            visit(&mut self.ranges[id]);
                        }
                    }
                }
            }
        }
    }
}

/// Functions whose value can change with nothing they read changing, or that
/// read cells their arguments do not name.
fn is_volatile(function: &Function) -> bool {
    matches!(
        function,
        Function::Now
            | Function::Today
            | Function::Rand
            | Function::Randbetween
            | Function::Randarray
            | Function::Offset
            | Function::Indirect
            | Function::Cell
            | Function::Info
            // It leaves out hidden rows, and hiding a row writes no cell.
            | Function::Subtotal
    )
}

/// What the formula with parsed index `f` reads when it runs in `cell`.
fn reads_of_formula(model: &Model, cell: CellReferenceIndex, f: i32) -> Reads {
    let mut reads = Reads::default();
    match model
        .parsed_formulas
        .get(cell.sheet as usize)
        .and_then(|formulas| formulas.get(f as usize))
    {
        Some((node, _)) => add_reads(model, cell, node, &mut reads),
        None => reads.volatile = true,
    }
    reads.cells.sort_unstable();
    reads.cells.dedup();
    reads
        .ranges
        .sort_unstable_by_key(|r| (r.sheet, r.row1, r.column1, r.row2, r.column2));
    reads.ranges.dedup();
    reads
}

fn add_reads(model: &Model, cell: CellReferenceIndex, node: &Node, reads: &mut Reads) {
    match node {
        Node::ReferenceKind { .. } | Node::RangeKind { .. } => {
            if let Some(rect) = resolve(model, cell, node) {
                if rect.is_cell() {
                    reads.cells.push((rect.sheet, rect.row1, rect.column1));
                } else {
                    reads.ranges.push(rect);
                }
            }
        }
        // `A1:B2` from two references is the rectangle around them. Built from
        // anything else (`A1:INDEX(...)`), it is only known when it runs.
        Node::OpRangeKind { left, right } => {
            match (resolve(model, cell, left), resolve(model, cell, right)) {
                (Some(a), Some(b)) if a.sheet == b.sheet => reads.ranges.push(Rect {
                    sheet: a.sheet,
                    row1: a.row1.min(b.row1),
                    column1: a.column1.min(b.column1),
                    row2: a.row2.max(b.row2),
                    column2: a.column2.max(b.column2),
                }),
                _ => {
                    reads.volatile = true;
                    add_reads(model, cell, left, reads);
                    add_reads(model, cell, right, reads);
                }
            }
        }
        Node::FunctionKind { kind, args } => {
            if is_volatile(kind) {
                reads.volatile = true;
            }
            for arg in args {
                add_reads(model, cell, arg, reads);
            }
        }
        // A LAMBDA bound by LET is in the formula; one held by a defined name
        // reads what its body reads, wherever it is called from.
        Node::NamedFunctionKind { id, args, .. } => {
            if id.is_none() {
                reads.volatile = true;
            }
            for arg in args {
                add_reads(model, cell, arg, reads);
            }
        }
        Node::DefinedNameKind((name, scope, _)) => {
            match model.get_parsed_defined_name(name, *scope) {
                Ok(Some(ParsedDefinedName::CellReference(reference))) => {
                    reads
                        .cells
                        .push((reference.sheet, reference.row, reference.column));
                }
                Ok(Some(ParsedDefinedName::RangeReference(range))) => reads.ranges.push(Rect {
                    sheet: range.left.sheet,
                    row1: range.left.row.min(range.right.row),
                    column1: range.left.column.min(range.right.column),
                    row2: range.left.row.max(range.right.row),
                    column2: range.left.column.max(range.right.column),
                }),
                Ok(Some(ParsedDefinedName::LambdaDefinition(..))) => reads.volatile = true,
                // A name that is not a reference, or no name: an error that
                // only redefining the name changes, which evaluates everything.
                Ok(Some(ParsedDefinedName::InvalidDefinedNameFormula)) | Ok(None) => {}
                Err(_) => reads.volatile = true,
            }
        }
        Node::TableNameKind(_) => reads.volatile = true,
        // `A1#` reads the anchor, and the area it spills to with it: a change
        // of that area evaluates everything (see `Model::evaluate`).
        Node::SpillRangeOperator { child } | Node::ImplicitIntersection { child, .. } => {
            add_reads(model, cell, child, reads)
        }
        Node::LambdaDefKind { body, .. } => add_reads(model, cell, body, reads),
        Node::LambdaCallKind { lambda, args } => {
            add_reads(model, cell, lambda, reads);
            for arg in args {
                add_reads(model, cell, arg, reads);
            }
        }
        Node::OpConcatenateKind { left, right }
        | Node::OpSumKind { left, right, .. }
        | Node::OpProductKind { left, right, .. }
        | Node::OpPowerKind { left, right }
        | Node::CompareKind { left, right, .. } => {
            add_reads(model, cell, left, reads);
            add_reads(model, cell, right, reads);
        }
        Node::UnaryKind { right, .. } => add_reads(model, cell, right, reads),
        Node::BooleanKind(_)
        | Node::NumberKind(_)
        | Node::StringKind(_)
        | Node::WrongReferenceKind { .. }
        | Node::WrongRangeKind { .. }
        | Node::ArrayKind(_)
        | Node::NamedVariableKind { .. }
        | Node::ErrorKind(_)
        | Node::ParseErrorKind { .. }
        | Node::EmptyArgKind => {}
    }
}

/// The cells a reference or a range in `cell` points to, `None` if the node is
/// neither or points outside the workbook.
fn resolve(model: &Model, cell: CellReferenceIndex, node: &Node) -> Option<Rect> {
    let place = |absolute: bool, value: i32, base: i32| if absolute { value } else { value + base };
    let rect = match node {
        Node::ReferenceKind {
            sheet_index,
            absolute_row,
            absolute_column,
            row,
            column,
            ..
        } => {
            let row = place(*absolute_row, *row, cell.row);
            let column = place(*absolute_column, *column, cell.column);
            Rect::cell((*sheet_index, row, column))
        }
        Node::RangeKind {
            sheet_index,
            absolute_row1,
            absolute_column1,
            row1,
            column1,
            absolute_row2,
            absolute_column2,
            row2,
            column2,
            ..
        } => {
            let r1 = place(*absolute_row1, *row1, cell.row);
            let r2 = place(*absolute_row2, *row2, cell.row);
            let c1 = place(*absolute_column1, *column1, cell.column);
            let c2 = place(*absolute_column2, *column2, cell.column);
            Rect {
                sheet: *sheet_index,
                row1: r1.min(r2),
                column1: c1.min(c2),
                row2: r1.max(r2),
                column2: c1.max(c2),
            }
        }
        _ => return None,
    };
    let inside = (rect.sheet as usize) < model.workbook.worksheets.len()
        && rect.row1 >= 1
        && rect.column1 >= 1
        && rect.row2 <= LAST_ROW
        && rect.column2 <= LAST_COLUMN;
    inside.then_some(rect)
}
