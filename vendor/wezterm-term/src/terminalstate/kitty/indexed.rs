use crate::color::ColorPalette;
use crate::terminalstate::image::check_image_dimensions;
use anyhow::Context;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use wezterm_cell::image::{ImageData, ImageDataType};
use wezterm_escape_parser::apc::KittyImageFormat;

type Colors = [[u8; 3]; 256];

pub(super) fn stride(format: Option<&KittyImageFormat>) -> Option<usize> {
    match format {
        Some(KittyImageFormat::Indexed) => Some(1),
        Some(KittyImageFormat::IndexedAlpha) => Some(2),
        _ => None,
    }
}

fn colors(palette: &ColorPalette) -> Colors {
    let mut colors = palette.colors.0;
    colors[248..].copy_from_slice(&[
        palette.background,
        palette.foreground,
        palette.selection_bg,
        palette.selection_fg,
        palette.cursor_bg,
        palette.cursor_fg,
        // The renderer has no separate URL-hover ink.
        palette.foreground,
        palette.foreground,
    ]);
    colors.map(|color| {
        let (r, g, b, _) = color.to_srgb_u8();
        [r, g, b]
    })
}

#[derive(Debug)]
pub(super) struct IndexedPixels {
    width: u32,
    height: u32,
    stride: usize,
    bytes: Vec<u8>,
}

impl IndexedPixels {
    pub(super) fn new(
        width: Option<u32>,
        height: Option<u32>,
        stride: usize,
        bytes: Vec<u8>,
    ) -> anyhow::Result<Self> {
        let width = width.context("missing width for indexed kitty image")?;
        let height = height.context("missing height for indexed kitty image")?;
        check_image_dimensions(width, height)?;
        let pixels = u64::from(width) * u64::from(height);
        anyhow::ensure!(
            pixels * (4 + stride as u64) <= 32 * 1024 * 1024,
            "indexed kitty image exceeds 32 MiB"
        );
        anyhow::ensure!(
            bytes.len() == pixels as usize * stride,
            "indexed kitty image data length does not match its dimensions"
        );
        Ok(Self {
            width,
            height,
            stride,
            bytes,
        })
    }

    pub(super) fn len(&self) -> usize {
        self.bytes.len()
    }

    fn render(&self, rgba: &mut [u8], colors: &Colors) {
        for (source, dest) in self
            .bytes
            .chunks_exact(self.stride)
            .zip(rgba.chunks_exact_mut(4))
        {
            dest[..3].copy_from_slice(&colors[usize::from(source[0])]);
            dest[3] = source.get(1).copied().unwrap_or(255);
        }
    }
}

#[derive(Debug)]
struct StoredPixels {
    image: Weak<ImageData>,
    pixels: IndexedPixels,
}

#[derive(Debug)]
struct Palette {
    source: ColorPalette,
    colors: Colors,
}

#[derive(Debug, Default)]
pub(super) struct IndexedImages {
    pixels: HashMap<usize, StoredPixels>,
    palette: Mutex<Option<Box<Palette>>>,
}

impl IndexedImages {
    pub(super) fn decode(&mut self, pixels: &IndexedPixels, source: ColorPalette) -> ImageDataType {
        // New uploads use the last materialized palette. All live images then
        // advance to the capture's palette together, even if output changed it.
        let palette = self.palette.get_mut().unwrap().get_or_insert_with(|| {
            Box::new(Palette {
                colors: colors(&source),
                source,
            })
        });
        let mut rgba = vec![0; pixels.width as usize * pixels.height as usize * 4];
        pixels.render(&mut rgba, &palette.colors);
        ImageDataType::new_single_frame(pixels.width, pixels.height, rgba)
    }

    pub(super) fn insert(&mut self, image: &Arc<ImageData>, pixels: Option<IndexedPixels>) {
        if let Some(pixels) = pixels {
            self.pixels.insert(
                Arc::as_ptr(image) as usize,
                StoredPixels {
                    image: Arc::downgrade(image),
                    pixels,
                },
            );
        }
    }

    pub(super) fn remove(&mut self, image: &Arc<ImageData>) -> usize {
        self.pixels
            .remove(&(Arc::as_ptr(image) as usize))
            .map_or(0, |stored| stored.pixels.len())
    }

    pub(super) fn len_for(&self, image: &Arc<ImageData>) -> usize {
        self.pixels
            .get(&(Arc::as_ptr(image) as usize))
            .map_or(0, |stored| stored.pixels.len())
    }

    pub(super) fn recolor(&self, source: &ColorPalette) {
        if self.pixels.is_empty() {
            return;
        }
        let mut cached = self.palette.lock().unwrap();
        if cached
            .as_ref()
            .is_some_and(|palette| palette.source == *source)
        {
            return;
        }
        let new_colors = colors(source);
        if cached
            .as_ref()
            .is_none_or(|palette| palette.colors != new_colors)
        {
            for stored in self.pixels.values() {
                let Some(image) = stored.image.upgrade() else {
                    continue;
                };
                let mut image = image.data();
                if let ImageDataType::Rgba8 { data, hash, .. } = &mut *image {
                    stored.pixels.render(data, &new_colors);
                    *hash = ImageDataType::hash_bytes(data);
                }
            }
        }
        *cached = Some(Box::new(Palette {
            source: source.clone(),
            colors: new_colors,
        }));
    }
}
