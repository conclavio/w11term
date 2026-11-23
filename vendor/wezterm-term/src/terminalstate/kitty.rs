use crate::color::ColorPalette;
use crate::terminalstate::image::*;
use crate::terminalstate::{ImageAttachParams, PlacementInfo};
use crate::TerminalState;
use ::image::{
    DynamicImage, GenericImage, GenericImageView, ImageBuffer, RgbImage, Rgba, RgbaImage,
};
use anyhow::Context;
use std::collections::{HashMap, HashSet};
use std::convert::TryFrom;
use std::fmt::Write as _;
use std::io::Write;
use std::ops::Range;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};
mod feature_query;
mod indexed;
mod placement;
mod playback;
mod scroll;
use indexed::{IndexedImages, IndexedPixels};
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
enum AnimationError {
    Missing,
    Full,
}

impl std::fmt::Display for AnimationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Missing => "animation image or frame is unavailable",
            Self::Full => "animation storage limit reached",
        })
    }
}

impl std::error::Error for AnimationError {}

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
    indexed: IndexedImages,
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
    pub(super) fn has_physical_placements(&self) -> bool {
        !self.placements.is_empty()
    }

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
            let freed = data.len() + self.indexed.remove(&data);
            self.used_memory = self.used_memory.saturating_sub(freed);
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
            .map(|data| data.len() + self.indexed.len_for(data))
            .sum();
    }

    fn check_image_capacity(
        &mut self,
        image_id: u32,
        bytes: usize,
        inactive: &Self,
    ) -> anyhow::Result<()> {
        self.refresh_memory();
        self.prune_unreferenced();
        let old = self
            .id_to_data
            .get(&image_id)
            .map_or(0, |data| data.len() + self.indexed.len_for(data));
        anyhow::ensure!(
            self.used_memory
                .saturating_sub(old)
                .saturating_add(inactive.used_memory)
                .saturating_add(bytes)
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
                let freed = img.len() + self.indexed.remove(&img);
                self.used_memory = self.used_memory.saturating_sub(freed);
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
            let indexed = &mut self.indexed;
            self.id_to_data.retain(|id, data| {
                if referenced.contains(id) || freed > target {
                    true
                } else {
                    freed += data.len() + indexed.remove(data);
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
    /// Update live indexed graphics before freezing the active screen for display.
    pub fn recolor_indexed_kitty_images(&self, palette: &ColorPalette) {
        self.kitty_img.indexed.recolor(palette);
    }

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
        let image_id = image_id.filter(|&id| id != 0);
        let image_number = image_number.filter(|&n| n != 0);
        let wire_placement = placement.placement_id.filter(|&p| p != 0);
        if image_id.is_none() && image_number.is_none() {
            return Ok(());
        }
        if image_id.is_some() && image_number.is_some() {
            self.kitty_send_placement_response(
                verbosity,
                false,
                image_id,
                image_number,
                wire_placement,
                "EINVAL:specify either an image ID or an image number",
            );
            return Ok(());
        }
        let resolved = image_id.or_else(|| self.kitty_img.id_for_number(image_number?));
        let Some((image_id, img)) = resolved.and_then(|id| {
            self.kitty_img
                .id_to_data
                .get(&id)
                .map(|img| (id, Arc::clone(img)))
        }) else {
            self.kitty_send_placement_response(
                verbosity,
                false,
                image_id,
                image_number,
                wire_placement,
                "ENOENT:image data is unavailable",
            );
            return Ok(());
        };

        log::trace!(
            "kitty_img_place image_id {:?} image_no {:?} placement {:?} verb {:?}",
            image_id,
            image_number,
            placement,
            verbosity
        );
        self.kitty_place_data(image_id, image_number, img, None, placement, verbosity)
    }

    fn kitty_place_data(
        &mut self,
        image_id: u32,
        image_number: Option<u32>,
        img: Arc<ImageData>,
        indexed: Option<IndexedPixels>,
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
                self.kitty_send_placement_response(
                    verbosity,
                    false,
                    Some(image_id),
                    image_number,
                    wire_placement,
                    reason,
                );
                return Ok(());
            }
        };
        if !replacing
            && self.kitty_img.placement_count() + self.inactive_kitty_img.placement_count() >= 1024
        {
            self.kitty_send_placement_response(
                verbosity,
                false,
                Some(image_id),
                image_number,
                wire_placement,
                "ENOSPC",
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
            self.kitty_img.projected.insert(
                key,
                ProjectedPlacement {
                    placement,
                    parent,
                    row_limit: u32::MAX,
                },
            );
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
            });
            let bounds = match bounds {
                Ok(bounds) => bounds,
                Err(err) => {
                    self.kitty_send_placement_response(
                        verbosity,
                        false,
                        Some(image_id),
                        image_number,
                        wire_placement,
                        &format!("EINVAL:{err:#}"),
                    );
                    return Ok(());
                }
            };

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
            self.kitty_img.used_memory +=
                img.len() + indexed.as_ref().map_or(0, IndexedPixels::len);
            self.kitty_img.indexed.insert(&img, indexed);
            self.kitty_img.anonymous_data.insert(key, img);
        }
        self.kitty_send_placement_response(
            verbosity,
            true,
            Some(image_id),
            image_number,
            wire_placement,
            "OK",
        );
        Ok(())
    }

    fn kitty_img_inner(&mut self, img: KittyImage) -> anyhow::Result<()> {
        let first = self.kitty_img.accumulator.first().unwrap_or(&img);
        let address = transmission_address(first);
        let verbosity = transfer_verbosity(first.verbosity(), img.verbosity());
        let image = match self.coalesce_kitty_accumulation(img) {
            Ok(image) => image,
            Err(err) => {
                if let Some(address) = address {
                    self.kitty_send_reply(
                        verbosity,
                        false,
                        &address,
                        &format!("EINVAL:invalid kitty upload chunk: {err:#}"),
                    );
                }
                return Ok(());
            }
        };
        match image {
            KittyImage::Query {
                transmit,
                verbosity,
            } => {
                if let Some(id) = transmit.image_id.filter(|&id| id != 0) {
                    if !matches!(verbosity, KittyImageVerbosity::Quiet) {
                        self.kitty_send_probe_result(id, verbosity, validate_kitty_probe(transmit));
                    }
                }
                Ok(())
            }
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
            } => {
                let (id, number, requested) =
                    (transmit.image_id, transmit.image_number, frame.frame_number);
                let result = self.kitty_frame_transmit(transmit, frame);
                self.kitty_send_animation_result(
                    verbosity,
                    id,
                    number,
                    requested,
                    result.map(Some),
                );
                Ok(())
            }
            _ => anyhow::bail!("impossible KittyImage variant"),
        }
    }

    pub(crate) fn kitty_img(&mut self, img: KittyImage) -> anyhow::Result<()> {
        log::trace!("{:?}", img);
        if !self.config.enable_kitty_graphics() {
            return Ok(());
        }
        let probe_pending = self
            .kitty_img
            .accumulator
            .first()
            .and_then(probe_reply)
            .is_some();
        // A standalone probe has its own metadata. Process it independently
        // so even a rejected probe leaves an in-flight image upload intact.
        let mut img = match img {
            KittyImage::Query {
                transmit,
                verbosity,
            } if !probe_pending
                || transmit.image_id.is_some()
                || transmit.image_number.is_some() =>
            {
                let Some(query) = self.kitty_probe_head(transmit, verbosity) else {
                    return Ok(());
                };
                query
            }
            other => other,
        };
        if probe_pending && starts_image_upload(&img) {
            self.cancel_kitty_probe("EINVAL:upload probe was interrupted");
        }
        if let Some(transmit) = transmission(&mut img) {
            let bytes = match &transmit.data {
                KittyImageData::Direct(data) => data.len(),
                KittyImageData::DirectBin(data) => data.len(),
                _ => {
                    self.reject_kitty_transmission(
                        &img,
                        "ENOTSUP:w11term requires direct kitty image transport",
                    );
                    return Ok(());
                }
            };
            if self
                .kitty_img
                .accumulated_bytes
                .saturating_add(self.inactive_kitty_img.accumulated_bytes)
                .saturating_add(bytes)
                > 48 * 1024 * 1024
            {
                self.reject_kitty_transmission(&img, "ENOSPC:kitty transmission exceeds 48 MiB");
                return Ok(());
            }
            if transmit.more_data_follows {
                if self.kitty_img.accumulator.len() + self.inactive_kitty_img.accumulator.len()
                    >= 16384
                {
                    self.reject_kitty_transmission(
                        &img,
                        "ENOSPC:too many kitty transmission chunks",
                    );
                    return Ok(());
                }
                self.kitty_img.accumulated_bytes += bytes;
                self.kitty_img.accumulator.push(img);
                return Ok(());
            }
            return self.kitty_img_inner(img);
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
            | KittyImage::TransmitFrame { .. }
            | KittyImage::Query { .. } => unreachable!("handled before dispatch"),
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
                let control = playback::Control {
                    state,
                    edit_frame,
                    gap,
                    current,
                    loops,
                };
                let result =
                    self.kitty_animation_image(image_id, image_number)
                        .and_then(|(id, img)| {
                            if self.kitty_img.indexed.len_for(&img) != 0 {
                                anyhow::bail!("indexed images do not support animation");
                            }
                            let now = self.kitty_animation_time.unwrap_or_else(Instant::now);
                            self.kitty_img
                                .playback
                                .entry(id)
                                .or_default()
                                .control(&mut img.data(), control, now)
                                .map_err(|code| match code {
                                    "ENOENT" => AnimationError::Missing.into(),
                                    _ => anyhow::anyhow!("invalid animation control"),
                                })
                        });
                // Playback changes are fire-and-forget. Keep addressed failures
                // available to applications that haven't opted out of errors.
                if let Err(err) = result {
                    self.kitty_send_animation_result(
                        verbosity,
                        image_id,
                        image_number,
                        edit_frame,
                        Err(err),
                    );
                }
            }
            KittyImage::ComposeFrame { frame, verbosity } => {
                let (id, number) = (frame.image_id, frame.image_number);
                let result = self.kitty_frame_compose(frame);
                self.kitty_send_animation_result(
                    verbosity,
                    id,
                    number,
                    None,
                    result.map(|()| None),
                );
            }
        };

        Ok(())
    }

    fn kitty_probe_head(
        &mut self,
        transmit: KittyImageTransmit,
        verbosity: KittyImageVerbosity,
    ) -> Option<KittyImage> {
        let id = transmit.image_id.filter(|&id| id != 0)?;
        let number = transmit.image_number.filter(|&number| number != 0);
        let error = if number.is_some() {
            "EINVAL:upload probes require an image ID alone"
        } else if !matches!(
            transmit.data,
            KittyImageData::Direct(_) | KittyImageData::DirectBin(_)
        ) {
            "ENOTSUP:w11term requires direct kitty image transport"
        } else if transmit.more_data_follows {
            if self.kitty_img.accumulator.is_empty() {
                return Some(KittyImage::Query {
                    transmit,
                    verbosity,
                });
            }
            "EBUSY:another kitty transmission is in progress"
        } else {
            if !matches!(verbosity, KittyImageVerbosity::Quiet) {
                self.kitty_send_probe_result(id, verbosity, validate_kitty_probe(transmit));
            }
            return None;
        };
        self.kitty_send_response(verbosity, false, Some(id), number, error.into());
        None
    }

    fn cancel_kitty_probe(&mut self, message: &str) {
        let Some((id, verbosity)) = self.kitty_img.accumulator.first().and_then(probe_reply) else {
            return;
        };
        self.kitty_img.accumulator.clear();
        self.kitty_img.accumulated_bytes = 0;
        self.kitty_send_response(verbosity, false, Some(id), None, message.into());
    }

    fn reject_kitty_transmission(&mut self, incoming: &KittyImage, message: &str) {
        let first = self.kitty_img.accumulator.first().unwrap_or(incoming);
        let address = transmission_address(first).or_else(|| transmission_address(incoming));
        let verbosity = transfer_verbosity(first.verbosity(), incoming.verbosity());
        self.kitty_img.accumulator.clear();
        self.kitty_img.accumulated_bytes = 0;
        if let Some(address) = address {
            self.kitty_send_reply(verbosity, false, &address, message);
        }
    }

    fn kitty_send_probe_result(
        &mut self,
        id: u32,
        verbosity: KittyImageVerbosity,
        result: anyhow::Result<()>,
    ) {
        let message = match &result {
            Ok(()) => "OK".into(),
            Err(err) => format!("EINVAL:{err:#}"),
        };
        self.kitty_send_response(verbosity, result.is_ok(), Some(id), None, message);
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
                let (retained, mut visible) = self.visible_kitty_keys();
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

    fn visible_kitty_keys(&self) -> (HashSet<Key>, HashSet<Key>) {
        let screen = self.screen();
        let (retained, mut visible) = self.kitty_img.physical_keys(screen);
        self.find_kitty_relatives(
            &mut visible,
            screen.phys_row(0)..screen.scrollback_rows(),
            0..screen.physical_cols,
            None,
        );
        (retained, visible)
    }

    pub(super) fn clear_visible_kitty_images(&mut self) {
        if self.kitty_img.placement_count() != 0 {
            let (_, mut visible) = self.visible_kitty_keys();
            self.kitty_img
                .collect_kitty_dependents(&mut visible, &mut HashSet::new());
            self.detach_kitty_cells(&visible);
            self.kitty_img.discard_placements(&visible);
        }
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
        self.kitty_send_placement_response(verbosity, success, image_id, image_no, None, &message);
    }

    fn kitty_send_placement_response(
        &mut self,
        verbosity: KittyImageVerbosity,
        success: bool,
        image_id: Option<u32>,
        image_no: Option<u32>,
        placement_id: Option<u32>,
        message: &str,
    ) {
        self.kitty_send_reply(
            verbosity,
            success,
            &[("I", image_no), ("i", image_id), ("p", placement_id)],
            message,
        );
    }

    fn kitty_send_reply(
        &mut self,
        verbosity: KittyImageVerbosity,
        success: bool,
        fields: &[(&str, Option<u32>)],
        message: &str,
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

        let mut response = String::from("\x1b_G");
        let mut delimiter = "";
        for (key, value) in fields {
            if let Some(value) = value {
                let _ = write!(response, "{delimiter}{key}={value}");
                delimiter = ",";
            }
        }
        let separator = if delimiter.is_empty() { "" } else { ";" };
        let _ = write!(response, "{separator}{message}\x1b\\");
        self.writer.write_all(response.as_bytes()).ok();
        self.writer.flush().ok();
    }

    fn kitty_send_animation_result(
        &mut self,
        verbosity: KittyImageVerbosity,
        id: Option<u32>,
        number: Option<u32>,
        requested_frame: Option<u32>,
        result: anyhow::Result<Option<u32>>,
    ) {
        let id = id.filter(|&id| id != 0);
        let number = number.filter(|&number| number != 0);
        if (id.is_none() && number.is_none()) || matches!(verbosity, KittyImageVerbosity::Quiet) {
            return;
        }
        match result {
            Ok(frame) => self.kitty_send_reply(
                verbosity,
                true,
                &[("i", id), ("I", number), ("r", frame)],
                "OK",
            ),
            Err(err) => {
                let code = match err.downcast_ref::<AnimationError>() {
                    Some(AnimationError::Missing) => "ENOENT",
                    Some(AnimationError::Full) => "ENOSPC",
                    None => "EINVAL",
                };
                self.kitty_send_reply(
                    verbosity,
                    false,
                    &[
                        ("i", id),
                        ("I", number),
                        ("r", requested_frame.filter(|&n| n != 0)),
                    ],
                    &format!("{code}:{err:#}"),
                );
            }
        }
    }

    fn kitty_animation_image(
        &self,
        id: Option<u32>,
        number: Option<u32>,
    ) -> anyhow::Result<(u32, Arc<ImageData>)> {
        let id = id.filter(|&id| id != 0);
        let number = number.filter(|&number| number != 0);
        anyhow::ensure!(
            id.is_none() || number.is_none(),
            "specify either an image ID or an image number"
        );
        let id = id
            .or_else(|| self.kitty_img.id_for_number(number?))
            .ok_or(AnimationError::Missing)?;
        self.kitty_img
            .id_to_data
            .get(&id)
            .map(|img| (id, Arc::clone(img)))
            .ok_or_else(|| AnimationError::Missing.into())
    }

    fn kitty_frame_compose(&mut self, frame: KittyImageFrameCompose) -> anyhow::Result<()> {
        let (_, img) = self.kitty_animation_image(frame.image_id, frame.image_number)?;
        anyhow::ensure!(
            self.kitty_img.indexed.len_for(&img) == 0,
            "indexed images do not support frame composition"
        );
        let mut img = img.data();
        let (width, height) = img.dimensions()?;
        let count = match &*img {
            ImageDataType::Rgba8 { .. } => 1,
            ImageDataType::AnimRgba8 { frames, .. } => frames.len(),
            _ => anyhow::bail!("expected decoded image"),
        };
        let frame_index = |number: Option<u32>| {
            number
                .and_then(|n| n.checked_sub(1))
                .map(|n| n as usize)
                .filter(|&index| index < count)
                .ok_or(AnimationError::Missing)
        };
        let source_index = frame_index(frame.source_frame)?;
        let target_index = frame_index(frame.target_frame)?;
        let (w, h) = (
            frame.w.filter(|&w| w != 0).unwrap_or(width),
            frame.h.filter(|&h| h != 0).unwrap_or(height),
        );
        let (source_x, source_y) = (frame.src_x.unwrap_or(0), frame.src_y.unwrap_or(0));
        let (target_x, target_y) = (frame.x.unwrap_or(0), frame.y.unwrap_or(0));
        let fits = |x, y| w <= width.saturating_sub(x) && h <= height.saturating_sub(y);
        anyhow::ensure!(
            fits(source_x, source_y) && fits(target_x, target_y),
            "animation composition rectangle is out of bounds"
        );
        anyhow::ensure!(
            source_index != target_index
                || source_x.abs_diff(target_x) >= w
                || source_y.abs_diff(target_y) >= h,
            "animation composition rectangles overlap in the same frame"
        );

        let source_data = match &*img {
            ImageDataType::Rgba8 { data, .. } => data,
            ImageDataType::AnimRgba8 { frames, .. } => &frames[source_index],
            _ => unreachable!("validated animation data"),
        };
        let source = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, source_data.as_slice())
            .context("invalid source frame")?;
        let mut region = RgbaImage::new(w, h);
        region.copy_from(&*source.view(source_x, source_y, w, h), 0, 0)?;
        let (pixels, hash) = match &mut *img {
            ImageDataType::Rgba8 { data, hash, .. } => (data, hash),
            ImageDataType::AnimRgba8 { frames, hashes, .. } => {
                (&mut frames[target_index], &mut hashes[target_index])
            }
            _ => unreachable!("validated animation data"),
        };
        let mut target = ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, pixels.as_mut_slice())
            .context("invalid target frame")?;
        blit(
            &mut target,
            &region,
            target_x,
            target_y,
            frame.composition_mode,
        )?;
        *hash = ImageDataType::hash_bytes(pixels);

        self.image_cache.clear();
        Ok(())
    }

    fn kitty_frame_transmit(
        &mut self,
        transmit: KittyImageTransmit,
        frame: KittyImageFrame,
    ) -> anyhow::Result<u32> {
        let (image_id, anim) =
            self.kitty_animation_image(transmit.image_id, transmit.image_number)?;
        anyhow::ensure!(
            indexed::stride(transmit.format.as_ref()).is_none()
                && self.kitty_img.indexed.len_for(&anim) == 0,
            "indexed images do not support animation frames"
        );
        let (img, _) = self.kitty_decode_image(transmit)?;

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
        let old_len = anim.len();
        let mut anim = anim.data();
        let (width, height) = anim.dimensions()?;
        anyhow::ensure!(
            img.width() <= width && img.height() <= height,
            "animation update exceeds image dimensions"
        );
        let frame_count = match &*anim {
            ImageDataType::Rgba8 { .. } => 1,
            ImageDataType::AnimRgba8 { frames, .. } => frames.len() as u32,
            _ => anyhow::bail!("expected decoded image for image id {image_id}"),
        };
        let next = frame_count + 1;
        let frame_no = frame
            .frame_number
            .filter(|&n| n != 0)
            .map_or(next, |n| n.min(next));
        let append = frame_no == next;
        let growth = if append {
            width as usize * height as usize * 4
        } else {
            0
        };
        if (append && frame_count >= 256)
            || old_len.saturating_add(growth) > 32 * 1024 * 1024
            || self
                .kitty_img
                .used_memory
                .saturating_add(self.inactive_kitty_img.used_memory)
                .saturating_add(growth)
                > 64 * 1024 * 1024
        {
            return Err(AnimationError::Full.into());
        }
        let x = frame.x.unwrap_or(0);
        let y = frame.y.unwrap_or(0);
        let gap_update = frame
            .duration_ms
            .filter(|&n| n != 0)
            .map(|n| Duration::from_millis(n.max(0) as u64));
        let frame_gap = gap_update.unwrap_or(Duration::from_millis(40));
        let base_frame = frame.base_frame.filter(|&n| n != 0);
        anyhow::ensure!(
            !append || base_frame.is_none_or(|n| n <= frame_count),
            "base frame is unavailable"
        );
        let old_gap = self
            .kitty_animation_time
            .and_then(|_| self.kitty_img.playback.get(&image_id)?.frame_gap(&anim));

        if append {
            let mut new_frame = if let Some(base) = base_frame {
                let base = match &*anim {
                    ImageDataType::Rgba8 { data, .. } => data,
                    ImageDataType::AnimRgba8 { frames, .. } => &frames[base as usize - 1],
                    _ => unreachable!("validated animation data"),
                };
                RgbaImage::from_vec(width, height, base.clone()).context("invalid base frame")?
            } else {
                RgbaImage::from_pixel(width, height, background_pixel)
            };
            blit(&mut new_frame, &img, x, y, frame.composition_mode)?;
            let new_data = new_frame.into_vec();
            let new_hash = ImageDataType::hash_bytes(&new_data);
            match &mut *anim {
                ImageDataType::Rgba8 { data, hash, .. } => {
                    let root_gap = self
                        .kitty_img
                        .playback
                        .get(&image_id)
                        .map_or(Duration::ZERO, |playback| playback.root_gap);
                    *anim = ImageDataType::AnimRgba8 {
                        width,
                        height,
                        frames: vec![std::mem::take(data), new_data],
                        durations: vec![root_gap, frame_gap],
                        hashes: vec![*hash, new_hash],
                    };
                }
                ImageDataType::AnimRgba8 {
                    frames,
                    durations,
                    hashes,
                    ..
                } => {
                    frames.push(new_data);
                    durations.push(frame_gap);
                    hashes.push(new_hash);
                }
                _ => unreachable!("validated animation data"),
            }
        } else {
            let index = frame_no as usize - 1;
            let (data, hash, duration) = match &mut *anim {
                ImageDataType::Rgba8 { data, hash, .. } => (data, hash, None),
                ImageDataType::AnimRgba8 {
                    frames,
                    hashes,
                    durations,
                    ..
                } => (
                    &mut frames[index],
                    &mut hashes[index],
                    Some(&mut durations[index]),
                ),
                _ => unreachable!("validated animation data"),
            };
            let mut target =
                ImageBuffer::<Rgba<u8>, _>::from_raw(width, height, data.as_mut_slice())
                    .context("invalid frame to edit")?;
            blit(&mut target, &img, x, y, frame.composition_mode)?;
            *hash = ImageDataType::hash_bytes(data);
            if let (Some(gap), Some(duration)) = (gap_update, duration) {
                *duration = gap;
            }
        }

        if let Some(gap) = gap_update.filter(|_| frame_no == 1) {
            self.kitty_img
                .playback
                .entry(image_id)
                .or_default()
                .root_gap = gap;
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
        self.image_cache.clear();
        Ok(frame_no)
    }

    fn kitty_img_transmit_inner(
        &mut self,
        transmit: KittyImageTransmit,
    ) -> anyhow::Result<(u32, Option<u32>, ImageDataType, Option<IndexedPixels>)> {
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

        let (img, indexed) = self.kitty_decode_image(transmit)?;
        Ok((id, no, img, indexed))
    }

    fn kitty_decode_image(
        &mut self,
        transmit: KittyImageTransmit,
    ) -> anyhow::Result<(ImageDataType, Option<IndexedPixels>)> {
        if let Some(stride) = indexed::stride(transmit.format.as_ref()) {
            let pixels = checked_indexed_pixels(transmit, stride)?;
            let palette = self.palette();
            let img = self.kitty_img.indexed.decode(&pixels, palette);
            return Ok((img, Some(pixels)));
        }

        let (width, height, data) = if transmit.format == Some(KittyImageFormat::Png) {
            let image = read_kitty_png(transmit)?.into_rgba8();
            (image.width(), image.height(), image.into_vec())
        } else {
            let rgb = transmit.format == Some(KittyImageFormat::Rgb);
            let (width, height, data) = read_raw_kitty_pixels(transmit)?;
            let data = if rgb {
                let image = RgbImage::from_vec(width, height, data).context("invalid RGB data")?;
                DynamicImage::ImageRgb8(image).into_rgba8().into_vec()
            } else {
                data
            };
            (width, height, data)
        };
        Ok((ImageDataType::new_single_frame(width, height, data), None))
    }

    fn kitty_img_transmit(
        &mut self,
        transmit: KittyImageTransmit,
        display: Option<KittyImagePlacement>,
        verbosity: KittyImageVerbosity,
    ) -> anyhow::Result<()> {
        let id = transmit.image_id.filter(|&id| id != 0);
        let number = transmit.image_number.filter(|&n| n != 0);
        let placement_id = display
            .as_ref()
            .and_then(|p| p.placement_id)
            .filter(|&p| p != 0);
        let (image_id, image_number, img, mut indexed) =
            match self.kitty_img_transmit_inner(transmit) {
                Err(err) if id.is_some() || number.is_some() => {
                    self.kitty_send_placement_response(
                        verbosity,
                        false,
                        id,
                        number,
                        placement_id,
                        &format!("EINVAL:{err:#}"),
                    );
                    return Ok(());
                }
                result => result?,
            };
        if image_id == 0 && display.as_ref().is_none_or(|p| p.virtual_placement) {
            // Neither a later put nor a Unicode placeholder has an image ID
            // with which to retrieve these bytes.
            return Ok(());
        }

        // An image id owns its mutable animation frames. Equal initial pixels under
        // two ids may subsequently receive different frames and playback commands.
        let img = Arc::new(ImageData::with_data(img));
        let bytes = img.len() + indexed.as_ref().map_or(0, IndexedPixels::len);
        let replacing = self.kitty_img.id_to_data.contains_key(&image_id);
        if self
            .kitty_img
            .check_image_capacity(image_id, bytes, &self.inactive_kitty_img)
            .is_err()
        {
            self.reclaim_missing_kitty_placements(image_id);
            if let Err(err) =
                self.kitty_img
                    .check_image_capacity(image_id, bytes, &self.inactive_kitty_img)
            {
                if image_id != 0 {
                    self.kitty_send_placement_response(
                        verbosity,
                        false,
                        id,
                        number,
                        placement_id,
                        &format!("ENOSPC:{err:#}"),
                    );
                }
                return Ok(());
            }
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
            self.kitty_img.used_memory += bytes;
            self.kitty_img.indexed.insert(&img, indexed.take());
            self.kitty_img.id_to_data.insert(image_id, Arc::clone(&img));
        }
        if let Some(number) = image_number {
            self.kitty_img
                .numbered_images
                .entry(number)
                .or_default()
                .push(image_id);
        }

        if let Some(placement) = display {
            self.kitty_place_data(image_id, image_number, img, indexed, placement, verbosity)?;
        } else if image_id != 0 {
            self.kitty_send_placement_response(
                verbosity,
                true,
                Some(image_id),
                image_number,
                None,
                "OK",
            );
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
            | KittyImage::TransmitFrame { verbosity, .. }
            | KittyImage::Query { verbosity, .. } => {
                *verbosity = transfer_verbosity(*verbosity, final_verbosity);
            }
            _ => unreachable!(),
        }
        Ok(first)
    }
}

