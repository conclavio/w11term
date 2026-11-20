use super::*;
use crate::Line;
use std::collections::BTreeMap;
use wezterm_cell::{color::ColorAttribute, image::ImageCell};
use wezterm_surface::TextureCoordinate;

mod diacritics;

type Key = (u32, Option<u32>);
type Point = (i64, i64);
const MAX_DEPTH: usize = 16;
const MAX_PROJECTED_TILES: usize = 65536;

/// Keep an off-screen parent's coordinates without scanning every character of
/// 10,000 scrollback rows on each animation tick. One entry per immutable row
/// revision. Parent/color changes, screen swaps and reflow invalidate the index.
#[derive(Debug, Default)]
pub(super) struct HistoryAnchors {
    screen: (bool, usize, usize),
    parents: HashSet<Key>,
    defaults: HashMap<u32, Key>,
    rows: HashMap<StableRowIndex, (usize, Vec<(Key, i64)>)>,
    count: usize,
}

#[derive(Debug)]
pub(super) struct ProjectedPlacement {
    pub(super) placement: KittyImagePlacement,
    pub(super) parent: Option<Key>,
}
impl TerminalState {
    pub(super) fn validate_kitty_parent(
        &self,
        key: Key,
        placement: &KittyImagePlacement,
    ) -> Result<Option<Key>, &'static str> {
        let Some(image) = placement.parent_image.filter(|&id| id != 0) else {
            return Ok(None);
        };
        if placement.virtual_placement {
            return Err("EINVAL");
        }
        let parent = match placement.parent_placement.filter(|&n| n != 0) {
            Some(p) => (image, Some(p)),
            None => self
                .kitty_img
                .placements
                .keys()
                .chain(self.kitty_img.projected.keys())
                .filter(|k| k.0 == image)
                .copied()
                .min()
                .ok_or("ENOPARENT")?,
        };
        let mut current = parent;
        for depth in 1..=MAX_DEPTH {
            if current == key {
                return Err("ECYCLE");
            }
            let next = if self.kitty_img.placements.contains_key(&current) {
                None
            } else {
                self.kitty_img
                    .projected
                    .get(&current)
                    .ok_or("ENOPARENT")?
                    .parent
            };
            if let Some(ancestor) = next {
                current = ancestor;
                continue;
            }
            // Moving a parent moves its whole subtree. Reserve depth for its
            // existing descendants as well as the proposed ancestors.
            let mut level = vec![key];
            for _ in depth..=MAX_DEPTH {
                let next: Vec<_> = self
                    .kitty_img
                    .projected
                    .iter()
                    .filter_map(|(&child, node)| {
                        node.parent.filter(|p| level.contains(p)).map(|_| child)
                    })
                    .collect();
                if next.is_empty() {
                    return Ok(Some(parent));
                }
                level = next;
            }
            return Err("ETOODEEP");
        }
        Err("ETOODEEP")
    }
    pub(super) fn has_virtual_ancestor(&self, mut key: Key) -> bool {
        while let Some(node) = self.kitty_img.projected.get(&key) {
            if node.placement.virtual_placement {
                return true;
            }
            if let Some(p) = node.parent {
                key = p;
            } else {
                break;
            }
        }
        false
    }
    pub(super) fn remove_kitty_dependents(&mut self, id: u32, p: Option<u32>) {
        let matches = |k: Key| k.0 == id && (p.is_none() || k.1 == p);
        let mut deleted: HashSet<_> = self
            .kitty_img
            .projected
            .keys()
            .chain(self.kitty_img.placements.keys())
            .filter(|&&k| matches(k))
            .copied()
            .collect();
        let mut children = HashSet::new();
        for _ in 0..MAX_DEPTH {
            let next: Vec<_> = self
                .kitty_img
                .projected
                .iter()
                .filter_map(|(&key, node)| {
                    (!deleted.contains(&key)
                        && node.parent.is_some_and(|parent| deleted.contains(&parent)))
                    .then_some(key)
                })
                .collect();
            if next.is_empty() {
                break;
            }
            for key in next {
                deleted.insert(key);
                children.insert(key.0);
            }
        }
        self.kitty_img
            .projected
            .retain(|key, _| !deleted.contains(key));
        for child in children {
            if !self
                .kitty_img
                .placements
                .keys()
                .chain(self.kitty_img.projected.keys())
                .any(|k| k.0 == child)
            {
                self.kitty_img.remove_data_for_id(child);
                self.kitty_img.number_to_id.retain(|_, id| *id != child);
            }
        }
    }

    /// Project virtual and relative placements onto a private copy of visible
    /// lines. The screen and its scrollback retain ordinary editable text.
    pub fn project_kitty_placements(&self, lines: &mut [Line], first_phys: usize) -> bool {
        if self.kitty_img.projected.is_empty() {
            return false;
        }
        let screen = self.screen();
        let cw = (self.pixel_width / screen.physical_cols.max(1)).max(1) as f64;
        let ch = (self.pixel_height / screen.physical_rows.max(1)).max(1) as f64;
        let mut geometry = BTreeMap::new();
        let mut default_virtual = HashMap::new();
        for (&key, node) in &self.kitty_img.projected {
            if let Some(img) = self.kitty_img.id_to_data.get(&key.0) {
                if let Some(grid) = Grid::new(key, &node.placement, img, cw, ch) {
                    geometry.insert(key, grid);
                    if node.placement.virtual_placement {
                        let default = default_virtual.entry(key.0).or_insert(key);
                        *default = (*default).min(key);
                    }
                }
            }
        }
        let mut roots: HashMap<Key, Point> = HashMap::new();
        let parents: HashSet<_> = self
            .kitty_img
            .projected
            .values()
            .filter_map(|node| node.parent)
            .collect();
        let mut tiles = Vec::new();
        let height = lines.len();
        let mut cache = self.kitty_img.history_anchors.lock().unwrap();
        let signature = (
            self.is_alt_screen_active(),
            screen.physical_cols,
            screen.physical_rows,
        );
        if cache.screen != signature
            || cache.parents != parents
            || cache.defaults != default_virtual
        {
            *cache = HistoryAnchors {
                screen: signature,
                parents: parents.clone(),
                defaults: default_virtual.clone(),
                ..Default::default()
            };
        }
        let oldest = screen.phys_to_stable_row_index(0);
        if !parents.is_empty() {
            let expired: usize = cache
                .rows
                .iter()
                .filter(|(row, _)| **row < oldest)
                .map(|(_, (_, keys))| keys.len())
                .sum();
            cache.rows.retain(|&row, _| row >= oldest);
            cache.count -= expired;
        }
        let mut scan = |line: &Line, y: i64| {
            let visible = (0..height as i64).contains(&y);
            let stable = oldest + first_phys as isize + y as isize;
            if !visible {
                if let Some((seqno, anchors)) = cache.rows.get(&stable) {
                    if *seqno == line.current_seqno() {
                        for &(key, x) in anchors {
                            min_point(&mut roots, key, (x, y));
                        }
                        return;
                    }
                }
            }
            let mut row_roots = HashMap::new();
            let mut record = |key: Key, x: i64| {
                if parents.contains(&key) {
                    let old = row_roots.entry(key).or_insert(x);
                    *old = (*old).min(x);
                }
            };
            let mut previous = None;
            for cell in line.visible_cells() {
                let x = cell.cell_index() as i64;
                if x >= screen.physical_cols as i64 {
                    break;
                }
                if !parents.is_empty() {
                    for tile in cell.attrs().images().into_iter().flatten() {
                        if let Some(id) = tile.image_id() {
                            record((id, tile.placement_id()), x);
                        }
                    }
                }
                previous = Placeholder::decode(
                    cell.str(),
                    cell.attrs().foreground(),
                    cell.attrs().underline_color(),
                    previous,
                );
                if let Some(ph) = previous {
                    let id = ph.low | (ph.high << 24);
                    let key = if ph.placement == 0 {
                        default_virtual.get(&id).copied()
                    } else {
                        Some((id, Some(ph.placement)))
                    };
                    if let Some((key, grid)) = key.and_then(|k| geometry.get(&k).map(|g| (k, g))) {
                        if !self.kitty_img.projected[&key].placement.virtual_placement {
                            continue;
                        }
                        record(key, x);
                        if visible && tiles.len() < MAX_PROJECTED_TILES {
                            if let Some(tile) = grid.tile(ph.col, ph.row) {
                                tiles.push((x as usize, y as usize, tile));
                            }
                        }
                    }
                }
            }
            for (&key, &x) in &row_roots {
                min_point(&mut roots, key, (x, y));
            }
            if !visible {
                if let Some((_, old)) = cache.rows.remove(&stable) {
                    cache.count -= old.len();
                }
                if cache.count + row_roots.len() <= MAX_PROJECTED_TILES {
                    cache.count += row_roots.len();
                    cache.rows.insert(
                        stable,
                        (line.current_seqno(), row_roots.into_iter().collect()),
                    );
                }
            }
        };
        if !parents.is_empty() {
            self.screen()
                .for_each_phys_line(|row, line| scan(line, row as i64 - first_phys as i64));
        } else {
            for (row, line) in lines.iter().enumerate() {
                scan(line, row as i64);
            }
        }
        drop(cache);
        for (&key, grid) in &geometry {
            let node = &self.kitty_img.projected[&key];
            if node.parent.is_none() {
                continue;
            }
            let mut anchor = key;
            let (mut x, mut y) = (0i64, 0i64);
            while let Some(node) = self.kitty_img.projected.get(&anchor) {
                let Some(parent) = node.parent else { break };
                x += i64::from(node.placement.horizontal_offset.unwrap_or(0));
                y += i64::from(node.placement.vertical_offset.unwrap_or(0));
                anchor = parent;
            }
            let Some((px, py)) = roots.get(&anchor) else {
                continue;
            };
            x += px;
            y += py;
            for row in y.max(0)..(y + i64::from(grid.rows)).min(height as i64) {
                for col in x.max(0)..(x + i64::from(grid.cols)).min(screen.physical_cols as i64) {
                    if tiles.len() == MAX_PROJECTED_TILES {
                        break;
                    }
                    if let Some(tile) = grid.tile((col - x) as u32, (row - y) as u32) {
                        tiles.push((col as usize, row as usize, tile));
                    }
                }
                if tiles.len() == MAX_PROJECTED_TILES {
                    break;
                }
            }
        }
        for (x, y, tile) in tiles {
            let line = &mut lines[y];
            if x >= line.len() {
                line.resize(x + 1, line.current_seqno());
            }
            line.cells_mut_for_attr_changes_only()[x]
                .attrs_mut()
                .attach_image(Box::new(tile));
        }
        true
    }
}

