//! The line-number gutter: one column of new-file numbers, or two columns
//! (old, new) as GitHub, GitLab and Gerrit show them.
//!
//! In two columns a deleted line shows only its old number, an added line only
//! its new number, and a context line both. The terminal view falls back to the
//! single column when two columns would leave the code less than
//! `MIN_CONTENT_WIDTH_FOR_TWO_COLUMNS` columns; `--print` and `--html` have no
//! width to fit and always use two columns when the diff has old numbers.

use crate::diff::{DiffLine, LineSource};

use super::PREFIX_CHAR_WIDTH;

/// Narrowest code area (columns left after borders, gutter and the
/// `+ C ` prefix) at which the terminal view still shows two number columns.
/// 40 is the narrowest width the diff view's own layout tests treat as usable.
pub const MIN_CONTENT_WIDTH_FOR_TWO_COLUMNS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Gutter {
    /// Digits of the widest old number; 0 in single-column mode.
    old_digits: usize,
    /// Digits of the widest number of the visible column: new numbers in two
    /// columns, `line_number` in one.
    new_digits: usize,
}

/// The (old, new) numbers a line shows in two columns.
pub fn two_column_numbers(line: &DiffLine) -> (Option<usize>, Option<usize>) {
    if matches!(line.source, LineSource::FileHeader | LineSource::Elided) {
        return (None, None);
    }
    let deleted = line.prefix == '-';
    let old = line.old_line().or(if deleted { line.line_number } else { None });
    let new = if deleted { None } else { line.line_number };
    (old, new)
}

fn digits(n: usize) -> usize {
    n.to_string().len()
}

