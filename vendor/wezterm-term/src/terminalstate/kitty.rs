use crate::terminalstate::image::*;
use crate::terminalstate::{ImageAttachParams, PlacementInfo};
use crate::TerminalState;
use ::image::{
    DynamicImage, GenericImage, GenericImageView, ImageBuffer, RgbImage, Rgba, RgbaImage,
};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::io::Write;
use std::ops::Range;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
mod feature_query;
mod placement;
mod playback;
use placement::{Key, ProjectedPlacement};
use playback::Playback;
use wezterm_cell::image::{ImageCell, ImageDataType};
use wezterm_escape_parser::apc::{
    KittyFrameCompositionMode, KittyImage, KittyImageCompression, KittyImageData, KittyImageDelete,
    KittyImageFormat, KittyImageFrame, KittyImageFrameCompose, KittyImagePlacement,
    KittyImageTransmit, KittyImageVerbosity,
};
use wezterm_surface::change::ImageData;

#[derive(Debug)]
struct PhysicalPlacement {
    bounds: PlacementInfo,
    row_revision: u64,
    created: wezterm_surface::SequenceNo,
    z: i32,
    // A reconnected SSH session can reuse an ID still visible in frozen history.
    data: Weak<ImageData>,
}

#[derive(Clone, Copy)]
struct PlacementSelection {
    column: Option<usize>,
    row: Option<usize>,
    z: Option<i32>,
}

impl PlacementSelection {
    fn cell(x: usize, y: usize, z: Option<i32>) -> Self {
        Self {
            column: Some(x),
            row: Some(y),
            z,
        }
    }

    fn wire_cell(x: u32, y: u32, z: Option<i32>) -> Option<Self> {
        Some(Self::cell(
            x.checked_sub(1)? as usize,
            y.checked_sub(1)? as usize,
            z,
        ))
    }

    fn column(x: u32) -> Option<Self> {
        Some(Self {
            column: Some(x.checked_sub(1)? as usize),
            row: None,
            z: None,
        })
    }

    fn row(y: u32) -> Option<Self> {
        Some(Self {
            column: None,
            row: Some(y.checked_sub(1)? as usize),
            z: None,
        })
    }
}

#[derive(Debug, Default)]
pub struct KittyImageState {
    accumulator: Vec<KittyImage>,
    max_image_id: u32,
    // Multiple live image IDs may have the same client-supplied number.
    // A numbered upload always allocates a fresh, increasing image ID.
    numbered_images: HashMap<u32, Vec<u32>>,
    id_to_data: HashMap<u32, Arc<ImageData>>,
    // An upload without a wire image ID belongs to its sole placement.
    anonymous_data: HashMap<Key, Arc<ImageData>>,
    // Cell attachments carry an occurrence token, independent of a client's p=.
    named_placements: HashMap<(u32, u32), Key>,
    placements: HashMap<Key, PhysicalPlacement>,
    playback: HashMap<u32, Playback>,
    projected: HashMap<Key, ProjectedPlacement>,
    history_anchors: std::sync::Mutex<placement::HistoryAnchors>,
    used_memory: usize,
    accumulated_bytes: usize,
}

impl KittyImageState {
    fn id_for_number(&self, number: u32) -> Option<u32> {
        self.numbered_images.get(&number)?.last().copied()
    }

    fn retain_numbered_images(
        numbered_images: &mut HashMap<u32, Vec<u32>>,
        mut keep: impl FnMut(u32) -> bool,
    ) {
        numbered_images.retain(|_, ids| {
            ids.retain(|&id| keep(id));
            !ids.is_empty()
        });
    }

    fn remove_data_for_id(&mut self, image_id: u32) {
        self.playback.remove(&image_id);
        if let Some(data) = self.id_to_data.remove(&image_id) {
            self.used_memory = self.used_memory.saturating_sub(data.len());
        }
    }

    fn keys(&self) -> impl Iterator<Item = Key> + '_ {
        self.placements.keys().chain(self.projected.keys()).copied()
    }

    fn tile_key(&self, tile: &ImageCell) -> Option<Key> {
        let key = (tile.image_id()?, tile.placement_id()?);
        (self.placements.get(&key)?.data.as_ptr() == Arc::as_ptr(tile.image_data())).then_some(key)
    }

    fn placement_count(&self) -> usize {
        self.placements.len() + self.projected.len()
    }

    fn key_for_placement(&self, image_id: u32, placement_id: Option<u32>) -> Key {
        if let Some(key) = placement_id.and_then(|p| self.named_placements.get(&(image_id, p))) {
            return *key;
        }
        // A fresh token also distinguishes frozen tiles across reconnects. A
        // wrapping process-wide counter cannot collide with a live placement.
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
        loop {
            let token = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let key = (image_id, token);
            if !self.placements.contains_key(&key) && !self.projected.contains_key(&key) {
                return key;
            }
        }
    }

    fn image_count(&self) -> usize {
        self.id_to_data.len() + self.anonymous_data.len()
    }

    fn refresh_memory(&mut self) {
        self.used_memory = self
            .id_to_data
            .values()
            .chain(self.anonymous_data.values())
            .map(|data| data.len())
            .sum();
    }

    fn check_image_capacity(
        &mut self,
        image_id: u32,
        data: &ImageData,
        inactive: &Self,
    ) -> anyhow::Result<()> {
        self.refresh_memory();
        self.prune_unreferenced();
        let old = self.id_to_data.get(&image_id).map_or(0, |data| data.len());
        anyhow::ensure!(
            self.used_memory
                .saturating_sub(old)
                .saturating_add(inactive.used_memory)
                .saturating_add(data.len())
                <= 64 * 1024 * 1024,
            "kitty image table exceeds 64 MiB"
        );
        anyhow::ensure!(
            self.id_to_data.contains_key(&image_id)
                || self.image_count() + inactive.image_count() < 512,
            "kitty image table exceeds 512 ids"
        );
        Ok(())
    }

    fn discard_placements(&mut self, keys: &HashSet<Key>) {
        self.named_placements.retain(|_, key| !keys.contains(key));
        for key in keys {
            self.placements.remove(key);
            self.projected.remove(key);
            if let Some(img) = self.anonymous_data.remove(key) {
                self.used_memory = self.used_memory.saturating_sub(img.len());
            }
        }
    }

    fn physical_keys(&self, screen: &crate::Screen) -> (HashSet<Key>, HashSet<Key>) {
        let mut retained = HashSet::new();
        let mut visible = HashSet::new();
        if let Some(oldest) = self.placements.values().map(|p| p.created).min() {
            let first_live = screen.phys_row(0);
            for (row, line) in screen.physical_lines().enumerate() {
                if !line.changed_since(oldest.saturating_sub(1)) {
                    continue;
                }
                for cell in line.visible_cells() {
                    for tile in cell.attrs().images().into_iter().flatten() {
                        if let Some(key) = self.tile_key(&tile) {
                            retained.insert(key);
                            if row >= first_live && cell.cell_index() < screen.physical_cols {
                                visible.insert(key);
                            }
                        }
                    }
                }
            }
        }
        (retained, visible)
    }

    fn reclaim_missing(&mut self, screen: &crate::Screen, placing: Option<u32>) {
        let (retained, _) = self.physical_keys(screen);
        let mut missing: HashSet<_> = self
            .placements
            .keys()
            .filter(|key| !retained.contains(key))
            .copied()
            .collect();
        if !missing.is_empty() {
            let mut free_ids = HashSet::new();
            self.collect_kitty_dependents(&mut missing, &mut free_ids);
            self.discard_placements(&missing);
            if let Some(id) = placing {
                free_ids.remove(&id);
            }
            self.release_unplaced_images(free_ids);
        }
    }

    fn release_unplaced_images(&mut self, mut ids: HashSet<u32>) -> bool {
        if ids.is_empty() {
            return false;
        }
        for key in self.keys() {
            ids.remove(&key.0);
        }
        if ids.is_empty() {
            return false;
        }
        Self::retain_numbered_images(&mut self.numbered_images, |id| !ids.contains(&id));
        for id in ids {
            self.remove_data_for_id(id);
        }
        true
    }

    fn prune_unreferenced(&mut self) {
        let budget = 32 * 1024 * 1024;
        if self.used_memory > budget {
            let referenced: HashSet<u32> = self
                .placements
                .keys()
                .chain(self.projected.keys())
                .map(|(k, _)| *k)
                .collect();
            let target = self.used_memory - budget;
            let mut freed = 0;
            self.id_to_data.retain(|id, data| {
                if referenced.contains(id) || freed > target {
                    true
                } else {
                    freed += data.len();
                    false
                }
            });

            log::info!(
                "using {} RAM for images, pruned {}",
                self.used_memory,
                freed
            );
            self.used_memory = self.used_memory.saturating_sub(freed);
            let table = &self.id_to_data;
            Self::retain_numbered_images(&mut self.numbered_images, |id| table.contains_key(&id));
            self.playback.retain(|id, _| table.contains_key(id));
        }
    }
}