fn min_point(roots: &mut HashMap<Key, Point>, key: Key, (x, y): Point) {
    let old = roots.entry(key).or_insert((x, y));
    old.0 = old.0.min(x);
    old.1 = old.1.min(y);
}

#[derive(Clone, Copy)]
struct Placeholder {
    low: u32,
    placement: u32,
    row: u32,
    col: u32,
    high: u32,
}
impl Placeholder {
    fn decode(
        text: &str,
        fg: ColorAttribute,
        ul: ColorAttribute,
        previous: Option<Self>,
    ) -> Option<Self> {
        let mut chars = text.strip_prefix('\u{10eeee}')?.chars();
        let low = color_id(fg);
        let placement = color_id(ul);
        let prev = previous.filter(|p| p.low == low && p.placement == placement);
        let mut next = || {
            chars.next().map(|c| {
                diacritics::VALUES
                    .binary_search(&(c as u32))
                    .ok()
                    .map(|i| i as u32)
            })
        };
        let row = match next() {
            Some(value) => value?,
            None => prev.map_or(0, |p| p.row),
        };
        let prev = prev.filter(|p| p.row == row);
        let col = match next() {
            Some(value) => value?,
            None => prev.map_or(0, |p| p.col.saturating_add(1)),
        };
        let high = match next() {
            Some(value) => value?,
            None => prev
                .filter(|p| p.col.checked_add(1) == Some(col))
                .map_or(0, |p| p.high),
        };
        (high < 256).then_some(Self {
            low,
            placement,
            row,
            col,
            high,
        })
    }
}
fn color_id(color: ColorAttribute) -> u32 {
    match color {
        ColorAttribute::Default => 0,
        ColorAttribute::PaletteIndex(n) => u32::from(n),
        ColorAttribute::TrueColorWithDefaultFallback(c)
        | ColorAttribute::TrueColorWithPaletteFallback(c, _) => {
            let (r, g, b, _) = c.to_srgb_u8();
            u32::from_be_bytes([0, r, g, b])
        }
    }
}