impl Gutter {
    /// Measure `lines`. `available_width` is the room between the borders in
    /// the terminal view; `None` (print, html) never falls back.
    pub fn measure<'a>(
        lines: impl IntoIterator<Item = &'a DiffLine>,
        available_width: Option<usize>,
    ) -> Self {
        let (mut max_line, mut max_old, mut max_new) = (0, 0, 0);
        for line in lines {
            max_line = max_line.max(line.line_number.unwrap_or(0));
            let (old, new) = two_column_numbers(line);
            max_old = max_old.max(old.unwrap_or(0));
            max_new = max_new.max(new.unwrap_or(0));
        }
        let single =
            Self { old_digits: 0, new_digits: if max_line > 0 { digits(max_line) } else { 0 } };
        if max_old == 0 || max_new == 0 {
            return single;
        }
        let two = Self { old_digits: digits(max_old), new_digits: digits(max_new) };
        match available_width {
            Some(w)
                if w.saturating_sub(two.tui_width() + 1 + PREFIX_CHAR_WIDTH)
                    < MIN_CONTENT_WIDTH_FOR_TWO_COLUMNS =>
            {
                single
            }
            _ => two,
        }
    }

    pub fn is_two_column(&self) -> bool {
        self.old_digits > 0
    }

    /// Number cells per row in `--html`: 2 in two columns, else 1.
    pub fn ln_cells(&self) -> usize {
        if self.is_two_column() { 2 } else { 1 }
    }

    pub fn old_digits(&self) -> usize {
        self.old_digits
    }

    pub fn new_digits(&self) -> usize {
        self.new_digits
    }

    /// Terminal-view width of the numbers: the old column (padded one wider),
    /// a space and the new column. The cell adds one trailing space. 0 means no
    /// gutter at all.
    pub fn tui_width(&self) -> usize {
        if self.is_two_column() {
            self.old_digits + 1 + 1 + self.new_digits
        } else if self.new_digits > 0 {
            self.new_digits + 1
        } else {
            0
        }
    }

    /// The gutter text in front of a terminal-view row, trailing space included.
    pub fn tui_cell(&self, line: &DiffLine) -> String {
        let width = self.tui_width();
        if self.is_two_column() {
            let (old, new) = two_column_numbers(line);
            if old.is_none() && new.is_none() {
                return " ".repeat(width + 1);
            }
            let old = old.map(|n| n.to_string()).unwrap_or_default();
            let new = new.map(|n| n.to_string()).unwrap_or_default();
            return format!(
                "{old:>ow$} {new:>nw$} ",
                ow = self.old_digits + 1,
                nw = self.new_digits
            );
        }
        match line.line_number {
            Some(num) => format!("{num:>width$} "),
            None if width > 0 => " ".repeat(width + 1),
            None => String::new(),
        }
    }

    /// Width of the `--print` gutter, without its trailing space.
    pub fn print_width(&self) -> usize {
        if self.is_two_column() { self.old_digits + 1 + self.new_digits } else { self.new_digits }
    }

    /// The `--print` gutter text for a line, without its trailing space.
    pub fn print_cell(&self, line: &DiffLine) -> String {
        let width = self.print_width();
        if self.is_two_column() {
            let (old, new) = two_column_numbers(line);
            let old = old.map(|n| n.to_string()).unwrap_or_default();
            let new = new.map(|n| n.to_string()).unwrap_or_default();
            return format!("{old:>ow$} {new:>nw$}", ow = self.old_digits, nw = self.new_digits);
        }
        match line.line_number {
            Some(num) => format!("{num:>width$}"),
            None => " ".repeat(width),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(prefix: char, old: Option<usize>, new: Option<usize>) -> DiffLine {
        let source = if prefix == '-' { LineSource::DeletedCommitted } else { LineSource::Base };
        let mut line = DiffLine::new(source, "x".into(), prefix, new);
        line.old_line_number = old.map(|n| n as u32);
        line
    }

    fn sample() -> Vec<DiffLine> {
        vec![
            line(' ', Some(1), Some(1)),
            line('-', Some(2), None),
            line('+', None, Some(2)),
            line(' ', Some(3), Some(3)),
        ]
    }

    #[test]
    fn deleted_lines_show_old_added_show_new_context_shows_both() {
        let lines = sample();
        assert_eq!(two_column_numbers(&lines[0]), (Some(1), Some(1)));
        assert_eq!(two_column_numbers(&lines[1]), (Some(2), None));
        assert_eq!(two_column_numbers(&lines[2]), (None, Some(2)));
    }

    #[test]
    fn a_whole_file_deletion_keeps_its_numbers_as_old() {
        let del = DiffLine::new(LineSource::DeletedCommitted, "x".into(), '-', Some(7));
        assert_eq!(two_column_numbers(&del), (Some(7), None));
    }

    #[test]
    fn headers_and_elided_markers_have_no_numbers() {
        assert_eq!(two_column_numbers(&DiffLine::file_header("a.rs")), (None, None));
    }

    #[test]
    fn cells_are_the_same_width_on_every_row() {
        let lines = sample();
        let gutter = Gutter::measure(&lines, None);
        assert!(gutter.is_two_column());
        for l in &lines {
            assert_eq!(gutter.tui_cell(l).chars().count(), gutter.tui_width() + 1);
            assert_eq!(gutter.print_cell(l).chars().count(), gutter.print_width());
        }
        assert_eq!(gutter.tui_cell(&lines[0]), " 1 1 ");
        assert_eq!(gutter.tui_cell(&lines[1]), " 2   ");
        assert_eq!(gutter.tui_cell(&lines[2]), "   2 ");
        assert_eq!(gutter.print_cell(&lines[1]), "2  ");
    }

    #[test]
    fn widths_follow_the_widest_old_and_new_numbers() {
        let lines = vec![line(' ', Some(120), Some(9)), line('+', None, Some(1000))];
        let gutter = Gutter::measure(&lines, None);
        assert_eq!((gutter.old_digits(), gutter.new_digits()), (3, 4));
        assert_eq!(gutter.tui_width(), 3 + 1 + 1 + 4);
    }

    #[test]
    fn no_old_numbers_means_the_single_column_of_today() {
        let lines = vec![line('+', None, Some(1)), line('+', None, Some(12))];
        let gutter = Gutter::measure(&lines, None);
        assert!(!gutter.is_two_column());
        assert_eq!(gutter.tui_width(), 3);
        assert_eq!(gutter.tui_cell(&lines[0]), "  1 ");
        assert_eq!(gutter.print_width(), 2);
    }

    #[test]
    fn no_numbers_at_all_means_no_gutter() {
        let lines = vec![DiffLine::file_header("a")];
        let gutter = Gutter::measure(&lines, None);
        assert_eq!(gutter.tui_width(), 0);
        assert_eq!(gutter.tui_cell(&lines[0]), "");
    }

    #[test]
    fn a_narrow_terminal_falls_back_to_one_column_at_the_threshold() {
        let lines = sample();
        // Two columns of one digit: tui_width 4, so gutter 5 + prefix 4 = 9 columns.
        let fits = MIN_CONTENT_WIDTH_FOR_TWO_COLUMNS + 9;
        assert!(Gutter::measure(&lines, Some(fits)).is_two_column());
        let fallback = Gutter::measure(&lines, Some(fits - 1));
        assert!(!fallback.is_two_column());
        assert_eq!(fallback.tui_cell(&lines[0]), " 1 ");
        assert_eq!(fallback.tui_cell(&lines[1]), "   ", "deleted lines carry no new number");
    }

    #[test]
    fn only_deletions_with_no_new_numbers_stay_single() {
        let lines = vec![line('-', Some(1), None)];
        assert!(!Gutter::measure(&lines, None).is_two_column());
    }
}
