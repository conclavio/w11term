use crate::terminalstate::image::*;
use crate::TerminalState;
use ::image::imageops::FilterType;
use ::image::ImageFormat;
use log::error;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use std::net::IpAddr;
use std::sync::Arc;
use url::Url;
use wezterm_cell::image::ImageDataType;
use wezterm_escape_parser::osc::ITermFileData;

pub(super) struct DirectoryPeer {
    user: String,
    authority: String,
    host_report_matched: bool,
}

// Only construct an authority from the configured peer. Normalize IP literals
// and DNS spelling without consulting DNS or accepting URL syntax in a hostname.
fn authority(host: &str) -> Option<String> {
    if let Some(addr) = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        return Some(format!("[{}]", addr.parse::<std::net::Ipv6Addr>().ok()?));
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) => return Some(addr.to_string()),
        Ok(IpAddr::V6(addr)) => return Some(format!("[{addr}]")),
        Err(_) => {}
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.split('.').any(|label| {
        label.is_empty()
            || !label
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    }) {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn directory_report(authority: &str, path: &str) -> Option<(Url, Arc<str>)> {
    let components = path.strip_prefix('/')?;
    if components.starts_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || components.split('/').any(|part| matches!(part, "." | ".."))
    {
        return None;
    }
    // Quote POSIX filenames before parsing a file URL. Otherwise a first
    // segment "C|" can become the Windows drive "C:", changing the directory.
    const PATH: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~')
        .remove(b'/');
    let raw: Arc<str> = format!("file://{authority}{}", utf8_percent_encode(path, PATH)).into();
    // Keep the original authority even when Url normalizes localhost away.
    let url = Url::parse(&raw).ok()?;
    Some((url, raw))
}

impl TerminalState {
    /// Accept iTerm2 shell integration's RemoteHost and CurrentDir for this
    /// authenticated SSH user and destination. Reconnects require a new report.
    pub fn enable_iterm_directory_reports(&mut self, user: &str, host: &str) {
        self.iterm_directory_peer = authority(host).map(|authority| DirectoryPeer {
            user: user.to_owned(),
            authority,
            host_report_matched: false,
        });
        if self.current_dir.is_some() {
            self.set_reported_current_dir(None);
        }
    }

    pub(super) fn forget_iterm_reported_host(&mut self) {
        if let Some(peer) = &mut self.iterm_directory_peer {
            peer.host_report_matched = false;
        }
    }

    pub(super) fn iterm_remote_host(&mut self, report: &str) {
        let Some(peer) = &mut self.iterm_directory_peer else {
            return;
        };
        let matched = report.rsplit_once('@').is_some_and(|(user, host)| {
            !user.is_empty()
                && !user.chars().any(char::is_control)
                && user == peer.user
                && authority(host).as_ref() == Some(&peer.authority)
        });
        let keep_directory = matched && peer.host_report_matched;
        peer.host_report_matched = matched;
        if !keep_directory && self.current_dir.is_some() {
            self.set_reported_current_dir(None);
        }
    }

    pub(super) fn iterm_current_dir(&mut self, path: &str) {
        if let Some(peer) = self
            .iterm_directory_peer
            .as_ref()
            .filter(|peer| peer.host_report_matched)
        {
            self.set_reported_current_dir(directory_report(&peer.authority, path));
        }
    }

    pub(crate) fn set_image(&mut self, image: ITermFileData) {
        if !image.inline {
            if let Some(handler) = &self.download_handler {
                handler.save_to_downloads(image.name, image.data);
            } else {
                error!(
                    "Ignoring file download request name={:?} size={}",
                    image.name,
                    image.data.len()
                );
            }
            return;
        }

        let info = match dimensions(&image.data) {
            Ok(dims) => dims,
            Err(e) => {
                error!(
                    "Unable to decode image: {}: size={} {:?}",
                    e,
                    image.data.len(),
                    image
                );
                return;
            }
        };

        if let Err(err) = check_image_dimensions(info.width, info.height) {
            log::error!("{err:#}, {info:#?}");
            return;
        }

        if self.pixel_width == 0 || self.pixel_height == 0 {
            error!(
                "my pixel dimensions are wacky! {}x{}",
                self.pixel_width, self.pixel_height
            );
            return;
        }

        // Figure out the dimensions.
        let physical_cols = self.screen().physical_cols;
        let physical_rows = self.screen().physical_rows;
        let cell_pixel_width = self.pixel_width / physical_cols;
        let cell_pixel_height = self.pixel_height / physical_rows;

        let width = image.width.to_pixels(cell_pixel_width, physical_cols);
        let height = image.height.to_pixels(cell_pixel_height, physical_rows);

        // Compute any Automatic dimensions
        let aspect = info.width as f32 / info.height as f32;

        let (width, height) = match (width, height) {
            (None, None) => {
                // Take the image's native size
                let width = info.width as usize;
                let height = info.height as usize;
                // but ensure that it fits
                if width as usize > self.pixel_width || height as usize > self.pixel_height {
                    let width = width as f32;
                    let height = height as f32;
                    let mut candidates = vec![];

                    let x_scale = self.pixel_width as f32 / width;
                    if height * x_scale <= self.pixel_height as f32 {
                        candidates.push((self.pixel_width, (height * x_scale) as usize));
                    }
                    let y_scale = self.pixel_height as f32 / height;
                    if width * y_scale <= self.pixel_width as f32 {
                        candidates.push(((width * y_scale) as usize, self.pixel_height));
                    }

                    candidates.sort_by(|a, b| (a.0 * a.1).cmp(&(b.0 * b.1)));

                    candidates.pop().unwrap()
                } else {
                    (width, height)
                }
            }
            (Some(w), None) => {
                let h = w as f32 / aspect;
                (w, h as usize)
            }
            (None, Some(h)) => {
                let w = h as f32 * aspect;
                (w as usize, h)
            }
            (Some(w), Some(_)) if image.preserve_aspect_ratio => {
                let h = w as f32 / aspect;
                (w, h as usize)
            }
            (Some(w), Some(h)) => (w, h),
        };

        let downscaled = (width < info.width as usize) || (height < info.height as usize);
        let data = match (downscaled, info.format) {
            (true, ImageFormat::Gif)
            | (true, ImageFormat::Png)
            | (true, ImageFormat::WebP)
            | (false, _) => {
                // Don't resample things that might be animations,
                // or things that don't need resampling
                ImageDataType::EncodedFile(image.data)
            }
            (true, _) => match ::image::load_from_memory(&image.data) {
                Ok(im) => {
                    let im = im.resize_exact(width as u32, height as u32, FilterType::CatmullRom);
                    let data = im.into_rgba8().into_vec();
                    ImageDataType::new_single_frame(width as u32, height as u32, data)
                }
                Err(_) => ImageDataType::EncodedFile(image.data),
            },
        };

        let image_data = match self.raw_image_to_image_data(data) {
            Ok(d) => d,
            Err(err) => {
                log::error!("error processing image data: {err:#}");
                return;
            }
        };

        if let Err(err) = self.assign_image_to_cells(ImageAttachParams {
            image_width: width as u32,
            image_height: height as u32,
            source_width: None,
            source_height: None,
            source_origin_x: 0,
            source_origin_y: 0,
            cell_padding_left: 0,
            cell_padding_top: 0,
            z_index: 0,
            columns: None,
            rows: None,
            data: image_data,
            style: ImageAttachStyle::Iterm,
            image_id: None,
            placement_id: None,
            do_not_move_cursor: image.do_not_move_cursor,
        }) {
            log::error!("set iterm2 image: {:#}", err);
        }
    }
}
