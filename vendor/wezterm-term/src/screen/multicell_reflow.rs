use super::*;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) struct ReflowPoint {
    pub source: (usize, usize),
    pub target: (usize, usize),
}

impl ReflowPoint {
    pub(super) fn new(row: usize, col: usize) -> Self {
        Self {
            source: (row, col),
            target: (0, 0),
        }
    }
}

// Only use the rectangle-aware path for groups of rows connected by soft
// wrapping or a text rectangle. Ordinary and DEC-sized rows keep their
// existing resize behavior.
pub(super) fn multicell_regions(lines: &VecDeque<Line>) -> Vec<Range<usize>> {
    if !lines.iter().any(Line::has_multicell_text) {
        return Vec::new();
    }
    let mut regions = Vec::new();
    let mut start = 0;
    let mut has_multicell = false;
    for (row, line) in lines.iter().enumerate() {
        let mut crosses_next = false;
        if line.has_multicell_text() {
            for cell in line.visible_cells() {
                if let Some(slice) = cell.attrs().multicell_row() {
                    has_multicell = true;
                    crosses_next |= slice.offset() + 1 < slice.run().scale();
                }
            }
        }
        let wraps = line_width_scale(line) == 1
            && line.last_cell_was_wrapped()
            && lines
                .get(row + 1)
                .is_some_and(|next| line_width_scale(next) == 1);
        if row + 1 == lines.len() || !(crosses_next || wraps) {
            if has_multicell {
                regions.push(start..row + 1);
            }
            start = row + 1;
            has_multicell = false;
        }
    }
    regions
}

struct ReflowRow {
    line: Line,
    reserved: Vec<Range<usize>>,
}

type Rectangles = BTreeMap<(isize, usize), (isize, usize)>;

struct Layout {
    rows: VecDeque<ReflowRow>,
    first: usize,
    row: usize,
    col: usize,
    cols: usize,
    height: usize,
    capacity: usize,
    seqno: SequenceNo,
    rectangles: Rectangles,
}

impl Layout {
    fn line_at(&mut self, row: usize) -> &mut ReflowRow {
        let offset = row - self.first;
        while self.rows.len() <= offset {
            self.rows.push_back(ReflowRow {
                line: Line::new(self.seqno),
                reserved: Vec::new(),
            });
        }
        &mut self.rows[offset]
    }

    fn advance_row(&mut self, wrapped: bool) {
        let seqno = self.seqno;
        self.line_at(self.row)
            .line
            .set_last_cell_was_wrapped(wrapped, seqno);
        self.row += 1;
        self.col = 0;
        // Keep the active row and any rows reserved below it. Rows already
        // written need only live as long as the new scrollback can retain them.
        while self.rows.len() > self.capacity && self.first < self.row {
            self.rows.pop_front();
            self.first += 1;
        }
    }

    fn find_slot(&mut self, width: usize, height: usize) -> (usize, usize) {
        loop {
            if self.col + width > self.cols {
                self.advance_row(true);
            }
            let end = self.col + width;
            let mut blocked_until = self.col;
            for row in self.row..self.row + height {
                if let Some(row) = self.rows.get(row - self.first) {
                    let next = row.reserved.partition_point(|part| part.end <= self.col);
                    if let Some(part) = row.reserved.get(next).filter(|part| part.start < end) {
                        blocked_until = blocked_until.max(part.end);
                    }
                }
            }
            if blocked_until == self.col {
                return (self.row, self.col);
            }
            self.col = blocked_until;
        }
    }

    fn rectangle(
        &mut self,
        slice: &wezterm_cell::MulticellRow,
        attrs: &CellAttributes,
        bidi: (bool, wezterm_bidi::ParagraphDirectionHint),
    ) -> Option<(isize, usize)> {
        let block = slice.run();
        let height = usize::from(block.scale());
        let first = usize::from(slice.offset());
        let width = block.columns();
        if height > self.height || width > self.cols {
            return None;
        }
        let (top, col) = self.find_slot(width, height - first);
        let seqno = self.seqno;
        for offset in first..height {
            let row = self.line_at(top + offset - first);
            let next = row.reserved.partition_point(|range| range.start < col);
            row.reserved.insert(next, col..col + width);
            let mut attrs = attrs.clone();
            attrs
                .set_wrapped(false)
                .set_multicell_row(Some(block.row(offset as u8)));
            if offset != first {
                attrs.clear_images();
            }
            row.line.set_bidi_info(bidi.0, bidi.1, seqno);
            let content = if offset == 0 { block.text() } else { " " };
            row.line
                .set_cell_grapheme(col, content, width, attrs, seqno);
        }
        if first == 0 {
            self.col += width;
        }
        Some((top as isize - first as isize, col))
    }