impl TerminalState {
    fn reclaim_missing_kitty_placements(&mut self, placing: u32) {
        self.kitty_img.reclaim_missing(&self.screen, Some(placing));
        let inactive = if self.screen.alt_screen_is_active {
            &self.screen.screen
        } else {
            &self.screen.alt_screen
        };
        self.inactive_kitty_img.reclaim_missing(inactive, None);
    }

    fn kitty_img_place(
        &mut self,
        image_id: Option<u32>,
        image_number: Option<u32>,
        placement: KittyImagePlacement,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        let image_number = image_number.filter(|&n| n != 0);
        let image_id = match image_id.filter(|&id| id != 0) {
            Some(id) => id,
            None => self
                .kitty_img
                .id_for_number(
                    image_number
                        .ok_or_else(|| anyhow::anyhow!("no image_id or image_number specified!"))?,
                )
                .ok_or_else(|| {
                    anyhow::anyhow!("image_number has no matching image id {:?}", image_number)
                })?,
        };

        log::trace!(
            "kitty_img_place image_id {:?} image_no {:?} placement {:?} verb {:?}",
            image_id,
            image_number,
            placement,
            verbosity
        );
        let img = Arc::clone(self.kitty_img.id_to_data.get(&image_id).ok_or_else(|| {
            anyhow::anyhow!(
                "no matching image id {} in id_to_data for image_number {:?}",
                image_id,
                image_number
            )
        })?);

        self.kitty_place_data(image_id, image_number, img, placement, verbosity)
    }