fn load_pixels(
    data: KittyImageData,
    compression: KittyImageCompression,
) -> anyhow::Result<Vec<u8>> {
    let data = data.load_data().context("decoding kitty image payload")?;
    match compression {
        KittyImageCompression::None => Ok(data),
        KittyImageCompression::Deflate => {
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&data, 32 * 1024 * 1024)
                .map_err(|err| anyhow::anyhow!("decompressing data: {err:?}"))
        }
    }
}

fn validate_kitty_probe(transmit: KittyImageTransmit) -> anyhow::Result<()> {
    if let Some(stride) = indexed::stride(transmit.format.as_ref()) {
        checked_indexed_pixels(transmit, stride)?;
    } else if transmit.format == Some(KittyImageFormat::Png) {
        read_kitty_png(transmit)?;
    } else {
        read_raw_kitty_pixels(transmit)?;
    }
    Ok(())
}

fn read_raw_kitty_pixels(transmit: KittyImageTransmit) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let width = transmit.width.context("missing kitty image width")?;
    let height = transmit.height.context("missing kitty image height")?;
    check_kitty_rgba_size(width, height)?;
    let channels = if transmit.format == Some(KittyImageFormat::Rgb) {
        3
    } else {
        4
    };
    let expected = width as usize * height as usize * channels;
    let data = load_pixels(transmit.data, transmit.compression)?;
    anyhow::ensure!(
        data.len() == expected,
        "expected {expected} bytes of raw kitty pixels but received {}",
        data.len()
    );
    Ok((width, height, data))
}

