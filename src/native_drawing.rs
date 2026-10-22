//! Small COM ABI adapter between Canvas's 0.100 bindings and Windows's 0.62 bindings.
//! Both project the same Direct2D/DirectWrite interfaces. No Rust objects cross this boundary.
use windows::Win32::Graphics::{
    Direct2D::Common::{
        D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
    },
    Direct2D::{
        D2D1_BITMAP_PROPERTIES1, D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
        D2D1_INTERPOLATION_MODE_LINEAR, ID2D1Bitmap1, ID2D1DeviceContext, ID2D1SolidColorBrush,
    },
    DirectWrite::{
        DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_FEATURE, DWRITE_FONT_FEATURE_TAG,
        DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
        DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES, DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
        DWRITE_TEXT_RANGE, DWriteCreateFactory, IDWriteFactory, IDWriteTextLayout,
        IDWriteTypography,
    },
    Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::core::Interface;
use windows_canvas::{DrawingSession, Rect, TextLayout};
use windows_core_next::Interface as NextInterface;

pub struct NativeDrawing {
    context: ID2D1DeviceContext,
    brush: ID2D1SolidColorBrush,
}
pub struct GpuImage(pub ID2D1Bitmap1);

pub struct Typography(IDWriteTypography);
impl Typography {
    pub fn new(family: &str, enabled: bool) -> windows_canvas::Result<Self> {
        // SAFETY: DirectWrite interfaces are created and used on the renderer's thread.
        unsafe {
            let factory: IDWriteFactory =
                DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).map_err(error)?;
            let typography = factory.CreateTypography().map_err(error)?;
            for name in [
                DWRITE_FONT_FEATURE_TAG_STANDARD_LIGATURES,
                DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_LIGATURES,
                DWRITE_FONT_FEATURE_TAG_CONTEXTUAL_ALTERNATES,
            ] {
                typography
                    .AddFontFeature(DWRITE_FONT_FEATURE {
                        nameTag: name,
                        parameter: u32::from(enabled),
                    })
                    .map_err(error)?;
            }
            if family.to_ascii_lowercase().starts_with("monaspace") {
                // Monaspace's documented programming ligatures live in ss01..ss09:
                // https://github.com/githubnext/monaspace#code-ligatures
                for index in 1..=9 {
                    let tag = u32::from_le_bytes([b's', b's', b'0', b'0' + index]);
                    typography
                        .AddFontFeature(DWRITE_FONT_FEATURE {
                            nameTag: DWRITE_FONT_FEATURE_TAG(tag),
                            parameter: u32::from(enabled),
                        })
                        .map_err(error)?;
                }
            }
            Ok(Self(typography))
        }
    }
    pub fn apply(&self, layout: &TextLayout) -> windows_canvas::Result<()> {
        let raw = layout.raw().as_raw();
        // SAFETY: both bindings project the same COM interface; layout outlives this call.
        let layout = unsafe { IDWriteTextLayout::from_raw_borrowed(&raw).unwrap() };
        unsafe {
            layout
                .SetTypography(
                    &self.0,
                    DWRITE_TEXT_RANGE {
                        startPosition: 0,
                        length: u32::MAX,
                    },
                )
                .map_err(error)
        }
    }
}