    fn place(
        &mut self,
        cell: &crate::CellRef<'_>,
        source_row: usize,
        region_start: usize,
        payload: bool,
        bidi: (bool, wezterm_bidi::ParagraphDirectionHint),
    ) -> Option<(usize, usize)> {
        if let Some(slice) = cell.attrs().multicell_row() {
            let source_top = source_row as isize - isize::from(slice.offset());
            let key = (source_top, cell.cell_index());
            // A retained history prefix can begin below a rectangle's original
            // top. Reflow its surviving slices without inventing another glyph.
            if (slice.offset() == 0 || source_top < region_start as isize)
                && !self.rectangles.contains_key(&key)
            {
                if let Some(target) = self.rectangle(slice, cell.attrs(), bidi) {
                    self.rectangles.insert(key, target);
                }
            }
            let &(top, col) = self.rectangles.get(&key)?;
            let row = (top + isize::from(slice.offset())) as usize;
            if slice.offset() > 0 && row >= self.first {
                let seqno = self.seqno;
                let line = &mut self.line_at(row).line;
                // Transfer per-row styling and images after reserving the
                // rectangle from the anchor. Keep any new wrapping marker.
                let wrapped = line
                    .get_cell(col)
                    .is_some_and(|cell| cell.attrs().wrapped());
                let mut attrs = cell.attrs().clone();
                attrs.set_wrapped(wrapped);
                line.set_cell_grapheme(col, " ", slice.run().columns(), attrs, seqno);
            }
            return Some((row, col));
        }
        if !payload || (cell.attrs().explicit_width() && cell.width() > self.cols) {
            return None;
        }
        let width = cell.width().min(self.cols);
        let (row, col) = self.find_slot(width, 1);
        let seqno = self.seqno;
        let line = &mut self.line_at(row).line;
        line.set_bidi_info(bidi.0, bidi.1, seqno);
        let mut attrs = cell.attrs().clone();
        attrs.set_wrapped(false);
        line.set_cell_grapheme(col, cell.str(), width, attrs, seqno);
        self.col += width;
        Some((row, col))
    }
}

pub(super) fn reflow_multicell_rows(
    mut source: impl Iterator<Item = (usize, Line)>,
    cols: usize,
    height: usize,
    capacity: usize,
    seqno: SequenceNo,
    mut points: &mut [ReflowPoint],
) -> (usize, VecDeque<Line>) {
    let mut layout = Layout {
        rows: VecDeque::new(),
        first: 0,
        row: 0,
        col: 0,
        cols,
        height,
        capacity,
        seqno,
        rectangles: BTreeMap::new(),
    };
    let first = source
        .next()
        .expect("a region contains at least one source row");
    let region_start = first.0;
    let mut previous_wrapped = true;
    let blank = CellAttributes::blank();
    for (source_row, line) in std::iter::once(first).chain(source) {
        let bidi = line.bidi_info();
        let cells: Vec<_> = line.visible_cells().collect();
        let last = cells
            .iter()
            .rposition(|cell| match cell.attrs().multicell_row() {
                Some(slice) => slice.offset() == 0,
                None => cell.str() != " " || cell.attrs().clone().set_wrapped(false) != &blank,
            });
        if !previous_wrapped {
            let mut has_slices = false;
            let mut reserved = layout.row;
            if last.is_none() {
                for cell in &cells {
                    if let Some(slice) = cell.attrs().multicell_row() {
                        has_slices = true;
                        let offset = isize::from(slice.offset());
                        let key = (source_row as isize - offset, cell.cell_index());
                        if let Some((row, _)) = layout.rectangles.get(&key) {
                            reserved = reserved.max((row + offset) as usize);
                        }
                    }
                }
            }
            if has_slices {
                // Layout created these rows with their rectangle. When that
                // rectangle was discarded, its empty slices require no rows.
                while layout.row < reserved {
                    layout.advance_row(false);
                }
            } else {
                layout.advance_row(false);
            }
        }
        previous_wrapped = line.last_cell_was_wrapped();

        let count = points.partition_point(|point| point.source.0 == source_row);
        let (mut row_points, rest) = points.split_at_mut(count);
        points = rest;
        for (index, cell) in cells.iter().enumerate() {
            let col = cell.cell_index();
            let target = layout.place(
                cell,
                source_row,
                region_start,
                last.is_some_and(|last| index <= last),
                bidi,
            );
            let end = row_points.partition_point(|point| point.source.1 < col + cell.width());
            for point in &mut row_points[..end] {
                point.target = target.map_or((layout.row, layout.col), |(row, start)| {
                    (row, (start + point.source.1.saturating_sub(col)).min(cols))
                });
            }
            row_points = &mut row_points[end..];
        }
        for point in row_points {
            point.target = (layout.row, layout.col);
        }
        layout.line_at(layout.row);
        layout
            .rectangles
            .retain(|(row, _), _| *row + 7 > source_row as isize);
    }
    let excess = layout.rows.len().saturating_sub(capacity);
    layout.rows.drain(..excess);
    (
        layout.first + excess,
        layout.rows.into_iter().map(|row| row.line).collect(),
    )
}