fn read_kitty_png(transmit: KittyImageTransmit) -> anyhow::Result<DynamicImage> {
    let data = load_pixels(transmit.data, transmit.compression)?;
    let info = dimensions(&data)?;
    check_kitty_rgba_size(info.width, info.height)?;
    let mut reader =
        image::ImageReader::with_format(std::io::Cursor::new(&data), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(info.width);
    limits.max_image_height = Some(info.height);
    limits.max_alloc = Some(32 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().context("decode png")
}

fn check_kitty_rgba_size(width: u32, height: u32) -> anyhow::Result<()> {
    check_image_dimensions(width, height)?;
    anyhow::ensure!(
        u64::from(width) * u64::from(height) * 4 <= 32 * 1024 * 1024,
        "kitty image exceeds 32 MiB"
    );
    Ok(())
}

fn checked_indexed_pixels(
    transmit: KittyImageTransmit,
    stride: usize,
) -> anyhow::Result<IndexedPixels> {
    let data = load_pixels(transmit.data, transmit.compression)?;
    IndexedPixels::new(transmit.width, transmit.height, stride, data)
}

fn transmission(img: &mut KittyImage) -> Option<&mut KittyImageTransmit> {
    match img {
        KittyImage::TransmitData { transmit, .. }
        | KittyImage::TransmitDataAndDisplay { transmit, .. }
        | KittyImage::TransmitFrame { transmit, .. }
        | KittyImage::Query { transmit, .. } => Some(transmit),
        _ => None,
    }
}

fn transmission_address(image: &KittyImage) -> Option<[(&'static str, Option<u32>); 3]> {
    let (transmit, suffix) = match image {
        KittyImage::TransmitData { transmit, .. } | KittyImage::Query { transmit, .. } => {
            (transmit, ("p", None))
        }
        KittyImage::TransmitDataAndDisplay {
            transmit,
            placement,
            ..
        } => (transmit, ("p", placement.placement_id)),
        KittyImage::TransmitFrame {
            transmit, frame, ..
        } => (transmit, ("r", frame.frame_number)),
        _ => return None,
    };
    let id = transmit.image_id.filter(|&id| id != 0);
    let number = transmit.image_number.filter(|&number| number != 0);
    let (key, extra) = suffix;
    let suffix = (key, extra.filter(|&value| value != 0));
    (id.is_some() || number.is_some()).then_some([("I", number), ("i", id), suffix])
}

fn probe_reply(image: &KittyImage) -> Option<(u32, KittyImageVerbosity)> {
    match image {
        KittyImage::Query {
            transmit,
            verbosity,
        } => Some((transmit.image_id.filter(|&id| id != 0)?, *verbosity)),
        _ => None,
    }
}

fn transfer_verbosity(
    initial: KittyImageVerbosity,
    last: KittyImageVerbosity,
) -> KittyImageVerbosity {
    match last {
        KittyImageVerbosity::Verbose => initial,
        last => last,
    }
}

fn starts_image_upload(image: &KittyImage) -> bool {
    match image {
        KittyImage::TransmitData { transmit, .. } => {
            transmit.image_id.is_some()
                || transmit.image_number.is_some()
                || transmit.width.is_some()
                || transmit.height.is_some()
                || transmit.format.is_some()
                || transmit.compression != KittyImageCompression::None
        }
        KittyImage::TransmitDataAndDisplay { .. } | KittyImage::TransmitFrame { .. } => true,
        _ => false,
    }
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