impl NativeDrawing {
    pub fn new(ctx: &DrawingSession<'_>) -> windows_canvas::Result<Self> {
        let raw = ctx.raw().as_raw();
        // SAFETY: both versions project ID2D1DeviceContext. Clone AddRefs before raw goes out of scope.
        let context = unsafe { ID2D1DeviceContext::from_raw_borrowed(&raw).unwrap().clone() };
        let brush = unsafe {
            context
                .CreateSolidColorBrush(
                    &D2D1_COLOR_F {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    None,
                )
                .map_err(error)?
        };
        Ok(Self { context, brush })
    }

    pub fn dpi(&self) -> f32 {
        let mut x = 96.0;
        let mut y = 96.0;
        // SAFETY: valid out-parameters on this frame's owning thread.
        unsafe {
            self.context.GetDpi(&mut x, &mut y);
        }
        x
    }

    pub fn set_dpi(&self, dpi: f32) {
        // SAFETY: this offscreen context belongs exclusively to the calling thread.
        unsafe {
            self.context.SetDpi(dpi, dpi);
        }
    }

    pub fn text(
        &self,
        layout: &TextLayout,
        x: f32,
        y: f32,
        rgba: [f32; 4],
        scale_x: f32,
    ) -> windows_canvas::Result<()> {
        let raw = layout.raw().as_raw();
        // SAFETY: borrowed layout remains alive for this synchronous draw call.
        let layout = unsafe { IDWriteTextLayout::from_raw_borrowed(&raw).unwrap() };
        unsafe {
            let mut previous = windows_numerics::Matrix3x2::default();
            self.context.GetTransform(&mut previous);
            let mut transform = previous;
            transform.M11 *= scale_x;
            transform.M12 *= scale_x;
            transform.M31 += x * previous.M11 + y * previous.M21;
            transform.M32 += x * previous.M12 + y * previous.M22;
            self.context.SetTransform(&transform);
            self.brush.SetColor(&D2D1_COLOR_F {
                r: rgba[0],
                g: rgba[1],
                b: rgba[2],
                a: rgba[3],
            });
            self.context.DrawTextLayout(
                windows_numerics::Vector2 { X: 0.0, Y: 0.0 },
                layout,
                &self.brush,
                D2D1_DRAW_TEXT_OPTIONS_ENABLE_COLOR_FONT,
            );
            self.context.SetTransform(&previous);
        }
        Ok(())
    }

    pub fn image(&self, rgba: &[u8], width: u32, height: u32) -> windows_canvas::Result<GpuImage> {
        let expected = width.checked_mul(height).and_then(|n| n.checked_mul(4));
        if width == 0 || height == 0 || expected.map(|n| n as usize) != Some(rgba.len()) {
            return Err(windows_core_next::Error::from_hresult(
                windows_core_next::HRESULT(0x80070057u32 as i32),
            ));
        }
        let mut pixels = Vec::with_capacity(rgba.len());
        for p in rgba.as_chunks::<4>().0 {
            let a = p[3] as u16;
            pixels.extend_from_slice(&[
                (p[2] as u16 * a / 255) as u8,
                (p[1] as u16 * a / 255) as u8,
                (p[0] as u16 * a / 255) as u8,
                p[3],
            ]);
        }
        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: 96.0,
            dpiY: 96.0,
            ..Default::default()
        };
        // SAFETY: tightly packed buffer has width*height*4 bytes and lives through CreateBitmap.
        let bitmap = unsafe {
            self.context
                .CreateBitmap(
                    D2D_SIZE_U { width, height },
                    Some(pixels.as_ptr().cast()),
                    width * 4,
                    &properties,
                )
                .map_err(error)?
        };
        Ok(GpuImage(bitmap))
    }

    pub fn draw_image(&self, image: &GpuImage, source: Rect, destination: Rect) {
        unsafe {
            self.context.DrawBitmap(
                &image.0,
                Some(&rect(destination)),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                Some(&rect(source)),
                None,
            );
        }
    }
}

fn rect(r: Rect) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}
fn error(e: windows::core::Error) -> windows_core_next::Error {
    windows_core_next::Error::from_hresult(windows_core_next::HRESULT(e.code().0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn programming_ligatures_change_directwrite_shaping_without_changing_advance() {
        use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
        let fonts = w11term::appearance::installed_fonts().unwrap();
        eprintln!("Curated installed fonts: {fonts:?}");
        let text = "-> => != === <= >= ffi";
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().unwrap();
        }
        struct Apartment;
        impl Drop for Apartment {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _apartment = Apartment;
        let device = windows_canvas::GpuDevice::new_or_warp().unwrap();
        let target = device.create_render_target(500, 50).unwrap();
        for font in fonts.iter().filter(|font| {
            matches!(
                font.as_str(),
                "Cascadia Code" | "Fira Code" | "JetBrains Mono"
            ) || font.starts_with("Monaspace")
        }) {
            let format = windows_canvas::TextFormat::new(font, 16.0).unwrap();
            let render = |enabled| {
                let layout = TextLayout::new(text, &format, 1000.0, 100.0).unwrap();
                Typography::new(font, enabled)
                    .unwrap()
                    .apply(&layout)
                    .unwrap();
                target
                    .draw(|ctx| {
                        ctx.clear(windows_canvas::ColorF::BLACK);
                        NativeDrawing::new(ctx)?.text(&layout, 0.0, 0.0, [1.0; 4], 1.0)
                    })
                    .unwrap();
                (
                    target.read_pixels().unwrap(),
                    layout.metrics().width_including_trailing_whitespace,
                )
            };
            let (enabled, width_on) = render(true);
            let (disabled, width_off) = render(false);
            assert!(
                enabled != disabled,
                "{font}: enabling programming ligatures did not change glyph rendering"
            );
            assert!(
                (width_on - width_off).abs() < 0.1,
                "{font}: shaping changed the monospaced advance: {width_on} vs {width_off}"
            );
        }
    }
}