    fn kitty_place_data(
        &mut self,
        image_id: u32,
        image_number: Option<u32>,
        img: Arc<ImageData>,
        mut placement: KittyImagePlacement,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        placement.placement_id = placement.placement_id.filter(|&p| image_id != 0 && p != 0);
        let wire_placement = placement.placement_id;
        // Only addressable requests have a reply, even for placement errors.
        let verbosity = if image_id == 0 {
            KittyImageVerbosity::Quiet
        } else {
            verbosity
        };
        let replacing = wire_placement
            .is_some_and(|p| self.kitty_img.named_placements.contains_key(&(image_id, p)));
        if !replacing
            && self.kitty_img.placement_count() + self.inactive_kitty_img.placement_count() >= 1024
        {
            self.reclaim_missing_kitty_placements(image_id);
        }
        let key = self.kitty_img.key_for_placement(image_id, wire_placement);
        let parent = match self.validate_kitty_parent(key, &placement) {
            Ok(parent) => parent,
            Err(reason) => {
                self.kitty_send_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    reason.into(),
                );
                return Ok(());
            }
        };
        if !replacing
            && self.kitty_img.placement_count() + self.inactive_kitty_img.placement_count() >= 1024
        {
            self.kitty_send_response(
                verbosity,
                false,
                Some(image_id),
                image_number,
                "ENOSPC".into(),
            );
            return Ok(());
        }
        // Repositioning an existing parent preserves its dependent placements.
        if replacing {
            let old = HashSet::from([key]);
            self.detach_kitty_cells(&old);
            self.kitty_img.discard_placements(&old);
        }
        if placement.virtual_placement || parent.is_some() {
            self.kitty_img
                .projected
                .insert(key, ProjectedPlacement { placement, parent });
        } else {
            let (image_width, image_height) = img.data().dimensions()?;

            let data = Arc::downgrade(&img);
            let row_revision = self.screen().placement_row_revision;
            let bounds = self.assign_image_to_cells(ImageAttachParams {
                image_width,
                image_height,
                source_width: placement.w,
                source_height: placement.h,
                source_origin_x: placement.x.unwrap_or(0),
                source_origin_y: placement.y.unwrap_or(0),
                cell_padding_left: placement.x_offset.unwrap_or(0) as u16,
                cell_padding_top: placement.y_offset.unwrap_or(0) as u16,
                data: Arc::clone(&img),
                style: ImageAttachStyle::Kitty,
                z_index: placement.z_index.unwrap_or(0),
                columns: placement.columns.map(|x| x as usize),
                rows: placement.rows.map(|x| x as usize),
                image_id: Some(image_id),
                placement_id: Some(key.1),
                do_not_move_cursor: placement.do_not_move_cursor,
            })?;

            self.kitty_img.placements.insert(
                key,
                PhysicalPlacement {
                    bounds,
                    row_revision,
                    created: self.seqno,
                    z: placement.z_index.unwrap_or(0),
                    data,
                },
            );
        }
        if let Some(p) = wire_placement {
            self.kitty_img.named_placements.insert((image_id, p), key);
        }
        if image_id == 0 {
            self.kitty_img.used_memory += img.len();
            self.kitty_img.anonymous_data.insert(key, img);
        }
        Ok(())
    }

    fn kitty_img_inner(&mut self, img: KittyImage) -> anyhow::Result<()> {
        match self
            .coalesce_kitty_accumulation(img)
            .context("coalesce_kitty_accumulation")?
        {
            KittyImage::TransmitData {
                transmit,
                verbosity,
            } => self.kitty_img_transmit(transmit, None, verbosity),
            KittyImage::TransmitDataAndDisplay {
                transmit,
                placement,
                verbosity,
            } => {
                log::trace!("TransmitDataAndDisplay {:#?} {:#?}", transmit, placement);
                self.kitty_img_transmit(transmit, Some(placement), verbosity)
            }
            KittyImage::TransmitFrame {
                transmit,
                frame,
                verbosity,
            } => self.kitty_frame_transmit(transmit, frame, verbosity),
            _ => anyhow::bail!("impossible KittImage variant"),
        }
    }

    pub(crate) fn kitty_img(&mut self, mut img: KittyImage) -> anyhow::Result<()> {
        log::trace!("{:?}", img);
        if !self.config.enable_kitty_graphics() {
            return Ok(());
        }
        let verbosity = img.verbosity();
        if let KittyImage::TransmitData { transmit, .. }
        | KittyImage::TransmitDataAndDisplay { transmit, .. }
        | KittyImage::TransmitFrame { transmit, .. }
        | KittyImage::Query { transmit } = &img
        {
            let bytes = match &transmit.data {
                KittyImageData::Direct(data) => data.len(),
                KittyImageData::DirectBin(data) => data.len(),
                _ => {
                    self.kitty_img.accumulator.clear();
                    self.kitty_img.accumulated_bytes = 0;
                    anyhow::bail!("SSH kitty images require direct transport");
                }
            };
            if self
                .kitty_img
                .accumulated_bytes
                .saturating_add(self.inactive_kitty_img.accumulated_bytes)
                .saturating_add(bytes)
                > 48 * 1024 * 1024
            {
                self.kitty_img.accumulator.clear();
                self.kitty_img.accumulated_bytes = 0;
                anyhow::bail!("kitty image transmission exceeds 48 MiB");
            }
            if transmit.more_data_follows {
                if matches!(&img, KittyImage::Query { .. })
                    || self.kitty_img.accumulator.len() + self.inactive_kitty_img.accumulator.len()
                        >= 16384
                {
                    self.kitty_img.accumulated_bytes = 0;
                    self.kitty_img.accumulator.clear();
                    anyhow::bail!("unsupported kitty query continuation or too many chunks");
                }
                self.kitty_img.accumulated_bytes += bytes;
                self.kitty_img.accumulator.push(img);
                return Ok(());
            }
            if transmission(&mut img).is_some() {
                return self.kitty_img_inner(img);
            }
        }
        match img {
            KittyImage::FeatureQuery {
                image_id,
                action,
                options,
                verbosity,
            } => {
                let response = feature_query::response(image_id, action, &options, verbosity);
                if let Some(response) = response {
                    self.writer.write_all(response.as_bytes())?;
                    self.writer.flush()?;
                }
            }
            KittyImage::Query { transmit } => match transmit.data.load_data() {
                Ok(_) => {
                    self.kitty_send_response(
                        verbosity,
                        true,
                        transmit.image_id,
                        transmit.image_number,
                        "OK".to_string(),
                    );
                }
                Err(err) => {
                    self.kitty_send_response(
                        verbosity,
                        false,
                        transmit.image_id,
                        transmit.image_number,
                        format!("ERROR:{:#}", err),
                    );
                }
            },
            KittyImage::Display {
                image_id,
                image_number,
                placement,
                verbosity,
            } => {
                self.kitty_img_place(image_id, image_number, placement, verbosity)?;
            }
            KittyImage::Delete { what, .. } => {
                // A delete aborts any incomplete transfer on this screen.
                self.kitty_img.accumulator.clear();
                self.kitty_img.accumulated_bytes = 0;
                self.kitty_delete(what);
            }
            KittyImage::TransmitData { .. }
            | KittyImage::TransmitDataAndDisplay { .. }
            | KittyImage::TransmitFrame { .. } => unreachable!("handled before dispatch"),
            KittyImage::Animation {
                image_id,
                image_number,
                state,
                edit_frame,
                gap,
                current,
                loops,
                verbosity,
            } => {
                let image_number = image_number.filter(|&n| n != 0);
                let id = image_id
                    .filter(|&id| id != 0)
                    .or_else(|| self.kitty_img.id_for_number(image_number?));
                let control = playback::Control {
                    state,
                    edit_frame,
                    gap,
                    current,
                    loops,
                };
                let result = id
                    .and_then(|id| {
                        self.kitty_img
                            .id_to_data
                            .get(&id)
                            .map(|img| (id, Arc::clone(img)))
                    })
                    .ok_or("ENOENT")
                    .and_then(|(id, img)| {
                        let now = self.kitty_animation_time.unwrap_or_else(Instant::now);
                        self.kitty_img.playback.entry(id).or_default().control(
                            &mut img.data(),
                            control,
                            now,
                        )
                    });
                self.kitty_send_response(
                    verbosity,
                    result.is_ok(),
                    id,
                    image_number,
                    result.err().unwrap_or("OK").into(),
                );
            }
            KittyImage::ComposeFrame { frame, verbosity } => {
                if let Err(err) = self.kitty_frame_compose(frame, verbosity) {
                    log::error!("Error {:#} while handling KittyImage::ComposeFrame", err);
                }
            }
        };

        Ok(())
    }

    fn kitty_remove_placement_from_model(
        &mut self,
        image_id: u32,
        placement_id: Option<u32>,
        info: PlacementInfo,
    ) {
        let seqno = self.seqno;
        let screen = self.screen_mut();
        let first = info.first_row - screen.phys_to_stable_row_index(0);
        let start = first.max(0) as usize;
        let end = (first.saturating_add(info.rows as isize).max(0) as usize)
            .min(screen.scrollback_rows());
        for idx in start..end {
            let line = screen.line_mut(idx);
            for c in line.cells_mut() {
                c.attrs_mut()
                    .detach_image_with_placement(image_id, placement_id);
            }
            line.update_last_change_seqno(seqno);
        }
    }

    fn kitty_delete(&mut self, what: KittyImageDelete) {
        let (mut keys, mut free_ids): (HashSet<Key>, HashSet<u32>) = match what {
            KittyImageDelete::All { delete } => {
                let (retained, mut visible) = self.kitty_img.physical_keys(self.screen());
                let screen = self.screen();
                self.find_kitty_relatives(
                    &mut visible,
                    screen.phys_row(0)..screen.scrollback_rows(),
                    0..screen.physical_cols,
                    None,
                );
                let release = visible
                    .iter()
                    .map(|key| key.0)
                    .filter(|&id| delete && id != 0)
                    .collect();
                // Cell erasure and history eviction can remove a physical placement
                // independently. A stale entry must not pin its source data when
                // the last visible placement of the same image is deleted.
                visible.extend(
                    self.kitty_img
                        .placements
                        .keys()
                        .filter(|key| !retained.contains(key))
                        .copied(),
                );
                (visible, release)
            }
            KittyImageDelete::ByImageIdRange {
                first,
                last,
                delete,
            } => {
                // Anonymous images have no client image ID.
                let matches = |id: u32| id != 0 && (first..=last).contains(&id);
                let keys = self.kitty_img.keys().filter(|key| matches(key.0)).collect();
                let release = self
                    .kitty_img
                    .id_to_data
                    .keys()
                    .copied()
                    .filter(|&id| delete && matches(id))
                    .collect();
                (keys, release)
            }
            KittyImageDelete::ByImageId {
                image_id,
                placement_id,
                delete,
            } => {
                if image_id == 0 {
                    return;
                }
                let keys = match placement_id.filter(|&p| p != 0) {
                    Some(p) => self
                        .kitty_img
                        .named_placements
                        .get(&(image_id, p))
                        .copied()
                        .into_iter()
                        .collect(),
                    None => self
                        .kitty_img
                        .keys()
                        .filter(|key| key.0 == image_id)
                        .collect(),
                };
                let release = delete.then_some(image_id).into_iter().collect();
                (keys, release)
            }
            KittyImageDelete::ByImageNumber {
                image_number,
                placement_id,
                delete,
            } => {
                if let Some(image_id) = self.kitty_img.id_for_number(image_number) {
                    self.kitty_delete(KittyImageDelete::ByImageId {
                        image_id,
                        placement_id,
                        delete,
                    });
                }
                return;
            }
            KittyImageDelete::AtCursorPosition { delete } => {
                let selection = usize::try_from(self.cursor.y)
                    .ok()
                    .map(|y| PlacementSelection::cell(self.cursor.x, y, None));
                self.kitty_delete_selection(selection, delete)
            }
            KittyImageDelete::DeleteAt { x, y, delete } => {
                self.kitty_delete_selection(PlacementSelection::wire_cell(x, y, None), delete)
            }
            KittyImageDelete::DeleteAtZ { x, y, z, delete } => {
                self.kitty_delete_selection(PlacementSelection::wire_cell(x, y, Some(z)), delete)
            }
            KittyImageDelete::DeleteColumn { x, delete } => {
                self.kitty_delete_selection(PlacementSelection::column(x), delete)
            }
            KittyImageDelete::DeleteRow { y, delete } => {
                self.kitty_delete_selection(PlacementSelection::row(y), delete)
            }
            KittyImageDelete::DeleteZ { z, delete } => {
                let keys = self
                    .kitty_img
                    .placements
                    .iter()
                    .filter_map(|(&key, p)| (p.z == z).then_some(key))
                    .chain(self.kitty_img.projected.iter().filter_map(|(&key, node)| {
                        (node.parent.is_some() && node.placement.z_index.unwrap_or(0) == z)
                            .then_some(key)
                    }))
                    .collect();
                self.kitty_selected_for_deletion(keys, delete)
            }
            KittyImageDelete::AnimationFrames {
                image_id,
                image_number,
                frame_number,
                delete,
            } => {
                let id = match (
                    image_id.filter(|&id| id != 0),
                    image_number.filter(|&n| n != 0),
                ) {
                    (Some(id), None) => Some(id),
                    (None, Some(number)) => self.kitty_img.id_for_number(number),
                    _ => None,
                };
                if let Some(id) = id {
                    self.kitty_delete_frame(id, frame_number.unwrap_or(0), delete);
                }
                return;
            }
        };
        self.kitty_img
            .collect_kitty_dependents(&mut keys, &mut free_ids);
        if let KittyImageDelete::ByImageId {
            image_id,
            delete: false,
            ..
        } = what
        {
            free_ids.remove(&image_id);
        }
        self.detach_kitty_cells(&keys);
        self.kitty_img.discard_placements(&keys);
        if self.kitty_img.release_unplaced_images(free_ids) {
            self.image_cache.clear();
        }
    }

    fn kitty_delete_frame(&mut self, id: u32, frame_number: u32, delete: bool) {
        let Some(image) = self.kitty_img.id_to_data.get(&id) else {
            return;
        };
        let mut data = image.data();
        let ImageDataType::AnimRgba8 {
            width,
            height,
            durations,
            frames,
            hashes,
        } = &mut *data
        else {
            drop(data);
            if delete {
                self.kitty_delete(KittyImageDelete::ByImageId {
                    image_id: id,
                    placement_id: None,
                    delete: true,
                });
            }
            return;
        };

        // Kitty treats an omitted or zero number as the first frame, and
        // numbers above the end as the last frame.
        let removed = (frame_number.saturating_sub(1) as usize).min(frames.len() - 1);
        let freed = frames.remove(removed).len();
        durations.remove(removed);
        hashes.remove(removed);
        let remaining = frames.len();
        let root_gap = durations[0];
        if remaining == 1 {
            *data = ImageDataType::Rgba8 {
                data: frames.pop().unwrap(),
                width: *width,
                height: *height,
                hash: hashes[0],
            };
        }
        if let Some(playback) = self.kitty_img.playback.get_mut(&id) {
            playback.frame_removed(
                &data,
                removed,
                remaining,
                root_gap,
                self.kitty_animation_time.unwrap_or_else(Instant::now),
            );
        }
        drop(data);
        self.kitty_img.used_memory = self.kitty_img.used_memory.saturating_sub(freed);
        self.image_cache.clear();
    }

    fn kitty_delete_selection(
        &self,
        selection: Option<PlacementSelection>,
        delete: bool,
    ) -> (HashSet<Key>, HashSet<u32>) {
        let mut keys = HashSet::new();
        let screen = self.screen();
        let Some(selection) = selection.filter(|p| {
            p.column.is_none_or(|x| x < screen.physical_cols)
                && p.row.is_none_or(|y| y < screen.physical_rows)
        }) else {
            return (keys, HashSet::new());
        };
        let rows = selection.row.map_or(0..screen.scrollback_rows(), |y| {
            let row = screen.phys_row(y as crate::VisibleRowIndex);
            row..row + 1
        });
        let columns = selection
            .column
            .map_or(0..screen.physical_cols, |x| x..x + 1);
        let mut collect = |attrs: &wezterm_cell::CellAttributes| {
            keys.extend(
                attrs
                    .images()
                    .into_iter()
                    .flatten()
                    .filter(|tile| selection.z.is_none_or(|z| z == tile.z_index()))
                    .filter_map(|tile| self.kitty_img.tile_key(&tile)),
            );
        };
        let oldest = if rows.len() > 1 {
            self.kitty_img.placements.values().map(|p| p.created).min()
        } else {
            Some(0)
        };
        for line in screen
            .physical_lines()
            .skip(rows.start)
            .take(rows.len())
            .filter(|line| oldest.is_some_and(|seq| line.changed_since(seq.saturating_sub(1))))
        {
            if let Some(x) = selection.column {
                if let Some(cell) = line.get_cell(x) {
                    collect(cell.attrs());
                }
            } else {
                for cell in line
                    .visible_cells()
                    .take_while(|cell| cell.cell_index() < screen.physical_cols)
                {
                    collect(cell.attrs());
                }
            }
        }
        self.find_kitty_relatives(&mut keys, rows, columns, selection.z);
        self.kitty_selected_for_deletion(keys, delete)
    }

    fn kitty_selected_for_deletion(
        &self,
        mut keys: HashSet<Key>,
        delete: bool,
    ) -> (HashSet<Key>, HashSet<u32>) {
        let release: HashSet<_> = keys
            .iter()
            .map(|key| key.0)
            .filter(|&id| delete && id != 0)
            .collect();
        if !release.is_empty() {
            // Erased cells can leave stale placement entries. Drop such entries
            // for the selected images so only live or historical tiles pin data.
            let siblings: Vec<_> = self
                .kitty_img
                .placements
                .keys()
                .filter(|key| release.contains(&key.0) && !keys.contains(key))
                .copied()
                .collect();
            if !siblings.is_empty() {
                let (retained, _) = self.kitty_img.physical_keys(self.screen());
                keys.extend(siblings.into_iter().filter(|key| !retained.contains(key)));
            }
        }
        (keys, release)
    }

    fn detach_kitty_cells(&mut self, keys: &HashSet<Key>) {
        // The cockpit replaces a single stationary image every frame. Touch
        // only its insertion rows while the screen's geometry is unchanged.
        if keys.len() == 1 {
            let key = keys.iter().next().unwrap();
            if let Some(placement) = self.kitty_img.placements.get(key) {
                if placement.row_revision == self.screen().placement_row_revision {
                    return self.kitty_remove_placement_from_model(
                        key.0,
                        Some(key.1),
                        placement.bounds,
                    );
                }
            }
        }
        let state = &self.kitty_img;
        let Some(oldest) = keys
            .iter()
            .filter_map(|key| state.placements.get(key))
            .map(|p| p.created)
            .min()
        else {
            return;
        };
        // Reflow and line edits can move a tile outside its insertion rectangle.
        // Scan retained rows once, expanding only lines with matching tiles.
        let seqno = self.seqno;
        let mut hits = Vec::new();
        self.screen.for_each_phys_line_mut(|_, line| {
            if !line.changed_since(oldest.saturating_sub(1)) {
                return;
            }
            for cell in line.visible_cells() {
                for tile in cell.attrs().images().into_iter().flatten() {
                    if let Some(key) = state.tile_key(&tile) {
                        if keys.contains(&key) {
                            hits.push((cell.cell_index(), key));
                        }
                    }
                }
            }
            if !hits.is_empty() {
                let cells = line.cells_mut_for_attr_changes_only();
                for (x, (id, p)) in hits.drain(..) {
                    cells[x]
                        .attrs_mut()
                        .detach_image_with_placement(id, Some(p));
                }
                line.update_last_change_seqno(seqno);
            }
        });
    }

    pub(super) fn reset_kitty_images(&mut self) {
        let keys = self.kitty_img.placements.keys().copied().collect();
        self.detach_kitty_cells(&keys);
        self.kitty_img = Default::default();
        self.image_cache.clear();
    }

    fn kitty_send_response(
        &mut self,
        verbosity: KittyImageVerbosity,
        success: bool,
        image_id: Option<u32>,
        image_no: Option<u32>,
        message: String,
    ) {
        match verbosity {
            KittyImageVerbosity::Verbose => {}
            KittyImageVerbosity::OnlyErrors => {
                if success {
                    return;
                }
            }
            KittyImageVerbosity::Quiet => {
                return;
            }
        }

        log::trace!("Query Response: {}", message);

        match (image_id, image_no) {
            (Some(id), Some(no)) => {
                write!(self.writer, "\x1b_GI={},i={};{}\x1b\\", no, id, message).ok();
            }
            (Some(id), None) => {
                write!(self.writer, "\x1b_Gi={};{}\x1b\\", id, message).ok();
            }
            (None, Some(no)) => {
                write!(self.writer, "\x1b_GI={};{}\x1b\\", no, message).ok();
            }
            (None, None) => {
                write!(self.writer, "\x1b_G{}\x1b\\", message).ok();
            }
        }
        self.writer.flush().ok();
    }

    fn kitty_frame_compose(
        &mut self,
        frame: KittyImageFrameCompose,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        self.image_cache.clear();
        let image_id = match frame.image_number.filter(|&n| n != 0) {
            Some(no) => match self.kitty_img.id_for_number(no) {
                Some(id) => id,
                None => {
                    self.kitty_send_response(
                        verbosity,
                        false,
                        frame.image_id,
                        frame.image_number,
                        "ENOENT".to_string(),
                    );
                    anyhow::bail!("no such image_number {}", no);
                }
            },
            None => frame.image_id.ok_or_else(|| {
                self.kitty_send_response(
                    verbosity,
                    false,
                    frame.image_id,
                    frame.image_number,
                    "ENOENT".to_string(),
                );
                anyhow::anyhow!("no image_id")
            })?,
        };

        let src_frame = frame.source_frame.ok_or_else(|| {
            self.kitty_send_response(
                verbosity,
                false,
                frame.image_id,
                frame.image_number,
                "ENOENT".to_string(),
            );
            anyhow::anyhow!("missing source frame")
        })? as usize;
        let target_frame = frame.target_frame.ok_or_else(|| {
            self.kitty_send_response(
                verbosity,
                false,
                frame.image_id,
                frame.image_number,
                "ENOENT".to_string(),
            );
            anyhow::anyhow!("missing target frame")
        })? as usize;

        let img = self
            .kitty_img
            .id_to_data
            .get(&image_id)
            .ok_or_else(|| anyhow::anyhow!("invalid image id {}", image_id))?;

        let mut img = img.data();
        match &mut *img {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                anyhow::bail!("invalid image type")
            }
            ImageDataType::Rgba8 {
                width,
                height,
                data,
                hash,
            } => {
                anyhow::ensure!(
                    src_frame == target_frame && src_frame == 1,
                    "src_frame={} target_frame={} but there is only a single frame",
                    src_frame,
                    target_frame
                );

                let src = clip_view(
                    *width,
                    *height,
                    data.as_mut_slice(),
                    frame.src_x,
                    frame.src_y,
                    frame.w,
                    frame.h,
                )?;

                let mut dest: ImageBuffer<Rgba<u8>, &mut [u8]> =
                    ImageBuffer::from_raw(*width, *height, data.as_mut_slice())
                        .ok_or_else(|| anyhow::anyhow!("ill formed image"))?;

                blit(
                    &mut dest,
                    &src,
                    frame.x.unwrap_or(0),
                    frame.y.unwrap_or(0),
                    frame.composition_mode,
                )?;

                drop(dest);

                *hash = ImageDataType::hash_bytes(data);
            }
            ImageDataType::AnimRgba8 {
                width,
                height,
                frames,
                hashes,
                ..
            } => {
                anyhow::ensure!(
                    src_frame > 0 && src_frame <= frames.len(),
                    "src_frame {} is out of range",
                    src_frame
                );
                anyhow::ensure!(
                    target_frame > 0 && target_frame <= frames.len(),
                    "target_frame {} is out of range",
                    target_frame
                );

                let src = clip_view(
                    *width,
                    *height,
                    frames[src_frame - 1].as_mut_slice(),
                    frame.src_x,
                    frame.src_y,
                    frame.w,
                    frame.h,
                )?;

                let mut dest: ImageBuffer<Rgba<u8>, &mut [u8]> =
                    ImageBuffer::from_raw(*width, *height, frames[target_frame - 1].as_mut_slice())
                        .ok_or_else(|| anyhow::anyhow!("ill formed image"))?;

                blit(
                    &mut dest,
                    &src,
                    frame.x.unwrap_or(0),
                    frame.y.unwrap_or(0),
                    frame.composition_mode,
                )?;

                drop(dest);
                hashes[target_frame - 1] = ImageDataType::hash_bytes(&frames[target_frame - 1]);
            }
        }

        Ok(())
    }

    fn kitty_frame_transmit(
        &mut self,
        mut transmit: KittyImageTransmit,
        frame: KittyImageFrame,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        if let Some(no) = transmit.image_number.take().filter(|&n| n != 0) {
            match self.kitty_img.id_for_number(no) {
                Some(id) => {
                    transmit.image_id.replace(id);
                }
                None => {
                    transmit.image_number.replace(no);
                }
            }
        }

        let (image_id, image_number, img) = self.kitty_img_transmit_inner(transmit)?;

        let img = match img.decode() {
            ImageDataType::Rgba8 {
                data,
                width,
                height,
                ..
            } => RgbaImage::from_vec(width, height, data)
                .ok_or_else(|| anyhow::anyhow!("data isn't rgba8"))?,
            wat => anyhow::bail!("data isn't rgba8 {:?}", wat),
        };

        let background_pixel = frame.background_pixel.unwrap_or(0);
        let background_pixel = Rgba([
            ((background_pixel >> 24) & 0xff) as u8,
            ((background_pixel >> 16) & 0xff) as u8,
            ((background_pixel >> 8) & 0xff) as u8,
            (background_pixel & 0xff) as u8,
        ]);

        self.kitty_img.refresh_memory();
        let anim = match self.kitty_img.id_to_data.get(&image_id) {
            Some(anim) => anim,
            None => {
                self.kitty_send_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    "ENOENT".to_string(),
                );
                anyhow::bail!(
                    "no matching image id {} in id_to_data for image_number {:?}",
                    image_id,
                    image_number
                )
            }
        };

        self.image_cache.clear();
        let old_len = anim.len();
        let mut anim = anim.data();
        let (width, height) = anim.dimensions()?;
        let (frame_count, append) = match &*anim {
            ImageDataType::Rgba8 { .. } => (1usize, frame.frame_number != Some(1)),
            ImageDataType::AnimRgba8 { frames, .. } => (
                frames.len(),
                frame
                    .frame_number
                    .is_none_or(|n| n as usize == frames.len() + 1),
            ),
            _ => (0, false),
        };
        let growth = if append {
            width as usize * height as usize * 4
        } else {
            0
        };
        anyhow::ensure!(
            !append || frame_count < 256,
            "kitty animation exceeds 256 frames"
        );
        anyhow::ensure!(
            old_len.saturating_add(growth) <= 32 * 1024 * 1024,
            "kitty animation exceeds 32 MiB"
        );
        anyhow::ensure!(
            self.kitty_img
                .used_memory
                .saturating_add(self.inactive_kitty_img.used_memory)
                .saturating_add(growth)
                <= 64 * 1024 * 1024,
            "kitty image table exceeds 64 MiB"
        );
        let x = frame.x.unwrap_or(0);
        let y = frame.y.unwrap_or(0);
        let frame_gap = Duration::from_millis(match frame.duration_ms {
            None | Some(0) => 40,
            Some(n) => n.max(0) as u64,
        });
        let old_gap = self
            .kitty_animation_time
            .and_then(|_| self.kitty_img.playback.get(&image_id)?.frame_gap(&anim));

        match &mut *anim {
            ImageDataType::EncodedLease(_) | ImageDataType::EncodedFile(_) => {
                anyhow::bail!("Expected decoded image for image id {}", image_id)
            }
            ImageDataType::Rgba8 {
                data,
                width,
                height,
                hash,
            } => {
                let base_frame = match frame.base_frame {
                    Some(1) => Some(1),
                    None => None,
                    Some(n) => anyhow::bail!(
                        "attempted to copy frame {} but there is only a single frame",
                        n
                    ),
                };

                match frame.frame_number {
                    Some(1) => {
                        // Edit in place
                        let len = data.len();
                        let mut anim_img: ImageBuffer<Rgba<u8>, &mut [u8]> =
                            ImageBuffer::from_raw(*width, *height, data.as_mut_slice())
                                .ok_or_else(|| {
                                    anyhow::anyhow!(
                                        "ImageBuffer::from_raw failed for single \
                                         frame of {}x{} ({} bytes)",
                                        width,
                                        height,
                                        len
                                    )
                                })?;

                        blit(&mut anim_img, &img, x, y, frame.composition_mode)?;

                        drop(anim_img);
                        *hash = ImageDataType::hash_bytes(data);
                    }
                    Some(2) | None => {
                        // Create a second frame

                        let mut new_frame = if base_frame.is_some() {
                            RgbaImage::from_vec(*width, *height, data.clone()).unwrap()
                        } else {
                            RgbaImage::from_pixel(*width, *height, background_pixel)
                        };

                        blit(&mut new_frame, &img, x, y, frame.composition_mode)?;

                        let new_frame_data = new_frame.into_vec();
                        let new_frame_hash = ImageDataType::hash_bytes(&new_frame_data);

                        let frames = vec![std::mem::take(data), new_frame_data];
                        let durations = vec![
                            self.kitty_img
                                .playback
                                .get(&image_id)
                                .map_or(Duration::ZERO, |p| p.root_gap),
                            frame_gap,
                        ];
                        let hashes = vec![*hash, new_frame_hash];

                        *anim = ImageDataType::AnimRgba8 {
                            width: *width,
                            height: *height,
                            frames,
                            durations,
                            hashes,
                        };
                    }
                    Some(n) => anyhow::bail!(
                        "attempted to edit frame {} but there is only a single frame",
                        n
                    ),
                }
            }
            ImageDataType::AnimRgba8 {
                width,
                height,
                frames,
                durations,
                hashes,
            } => {
                let frame_no = frame.frame_number.unwrap_or(frames.len() as u32 + 1);
                if frame_no == frames.len() as u32 + 1 {
                    // Append a new frame

                    let mut new_frame = match frame.base_frame {
                        None => RgbaImage::from_pixel(*width, *height, background_pixel),
                        Some(n) => {
                            let n = n as usize;
                            anyhow::ensure!(
                                n > 0 && n <= frames.len(),
                                "attempted to copy frame {} which is outside range 1-{}",
                                n,
                                frames.len()
                            );
                            RgbaImage::from_vec(*width, *height, frames[n - 1].clone()).unwrap()
                        }
                    };

                    blit(&mut new_frame, &img, x, y, frame.composition_mode)?;

                    let new_frame_data = new_frame.into_vec();
                    let new_frame_hash = ImageDataType::hash_bytes(&new_frame_data);

                    frames.push(new_frame_data);
                    hashes.push(new_frame_hash);
                    durations.push(frame_gap);
                } else {
                    anyhow::ensure!(
                        frame_no > 0 && frame_no <= frames.len() as u32,
                        "attempted to edit frame {} which is outside range 1-{}",
                        frame_no,
                        frames.len()
                    );

                    let frame_no = frame_no as usize;

                    let len = frames[frame_no - 1].len();
                    let mut anim_img: ImageBuffer<Rgba<u8>, &mut [u8]> =
                        ImageBuffer::from_raw(*width, *height, frames[frame_no - 1].as_mut_slice())
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "ImageBuffer::from_raw failed for single \
                                         frame of {}x{} ({} bytes)",
                                    width,
                                    height,
                                    len
                                )
                            })?;

                    blit(&mut anim_img, &img, x, y, frame.composition_mode)?;

                    drop(anim_img);
                    hashes[frame_no - 1] = ImageDataType::hash_bytes(&frames[frame_no - 1]);
                    if frame.duration_ms.is_some() {
                        durations[frame_no - 1] = frame_gap;
                    }
                }
            }
        }

        if frame.frame_number == Some(1) && frame.duration_ms.is_some() {
            self.kitty_img
                .playback
                .entry(image_id)
                .or_default()
                .root_gap = frame_gap;
        }
        if let Some(playback) = self.kitty_img.playback.get_mut(&image_id) {
            playback.frames_changed(
                &anim,
                self.kitty_animation_time.unwrap_or_else(Instant::now),
                old_gap,
            );
        }
        drop(anim);
        self.kitty_img.used_memory += growth;
        Ok(())
    }

    fn kitty_img_transmit_inner(
        &mut self,
        transmit: KittyImageTransmit,
    ) -> anyhow::Result<(u32, Option<u32>, ImageDataType)> {
        log::trace!("transmit {:?}", transmit);
        let (id, no) = match (
            transmit.image_id.filter(|&id| id != 0),
            transmit.image_number.filter(|&n| n != 0),
        ) {
            (Some(_), Some(_)) => {
                // TODO: send an EINVAL error back here
                anyhow::bail!("cannot use both i= and I= in the same request");
            }
            (None, None) => (0, None),
            (Some(id), None) => (id, None),
            (None, Some(no)) => {
                let id = self
                    .kitty_img
                    .max_image_id
                    .checked_add(1)
                    .context("kitty image id space exhausted")?;
                (id, Some(no))
            }
        };

        let data = transmit
            .data
            .load_data()
            .context("data should have been materialized in coalesce_kitty_accumulation")?;

        let data = match transmit.compression {
            KittyImageCompression::None => data,
            KittyImageCompression::Deflate => {
                miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, 32 * 1024 * 1024)
                    .map_err(|e| anyhow::anyhow!("decompressing data: {:?}", e))?
            }
        };

        let img = match transmit.format {
            None | Some(KittyImageFormat::Rgba) | Some(KittyImageFormat::Rgb) => {
                let (width, height) = match (transmit.width, transmit.height) {
                    (Some(w), Some(h)) => (w, h),
                    _ => {
                        anyhow::bail!("missing width/height info for kitty img");
                    }
                };

                check_image_dimensions(width, height)?;
                anyhow::ensure!(
                    width as u64 * height as u64 * 4 <= 32 * 1024 * 1024,
                    "kitty image exceeds 32 MiB"
                );

                let data = match transmit.format {
                    Some(KittyImageFormat::Rgb) => {
                        let img = DynamicImage::ImageRgb8(
                            RgbImage::from_vec(width, height, data)
                                .ok_or_else(|| anyhow::anyhow!("failed to decode image"))?,
                        );
                        let img = img.into_rgba8();
                        img.into_vec()
                    }
                    _ => data,
                };

                anyhow::ensure!(
                    width * height * 4 == data.len() as u32,
                    "transmit data len is {} but it doesn't match width*height*4 {}x{}x4 = {}",
                    data.len(),
                    width,
                    height,
                    width * height * 4
                );

                ImageDataType::new_single_frame(width, height, data)
            }
            Some(KittyImageFormat::Png) => {
                let info = dimensions(&data)?;
                check_image_dimensions(info.width, info.height)?;
                anyhow::ensure!(
                    info.width as u64 * info.height as u64 * 4 <= 32 * 1024 * 1024,
                    "kitty image exceeds 32 MiB"
                );
                let mut reader = image::ImageReader::with_format(
                    std::io::Cursor::new(&data),
                    image::ImageFormat::Png,
                );
                let mut limits = image::Limits::default();
                limits.max_image_width = Some(info.width);
                limits.max_image_height = Some(info.height);
                limits.max_alloc = Some(32 * 1024 * 1024);
                reader.limits(limits);
                let decoded = reader.decode().context("decode png")?;
                let (width, height) = decoded.dimensions();
                let data = decoded.into_rgba8().into_vec();
                ImageDataType::new_single_frame(width, height, data)
            }
        };

        Ok((id, no, img))
    }

    fn kitty_img_transmit(
        &mut self,
        transmit: KittyImageTransmit,
        display: Option<KittyImagePlacement>,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        let (image_id, image_number, img) = self.kitty_img_transmit_inner(transmit)?;
        if image_id == 0 && display.as_ref().is_none_or(|p| p.virtual_placement) {
            // Neither a later put nor a Unicode placeholder has an image ID
            // with which to retrieve these bytes.
            return Ok(());
        }

        // An image id owns its mutable animation frames. Equal initial pixels under
        // two ids may subsequently receive different frames and playback commands.
        let img = Arc::new(ImageData::with_data(img));
        let replacing = self.kitty_img.id_to_data.contains_key(&image_id);
        if self
            .kitty_img
            .check_image_capacity(image_id, &img, &self.inactive_kitty_img)
            .is_err()
        {
            self.reclaim_missing_kitty_placements(image_id);
            self.kitty_img
                .check_image_capacity(image_id, &img, &self.inactive_kitty_img)?;
        }
        if image_id != 0 {
            if replacing {
                self.kitty_delete(KittyImageDelete::ByImageId {
                    image_id,
                    placement_id: None,
                    delete: false,
                });
                self.kitty_img.remove_data_for_id(image_id);
            }
            self.kitty_img.max_image_id = self.kitty_img.max_image_id.max(image_id);
            self.kitty_img.used_memory += img.len();
            self.kitty_img.id_to_data.insert(image_id, Arc::clone(&img));
        }
        if let Some(number) = image_number {
            self.kitty_img
                .numbered_images
                .entry(number)
                .or_default()
                .push(image_id);
        }

        if image_number.is_some() {
            self.kitty_send_response(
                verbosity,
                true,
                Some(image_id),
                image_number,
                "OK".to_string(),
            );
        }

        if let Some(placement) = display {
            self.kitty_place_data(image_id, image_number, img, placement, verbosity)?;
        }
        Ok(())
    }

    fn coalesce_kitty_accumulation(&mut self, img: KittyImage) -> anyhow::Result<KittyImage> {
        self.kitty_img.accumulated_bytes = 0;
        if self.kitty_img.accumulator.is_empty() {
            return Ok(img);
        }
        let final_verbosity = img.verbosity();
        let chunks = std::mem::take(&mut self.kitty_img.accumulator);
        let mut chunks = chunks.into_iter().chain(std::iter::once(img));
        let mut first = chunks.next().unwrap();
        let head = transmission(&mut first).unwrap();
        let load = |part: &mut KittyImageTransmit| {
            std::mem::replace(&mut part.data, KittyImageData::DirectBin(Vec::new())).load_data()
        };
        let mut bytes = load(head)?;
        for mut chunk in chunks {
            let part =
                transmission(&mut chunk).context("expected kitty transmission continuation")?;
            bytes.append(&mut load(part)?);
        }
        head.data = KittyImageData::DirectBin(bytes);
        head.more_data_follows = false;
        match &mut first {
            KittyImage::TransmitData { verbosity, .. }
            | KittyImage::TransmitDataAndDisplay { verbosity, .. }
            | KittyImage::TransmitFrame { verbosity, .. } => *verbosity = final_verbosity,
            _ => unreachable!(),
        }
        Ok(first)
    }
}

