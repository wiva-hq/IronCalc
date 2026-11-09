//! An index of the values of a lookup range, for exact matches.
//!
//! VLOOKUP, HLOOKUP, MATCH, XLOOKUP and XMATCH in an exact mode look for the
//! first (or last) position of a value in a row or a column. Searching the
//! range cell by cell costs its length for every formula; a sheet with a
//! lookup per row pays that once per row. The index maps every value of the
//! range to the first and last position where it is, so a search costs a hash
//! or a binary search instead.
//!
//! An evaluation builds the index of a range the first time a lookup asks for
//! it and keeps it until the evaluation ends (see `Model::lookup_index`).
//! Within one evaluation the values of a range do not change: a spill that
//! would change one restarts the evaluation, which starts with no index.
//!
//! The index answers with exactly what the linear search would find, under the
//! same rules (see `Equality`). What it cannot answer (wildcards, approximate
//! modes, targets that are not plain values) is left to the linear search.

use std::collections::HashMap;
use std::sync::Arc;

use crate::calc_result::CalcResult;
use crate::expressions::types::CellReferenceIndex;
use crate::model::Model;
use crate::number_format::to_excel_precision;

/// The longest text target the index answers for. The wildcard search turns
/// the target into a regular expression, which has a size limit.
const MAX_TEXT: usize = 10_000;

/// The rule a lookup compares values with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Equality {
    /// `compare_values(value, target) == 0`: numbers to 15 digits, text
    /// ignoring case, an empty cell equal to 0, "" and FALSE. XLOOKUP, XMATCH
    /// and the non-text targets of VLOOKUP and HLOOKUP.
    Compare,
    /// `values_are_equal(value, target)`: numbers as they are, an empty cell
    /// equal only to an empty cell. The non-text targets of MATCH.
    Equal,
    /// The wildcard search of a text without wildcards: text only, ignoring
    /// case. The text targets of VLOOKUP, HLOOKUP and MATCH.
    Text,
}

/// The first and last position of a value, 0-based.
type Span = (u32, u32);

fn widen(found: &mut Option<Span>, span: Span) {
    *found = Some(match *found {
        None => span,
        Some((first, last)) => (first.min(span.0), last.max(span.1)),
    });
}

/// Numbers in ascending order, each once, with where they are.
#[derive(Default)]
struct Numbers(Vec<(f64, Span)>);

impl Numbers {
    fn from(mut numbers: Vec<(f64, u32)>) -> Numbers {
        numbers.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut list: Vec<(f64, Span)> = Vec::with_capacity(numbers.len());
        for (value, position) in numbers {
            match list.last_mut() {
                Some((last, span)) if last.to_bits() == value.to_bits() => {
                    span.1 = span.1.max(position);
                }
                _ => list.push((value, (position, position))),
            }
        }
        Numbers(list)
    }

    /// Widens `found` with every number `n` for which `(target - n).abs()` is
    /// below `f64::EPSILON`, the test both comparisons use. Such numbers lie
    /// within two epsilons of the target, so only that stretch is tested.
    fn find(&self, target: f64, found: &mut Option<Span>) {
        let low = target - 2.0 * f64::EPSILON;
        let high = target + 2.0 * f64::EPSILON;
        let start = self.0.partition_point(|(value, _)| *value < low);
        for (value, span) in &self.0[start..] {
            if *value > high {
                break;
            }
            if (target - value).abs() < f64::EPSILON {
                widen(found, *span);
            }
        }
    }
}

/// The values of a row or a column, by kind.
pub(crate) struct LookupIndex {
    /// Numbers as they are.
    numbers: Numbers,
    /// Numbers rounded to 15 digits, as `compare_values` sees them.
    rounded: Numbers,
    /// Text in upper case, as `compare_values` and `values_are_equal` see it.
    upper: HashMap<String, Span>,
    /// Text in lower case, as the wildcard search sees it.
    lower: HashMap<String, Span>,
    /// FALSE and TRUE.
    booleans: [Option<Span>; 2],
    /// Empty cells.
    empty: Option<Span>,
}

impl LookupIndex {
    /// The index of `values`. `None` if one of them is not a plain value (a
    /// number, text, a boolean, an error or an empty cell).
    pub(crate) fn new(values: &[CalcResult]) -> Option<LookupIndex> {
        let mut numbers = Vec::new();
        let mut upper: HashMap<String, Span> = HashMap::new();
        let mut lower: HashMap<String, Span> = HashMap::new();
        let mut booleans = [None, None];
        let mut empty = None;
        for (position, value) in values.iter().enumerate() {
            let position = position as u32;
            match value {
                CalcResult::Number(number) => {
                    if !number.is_nan() {
                        numbers.push((*number, position));
                    }
                }
                CalcResult::String(text) => {
                    widen_entry(&mut upper, text.to_uppercase(), position);
                    widen_entry(&mut lower, text.to_lowercase(), position);
                }
                CalcResult::Boolean(boolean) => {
                    widen(&mut booleans[*boolean as usize], (position, position));
                }
                CalcResult::EmptyCell => widen(&mut empty, (position, position)),
                // An error equals no target a lookup searches for.
                CalcResult::Error { .. } => {}
                CalcResult::Range { .. }
                | CalcResult::EmptyArg
                | CalcResult::Array(_)
                | CalcResult::Lambda(_) => return None,
            }
        }
        let rounded = numbers
            .iter()
            .map(|(number, position)| (to_excel_precision(*number, 15), *position))
            .collect();
        Some(LookupIndex {
            numbers: Numbers::from(numbers),
            rounded: Numbers::from(rounded),
            upper,
            lower,
            booleans,
            empty,
        })
    }

