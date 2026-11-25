use crate::line::cellref::CellRef;
use alloc::sync::Arc;
#[cfg(feature = "use_serde")]
use serde::{Deserialize, Serialize};
use wezterm_cell::Cell;

extern crate alloc;
use alloc::vec::Vec;

#[cfg_attr(feature = "use_serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VecStorage {
    cells: Vec<Cell>,
}

impl VecStorage {
    pub(crate) fn new(cells: Vec<Cell>) -> Self {
        Self { cells }
    }

    #[cfg_attr(not(feature = "use_image"), allow(unused_mut, unused_variables))]
    pub(crate) fn set_cell(&mut self, idx: usize, mut cell: Cell, clear_image_placement: bool) {
        #[cfg(feature = "use_image")]
        if !clear_image_placement {
            if let Some(images) = self.cells[idx].attrs().images() {
                for image in images {
                    if image.has_placement_id() {
                        cell.attrs_mut().attach_image(Box::new(image));
                    }
                }
            }
        }
        self.cells[idx] = cell;
    }

    pub(crate) fn scan_and_create_hyperlinks(
        &mut self,
        matches: Vec<crate::hyperlink::RuleMatch>,
    ) -> bool {
        // Match offsets are measured against visible text. A cell can
        // contain multiple graphemes and can cover hidden continuation cells.
        let mut cell_idx = 0;
        let mut byte_start = 0;
        let mut has_implicit_hyperlinks = false;
        while cell_idx < self.cells.len() {
            let cell = &mut self.cells[cell_idx];
            let byte_end = byte_start + cell.str().len();
            if cell.attrs().hyperlink().is_none() {
                if let Some(matched) = matches.iter().find(|m| {
                    byte_start < m.range.end && m.range.start < byte_end
                }) {
                    cell.attrs_mut().set_hyperlink(Some(Arc::clone(&matched.link)));
                    has_implicit_hyperlinks = true;
                }
            }
            byte_start = byte_end;
            cell_idx += cell.width().max(1);
        }

        has_implicit_hyperlinks
    }
}

impl core::ops::Deref for VecStorage {
    type Target = Vec<Cell>;

    fn deref(&self) -> &Vec<Cell> {
        &self.cells
    }
}

impl core::ops::DerefMut for VecStorage {
    fn deref_mut(&mut self) -> &mut Vec<Cell> {
        &mut self.cells
    }
}

/// Iterates over a slice of Cell, yielding only visible cells
pub(crate) struct VecStorageIter<'a> {
    pub cells: core::slice::Iter<'a, Cell>,
    pub idx: usize,
    pub skip_width: usize,
}

impl<'a> Iterator for VecStorageIter<'a> {
    type Item = CellRef<'a>;

    fn next(&mut self) -> Option<CellRef<'a>> {
        while self.skip_width > 0 {
            self.skip_width -= 1;
            let _ = self.cells.next()?;
            self.idx += 1;
        }
        let cell = self.cells.next()?;
        let cell_index = self.idx;
        self.idx += 1;
        self.skip_width = cell.width().saturating_sub(1);
        Some(CellRef::CellRef { cell_index, cell })
    }
}
