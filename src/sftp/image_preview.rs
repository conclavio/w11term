use super::{text_preview::PreviewLimit, *};
use image::{DynamicImage, ImageDecoder as _, ImageFormat, ImageReader, Limits};
use std::io::Cursor;

const SOURCE: PreviewLimit = PreviewLimit {
    bytes: 8 * 1024 * 1024,
    name: "Image preview",
    limit: "the 8 MiB image preview limit",
};
const MAX_SIDE: u32 = 4096;
const MAX_PIXELS: u64 = 8 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = MAX_PIXELS * 8;
const THUMBNAIL_SIDE: u32 = 1024;

/// Recognize image names in SFTP listings without interpreting remote paths as Windows paths.
pub fn image_preview_name(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, suffix)| {
        ["png", "jpg", "jpeg", "gif"]
            .iter()
            .any(|format| suffix.eq_ignore_ascii_case(format))
    })
}

impl Engine {
    pub(super) async fn read_preview_image(
        &self,
        path: &str,
        cancel: Option<&TransferCancel>,
    ) -> Result<Value> {
        let bytes = self.read_preview_bytes(path, cancel, &SOURCE).await?;
        let cancel = cancel.cloned();
        tokio::task::spawn_blocking(move || {
            let check_cancel = || {
                ensure!(
                    !cancel.as_ref().is_some_and(TransferCancel::is_cancelled),
                    "Image preview cancelled"
                );
                Ok::<_, anyhow::Error>(())
            };
            check_cancel()?;
            let (png, width, height) = preview_png(&bytes)?;
            check_cancel()?;
            Ok(json!({"png": STANDARD.encode(png), "width": width, "height": height}))
        })
        .await?
    }
}

fn preview_png(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let mut reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    ensure!(
        matches!(
            reader.format(),
            Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif)
        ),
        "Choose a PNG, JPEG or GIF image"
    );
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let decoder = reader.into_decoder().context("reading the image header")?;
    let (width, height) = decoder.dimensions();
    ensure!(
        width != 0
            && height != 0
            && width <= MAX_SIDE
            && height <= MAX_SIDE
            && u64::from(width) * u64::from(height) <= MAX_PIXELS
            && decoder.total_bytes() <= MAX_DECODE_BYTES,
        "Image dimensions exceed the preview limit"
    );
    let image = DynamicImage::from_decoder(decoder).context("decoding the selected image")?;
    let thumbnail = if width > THUMBNAIL_SIDE || height > THUMBNAIL_SIDE {
        image.thumbnail(THUMBNAIL_SIDE, THUMBNAIL_SIDE)
    } else {
        image
    };
    let mut png = Cursor::new(Vec::new());
    // Send a bounded bitmap with fresh metadata to the native image control.
    DynamicImage::ImageRgba8(thumbnail.into_rgba8()).write_to(&mut png, ImageFormat::Png)?;
    Ok((png.into_inner(), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(format: ImageFormat, width: u32, height: u32) -> Result<Vec<u8>> {
        let mut data = Cursor::new(Vec::new());
        let pixels = image::RgbImage::from_pixel(width, height, image::Rgb([18, 85, 204]));
        DynamicImage::ImageRgb8(pixels).write_to(&mut data, format)?;
        Ok(data.into_inner())
    }

    #[test]
    fn preview_decodes_only_supported_images_and_produces_a_bounded_png() -> Result<()> {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::Gif] {
            let (png, width, height) = preview_png(&sample(format, 64, 32)?)?;
            assert_eq!(image::guess_format(&png)?, ImageFormat::Png);
            assert_eq!((width, height), (64, 32));
            let rgba = image::load_from_memory(&png)?.into_rgba8();
            assert_eq!(rgba.dimensions(), (64, 32));
            let [red, green, blue, alpha] = rgba.get_pixel(36, 18).0;
            assert!(red.abs_diff(18) <= 3 && green.abs_diff(85) <= 3 && blue.abs_diff(204) <= 3);
            assert_eq!(alpha, 255);
        }
        let (png, width, height) = preview_png(&sample(ImageFormat::Png, 2048, 1024)?)?;
        assert_eq!((width, height), (2048, 1024));
        let downscaled = image::load_from_memory(&png)?.into_rgba8();
        assert_eq!(downscaled.dimensions(), (1024, 512));
        assert!(preview_png(&sample(ImageFormat::Png, MAX_SIDE + 1, 1)?).is_err());
        assert!(preview_png(b"ordinary text").is_err());
        assert!(image_preview_name("map 雪.PNG"));
        assert!(!image_preview_name("map.gif.old"));
        Ok(())
    }
}