    /// Whether the index can answer a search for `target` under `equality`.
    /// When it cannot, the search is linear and building the index is wasted.
    pub(crate) fn answers(target: &CalcResult, equality: Equality) -> bool {
        match (equality, target) {
            // A text too long for a regular expression matches nothing there.
            (Equality::Text, CalcResult::String(text)) => {
                !has_wildcards(text) && text.len() <= MAX_TEXT
            }
            (Equality::Text, _) => false,
            (
                Equality::Compare,
                CalcResult::Number(_)
                | CalcResult::String(_)
                | CalcResult::Boolean(_)
                | CalcResult::EmptyCell,
            ) => true,
            (Equality::Equal, CalcResult::Number(_) | CalcResult::Boolean(_)) => true,
            _ => false,
        }
    }

    /// The first and last 0-based position equal to `target` under
    /// `equality`, or `None` if there is none. Call only when `answers` is
    /// true.
    pub(crate) fn find(&self, target: &CalcResult, equality: Equality) -> Option<(usize, usize)> {
        let mut found = None;
        match (equality, target) {
            (Equality::Text, CalcResult::String(text)) => {
                if let Some(span) = self.lower.get(&text.to_lowercase()) {
                    widen(&mut found, *span);
                }
            }
            (Equality::Compare, CalcResult::Number(number)) => {
                let target = to_excel_precision(*number, 15);
                self.rounded.find(target, &mut found);
                // An empty cell compares as 0.
                if target.abs() < f64::EPSILON {
                    self.with_empty(&mut found);
                }
            }
            (Equality::Compare, CalcResult::String(text)) => {
                let text = text.to_uppercase();
                if let Some(span) = self.upper.get(&text) {
                    widen(&mut found, *span);
                }
                // An empty cell compares as "".
                if text.is_empty() {
                    self.with_empty(&mut found);
                }
            }
            (Equality::Compare, CalcResult::Boolean(boolean)) => {
                self.with_boolean(*boolean, &mut found);
                // An empty cell compares as FALSE.
                if !boolean {
                    self.with_empty(&mut found);
                }
            }
            (Equality::Compare, CalcResult::EmptyCell) => {
                self.rounded.find(0.0, &mut found);
                if let Some(span) = self.upper.get("") {
                    widen(&mut found, *span);
                }
                self.with_boolean(false, &mut found);
                self.with_empty(&mut found);
            }
            (Equality::Equal, CalcResult::Number(number)) => self.numbers.find(*number, &mut found),
            (Equality::Equal, CalcResult::Boolean(boolean)) => {
                self.with_boolean(*boolean, &mut found)
            }
            _ => {}
        }
        found.map(|(first, last)| (first as usize, last as usize))
    }

    fn with_boolean(&self, boolean: bool, found: &mut Option<Span>) {
        if let Some(span) = self.booleans[boolean as usize] {
            widen(found, span);
        }
    }

    fn with_empty(&self, found: &mut Option<Span>) {
        if let Some(span) = self.empty {
            widen(found, span);
        }
    }
}

fn widen_entry(map: &mut HashMap<String, Span>, key: String, position: u32) {
    map.entry(key)
        .and_modify(|span| span.1 = position)
        .or_insert((position, position));
}

/// Whether a text has a character the wildcard search gives a meaning to.
fn has_wildcards(text: &str) -> bool {
    text.contains(['*', '?', '~'])
}