fn transmission(img: &mut KittyImage) -> Option<&mut KittyImageTransmit> {
    match img {
        KittyImage::TransmitData { transmit, .. }
        | KittyImage::TransmitDataAndDisplay { transmit, .. }
        | KittyImage::TransmitFrame { transmit, .. } => Some(transmit),
        _ => None,
    }
}

/// Make a copy of the source region.
/// Ideally we wouldn't need this, but Rust's mutability rules
/// make it very awkward to mutably reference a frame while
/// an immutable reference exists to a separate frame.
fn clip_view(
    width: u32,
    height: u32,
    data: &mut [u8],
    src_x: Option<u32>,
    src_y: Option<u32>,
    view_width: Option<u32>,
    view_height: Option<u32>,
) -> anyhow::Result<RgbaImage> {
    let src = ImageBuffer::from_raw(width, height, data)
        .ok_or_else(|| anyhow::anyhow!("ill formed image"))?;

    let src_x = src_x.unwrap_or(0);
    let src_y = src_y.unwrap_or(0);

    let view_width = view_width.unwrap_or(width);
    let view_height = view_height.unwrap_or(height);

    let (view_width, view_height) =
        image::imageops::overlay_bounds((width, height), (view_width, view_height), src_x, src_y);

    let view = src.view(src_x, src_y, view_width, view_height);

    let mut tmp = RgbaImage::new(view_width, view_height);
    tmp.copy_from(&*view, 0, 0).context("copy source image")?;
    Ok(tmp)
}

fn blit<D, S, P>(
    dest: &mut D,
    src: &S,
    x: u32,
    y: u32,
    mode: KittyFrameCompositionMode,
) -> anyhow::Result<()>
where
    D: GenericImage<Pixel = P>,
    S: GenericImageView<Pixel = P>,
{
    match mode {
        KittyFrameCompositionMode::Overwrite => {
            ::image::imageops::replace(dest, src, x.into(), y.into());
        }
        KittyFrameCompositionMode::AlphaBlending => {
            ::image::imageops::overlay(dest, src, x.into(), y.into());
        }
    }
    Ok(())
}