struct Grid<'a> {
    key: Key,
    data: &'a Arc<ImageData>,
    z: i32,
    cols: u32,
    rows: u32,
    cell: [f64; 2],
    offset: [f64; 2],
    size: [f64; 2],
    source: [f64; 4],
    image: [f64; 2],
}
impl<'a> Grid<'a> {
    fn new(
        key: Key,
        p: &KittyImagePlacement,
        data: &'a Arc<ImageData>,
        cw: f64,
        ch: f64,
    ) -> Option<Self> {
        let (iw, ih) = data.data().dimensions().ok()?;
        let ox = p.x.unwrap_or(0);
        let oy = p.y.unwrap_or(0);
        let w = p.w.unwrap_or(iw).min(iw.saturating_sub(ox)) as f64;
        let h = p.h.unwrap_or(ih).min(ih.saturating_sub(oy)) as f64;
        if w == 0. || h == 0. {
            return None;
        }
        let mut offset = [
            f64::from(p.x_offset.unwrap_or(0)).min(cw - 1.),
            f64::from(p.y_offset.unwrap_or(0)).min(ch - 1.),
        ];
        let box_w = p
            .columns
            .filter(|&v| v > 0)
            .map(f64::from)
            .or_else(|| p.virtual_placement.then(|| (w / cw).ceil()))
            .map(|n| n * cw - offset[0]);
        let box_h = p
            .rows
            .filter(|&v| v > 0)
            .map(f64::from)
            .or_else(|| p.virtual_placement.then(|| (h / ch).ceil()))
            .map(|n| n * ch - offset[1]);
        let size = match (box_w, box_h) {
            (Some(x), Some(y)) if p.virtual_placement => {
                let scale = (x / w).min(y / h);
                let size = [scale * w, scale * h];
                offset[0] += (x - size[0]) * 0.5;
                offset[1] += (y - size[1]) * 0.5;
                size
            }
            (Some(x), Some(y)) => [x, y],
            (Some(x), None) => [x, x * h / w],
            (None, Some(y)) => [y * w / h, y],
            (None, None) => [w, h],
        };
        Some(Self {
            key,
            data,
            z: p.z_index.unwrap_or(0),
            cols: ((offset[0] + size[0]) / cw).ceil() as u32,
            rows: ((offset[1] + size[1]) / ch).ceil() as u32,
            cell: [cw, ch],
            offset,
            size,
            source: [ox as f64, oy as f64, w, h],
            image: [iw as f64, ih as f64],
        })
    }
    fn tile(&self, col: u32, row: u32) -> Option<ImageCell> {
        let crop = |n: u32, axis: usize| {
            let cell = self.cell[axis];
            let a = f64::from(n) * cell;
            let start = a.max(self.offset[axis]);
            let end = (a + cell).min(self.offset[axis] + self.size[axis]);
            if end <= start {
                return None;
            }
            let uv = |v: f64| {
                ((self.source[axis]
                    + (v - self.offset[axis]) / self.size[axis] * self.source[axis + 2])
                    / self.image[axis]) as f32
            };
            Some((
                uv(start),
                uv(end),
                (start - a).round() as u16,
                (a + cell - end).round() as u16,
            ))
        };
        let (x1, x2, l, r) = crop(col, 0)?;
        let (y1, y2, t, b) = crop(row, 1)?;
        Some(ImageCell::with_z_index(
            TextureCoordinate::new_f32(x1, y1),
            TextureCoordinate::new_f32(x2, y2),
            self.data.clone(),
            self.z,
            l,
            t,
            r,
            b,
            Some(self.key.0),
            self.key.1,
        ))
    }
}
