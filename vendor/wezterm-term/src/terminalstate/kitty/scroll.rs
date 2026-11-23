use super::{Key, KittyImageState};
use crate::{TerminalState, VisibleRowIndex};
use std::collections::HashSet;
use std::ops::Range;
use wezterm_cell::image::ImageCell;
use wezterm_surface::line::Line;

struct Tile {
    row: VisibleRowIndex,
    col: usize,
    key: Key,
    image: ImageCell,
}

pub(in crate::terminalstate) struct KittyTileUpdate {
    stationary: Vec<Tile>,
    clipped: HashSet<Key>,
}

enum TextMotion {
    Scroll(VisibleRowIndex),
    Edit { pushes_history: bool },
}

impl TerminalState {
    pub(in crate::terminalstate) fn prepare_kitty_margin_scroll(
        &mut self,
        rows: &Range<VisibleRowIndex>,
        cols: &Range<usize>,
        delta: VisibleRowIndex,
    ) -> Option<KittyTileUpdate> {
        self.prepare_kitty_tiles(rows, cols, TextMotion::Scroll(delta))
    }

    pub(in crate::terminalstate) fn prepare_kitty_text_edit(
        &mut self,
        rows: &Range<VisibleRowIndex>,
        cols: &Range<usize>,
        pushes_history: bool,
    ) -> Option<KittyTileUpdate> {
        self.prepare_kitty_tiles(rows, cols, TextMotion::Edit { pushes_history })
    }

    fn prepare_kitty_tiles(
        &mut self,
        rows: &Range<VisibleRowIndex>,
        cols: &Range<usize>,
        motion: TextMotion,
    ) -> Option<KittyTileUpdate> {
        let state = &self.kitty_img;
        if state.placements.is_empty() || rows.is_empty() || cols.is_empty() {
            return None;
        }
        let full_width = cols.start == 0 && cols.end == self.screen.physical_cols;
        let (stationary_edit, delta, pushes_history) = match motion {
            TextMotion::Scroll(delta) => {
                if delta == 0
                    || (full_width
                        && rows.start == 0
                        && rows.end == self.screen.physical_rows as VisibleRowIndex)
                {
                    return None;
                }
                (false, delta, delta < 0)
            }
            TextMotion::Edit { pushes_history } => (true, 0, pushes_history),
        };

        let screen = &mut self.screen;
        let first_live = screen.phys_row(0);
        let region = screen.phys_range(rows);
        let earliest = state
            .placements
            .values()
            .map(|placement| placement.created)
            .min()?;
        let mut tiles = Vec::new();
        let mut involved = HashSet::new();
        for (row, physical) in rows.clone().zip(region.clone()) {
            let line = screen.line_mut(physical);
            if !line.changed_since(earliest.saturating_sub(1)) {
                continue;
            }
            visit_kitty_tiles(line, state, |col, key, image| {
                if cols.contains(&col) {
                    involved.insert(key);
                    tiles.push(Tile {
                        row,
                        col,
                        key,
                        image,
                    });
                }
            });
        }
        if involved.is_empty() {
            return None;
        }

        // A placement with a tile anywhere outside this rectangle stays put.
        // Check retained history too, which can contain part of a live placement.
        let earliest = involved
            .iter()
            .map(|key| state.placements[key].created)
            .min()?;
        let history_moves = pushes_history && rows.start == 0 && full_width && first_live != 0;
        let mut crossing = HashSet::new();
        if !stationary_edit || history_moves {
            screen.for_each_phys_line_mut(|physical, line| {
                if (full_width && region.contains(&physical))
                    || !line.changed_since(earliest.saturating_sub(1))
                {
                    return;
                }
                if physical < first_live {
                    // Read historical rows in their existing representation. Only
                    // stationary tiles that need to move relative to history are staged.
                    for cell in line.visible_cells() {
                        for image in cell.attrs().images().into_iter().flatten() {
                            if let Some(key) =
                                state.tile_key(&image).filter(|key| involved.contains(key))
                            {
                                crossing.insert(key);
                                if history_moves {
                                    tiles.push(Tile {
                                        row: physical as VisibleRowIndex
                                            - first_live as VisibleRowIndex,
                                        col: cell.cell_index(),
                                        key,
                                        image,
                                    });
                                }
                            }
                        }
                    }
                    return;
                }
                if stationary_edit || crossing.len() == involved.len() {
                    return;
                }
                visit_kitty_tiles(line, state, |col, key, _| {
                    if (!region.contains(&physical) || !cols.contains(&col))
                        && involved.contains(&key)
                    {
                        crossing.insert(key);
                    }
                });
            });
        }

        let mut stationary = Vec::new();
        let mut surviving = HashSet::new();
        for tile in tiles {
            let stays = stationary_edit || crossing.contains(&tile.key);
            if !stays && rows.contains(&(tile.row + delta)) {
                // The normal text scroll already moves these tiles without
                // changing their texture coordinates or pixel padding.
                surviving.insert(tile.key);
                continue;
            }
            let physical = (first_live as VisibleRowIndex + tile.row) as usize;
            let line = screen.line_mut(physical);
            line.cells_mut_for_attr_changes_only()[tile.col]
                .attrs_mut()
                .detach_image_with_placement(tile.key.0, Some(tile.key.1));
            line.update_last_change_seqno(self.seqno);
            if stays {
                surviving.insert(tile.key);
                stationary.push(tile);
            }
        }
        involved.retain(|key| !surviving.contains(key));
        Some(KittyTileUpdate {
            stationary,
            clipped: involved,
        })
    }

    pub(in crate::terminalstate) fn finish_kitty_tile_update(
        &mut self,
        update: Option<KittyTileUpdate>,
    ) {
        let Some(KittyTileUpdate {
            stationary,
            mut clipped,
        }) = update
        else {
            return;
        };
        let seqno = self.seqno;
        let screen = &mut self.screen;
        let first_live = screen.phys_row(0);
        for tile in stationary {
            let Some(physical) = first_live.checked_add_signed(tile.row as isize) else {
                continue;
            };
            let line = screen.line_mut(physical);
            if tile.col >= line.len() {
                line.resize(tile.col + 1, seqno);
            }
            line.cells_mut_for_attr_changes_only()[tile.col]
                .attrs_mut()
                .attach_image(Box::new(tile.image));
            line.update_last_change_seqno(seqno);
        }
        if !clipped.is_empty() {
            self.kitty_img
                .collect_kitty_dependents(&mut clipped, &mut HashSet::new());
            self.kitty_img.discard_placements(&clipped);
        }
    }
}

fn visit_kitty_tiles(
    line: &mut Line,
    state: &KittyImageState,
    mut visit: impl FnMut(usize, Key, ImageCell),
) {
    // The image on a wide glyph's continuation can differ from its first
    // cell. Inspect raw cells on rows that can contain graphics.
    if !line
        .visible_cells()
        .any(|cell| cell.width() > 1 || cell.attrs().images().is_some())
    {
        return;
    }
    for (column, cell) in line.cells_mut_for_attr_changes_only().iter().enumerate() {
        for image in cell.attrs().images().into_iter().flatten() {
            if let Some(key) = state.tile_key(&image) {
                visit(column, key, image);
            }
        }
    }
}