impl Model<'_> {
    /// Searches the cells from `left` to `right`, a single row or column, for
    /// `target` with their index. `None` if the index cannot answer: search
    /// linearly then. Otherwise the first and last 0-based position found.
    pub(crate) fn find_with_index(
        &mut self,
        left: CellReferenceIndex,
        right: CellReferenceIndex,
        target: &CalcResult,
        equality: Equality,
    ) -> Option<Option<(usize, usize)>> {
        if !LookupIndex::answers(target, equality) {
            return None;
        }
        let index = self.lookup_index(left, right)?;
        Some(index.find(target, equality))
    }

    /// The index of the cells from `left` to `right`, a single row or column,
    /// built the first time the evaluation pass asks for it. `None` outside a
    /// pass, where there is no pass to keep it for, or if a cell holds
    /// something the index cannot.
    fn lookup_index(
        &mut self,
        left: CellReferenceIndex,
        right: CellReferenceIndex,
    ) -> Option<Arc<LookupIndex>> {
        #[cfg(test)]
        if self.evaluation.without_lookup_index {
            return None;
        }
        if !self.evaluation.in_pass {
            return None;
        }
        let key = (left.sheet, left.row, left.column, right.row, right.column);
        if let Some(index) = self.evaluation.lookup_indexes.get(&key) {
            return index.clone();
        }
        // Reading the cells evaluates them, as the linear search would, so
        // a cycle through the range is found here just the same.
        let values = self.prepare_array(&left, &right, left.column == right.column);
        let index = LookupIndex::new(&values).map(Arc::new);
        // What is read while the pass is abandoned or unwinding is not what
        // the cells hold, and the formula asking is run again anyway.
        if self.evaluation.restart.is_none() && !self.evaluation.unwinding {
            self.evaluation.lookup_indexes.insert(key, index.clone());
        }
        index
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::{Equality, LookupIndex};
    use crate::calc_result::CalcResult;
    use crate::expressions::token::Error;
    use crate::expressions::types::CellReferenceIndex;
    use crate::functions::util::{
        compare_values, from_wildcard_to_regex, result_matches_regex, values_are_equal,
    };

    /// What the linear searches find: the first and last match.
    fn linear(
        values: &[CalcResult],
        target: &CalcResult,
        equality: Equality,
    ) -> Option<(usize, usize)> {
        let matches = |value: &CalcResult| match (equality, target) {
            (Equality::Compare, _) => compare_values(value, target) == 0,
            (Equality::Equal, _) => values_are_equal(value, target),
            (Equality::Text, CalcResult::String(text)) => {
                let regex = from_wildcard_to_regex(&text.to_lowercase(), true).unwrap();
                result_matches_regex(value, &regex)
            }
            (Equality::Text, _) => unreachable!(),
        };
        let first = values.iter().position(matches)?;
        let last = values.iter().rposition(matches)?;
        Some((first, last))
    }

    fn values() -> Vec<CalcResult> {
        let origin = CellReferenceIndex {
            sheet: 0,
            row: 1,
            column: 1,
        };
        let mut values = vec![
            CalcResult::Number(0.0),
            CalcResult::Number(-0.0),
            CalcResult::Number(1.0),
            CalcResult::Number(0.1 + 0.2),
            CalcResult::Number(0.3),
            CalcResult::Number(1e-17),
            CalcResult::Number(2.000000000000001),
            CalcResult::Number(2.0),
            CalcResult::Number(123456789.12345679),
            CalcResult::Number(f64::INFINITY),
            CalcResult::String("Apple".to_string()),
            CalcResult::String("APPLE".to_string()),
            CalcResult::String("".to_string()),
            CalcResult::String("straße".to_string()),
            CalcResult::String("STRASSE".to_string()),
            CalcResult::String("1".to_string()),
            CalcResult::String("a*b".to_string()),
            CalcResult::Boolean(true),
            CalcResult::Boolean(false),
            CalcResult::EmptyCell,
            CalcResult::new_error(Error::NA, origin, String::new()),
            CalcResult::new_error(Error::DIV, origin, String::new()),
        ];
        // Again in another order, so that first and last differ.
        let mut again = values.clone();
        again.reverse();
        values.extend(again);
        values
    }

    #[test]
    fn finds_what_the_linear_search_finds() {
        let values = values();
        let index = LookupIndex::new(&values).unwrap();
        let mut targets = values.clone();
        targets.extend([
            CalcResult::Number(0.30000000000000004),
            CalcResult::Number(1.0 + f64::EPSILON),
            CalcResult::Number(5.0),
            CalcResult::String("apple".to_string()),
            CalcResult::String("pear".to_string()),
            CalcResult::String("STRAßE".to_string()),
            CalcResult::String("a~*b".to_string()),
            CalcResult::String("a?b".to_string()),
        ]);
        for equality in [Equality::Compare, Equality::Equal, Equality::Text] {
            for (number, target) in targets.iter().enumerate() {
                if !LookupIndex::answers(target, equality) {
                    continue;
                }
                assert_eq!(
                    index.find(target, equality),
                    linear(&values, target, equality),
                    "target {number} with {equality:?}"
                );
            }
        }
    }

    #[test]
    fn leaves_wildcards_and_other_targets_to_the_linear_search() {
        assert!(!LookupIndex::answers(
            &CalcResult::String("a*".to_string()),
            Equality::Text
        ));
        assert!(!LookupIndex::answers(
            &CalcResult::String("~".to_string()),
            Equality::Text
        ));
        assert!(!LookupIndex::answers(
            &CalcResult::Number(1.0),
            Equality::Text
        ));
        assert!(!LookupIndex::answers(
            &CalcResult::EmptyArg,
            Equality::Compare
        ));
        assert!(!LookupIndex::answers(
            &CalcResult::String("a".to_string()),
            Equality::Equal
        ));
        assert!(LookupIndex::new(&[CalcResult::EmptyArg]).is_none());
    }
}
