//! Shared flame graph geometry, color palettes, and offline SVG export.
use super::{Comparison, Frame, Profile, sample_share as share};
use std::{
    io::{self, BufWriter, Write},
    sync::Arc,
};

pub const ROW_HEIGHT: f32 = 23.0;
pub const MIN_WIDTH: f32 = 1.0;
pub const BACKGROUND: [u8; 3] = [26, 26, 28];
const HIGHLIGHT: [u8; 3] = [237, 163, 247];
const NEUTRAL: [u8; 3] = [185, 181, 194];
const INCREASE: [u8; 3] = [255, 111, 99];
const DECREASE: [u8; 3] = [108, 171, 255];
const PALETTE: [[u8; 3]; 8] = [
    [251, 192, 106],
    [239, 136, 91],
    [218, 115, 105],
    [254, 214, 136],
    [229, 165, 94],
    [218, 129, 117],
    [247, 167, 114],
    [226, 147, 114],
];

#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Range {
    pub zoom: usize,
    pub first_depth: usize,
    pub rows: usize,
}

impl Range {
    pub fn height(self) -> f32 {
        ROW_HEIGHT * self.rows as f32
    }

    pub fn y(self, row: usize) -> f32 {
        ROW_HEIGHT * (self.rows - 1 - row) as f32
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProfileSide {
    Before,
    After,
}

impl ProfileSide {
    pub fn index(self) -> usize {
        self as usize
    }

    pub fn caption(self) -> &'static str {
        match self {
            Self::Before => "Before profile",
            Self::After => "After profile",
        }
    }

    pub fn widths(self) -> &'static str {
        match self {
            Self::Before => "Before widths",
            Self::After => "After widths",
        }
    }
}

#[derive(Clone)]
pub struct Differential {
    pub comparison: Arc<Comparison>,
    pub side: ProfileSide,
}

impl Differential {
    pub fn write_svg(&self, zoom: usize, output: impl Write) -> io::Result<()> {
        let profile = &self.comparison.profiles()[self.side.index()];
        write_graph_svg(profile, zoom, Some(self), output)
    }

    pub fn color(&self, id: usize) -> [u8; 3] {
        let comparison = &self.comparison;
        let Some(change) = comparison.change_at_frame(self.side.index(), id) else {
            return NEUTRAL;
        };
        let delta = comparison.delta_points(change);
        let peak = comparison.delta_points(&comparison.changes()[0]).abs();
        let strength = 0.35 + 0.65 * (delta.abs() / peak).min(1.0);
        let endpoint = if delta > 0.0 { INCREASE } else { DECREASE };
        std::array::from_fn(|channel| {
            let base = f64::from(NEUTRAL[channel]);
            (base + (f64::from(endpoint[channel]) - base) * strength).round() as u8
        })
    }

    pub fn detail(&self, id: usize) -> String {
        let comparison = &self.comparison;
        let Some(change) = comparison.change_at_frame(self.side.index(), id) else {
            return "Exact-stack sample share unchanged".into();
        };
        let [before, after] = change.counts();
        format!(
            "Exact stack · Before {before} · After {after} · {}",
            comparison.delta(change)
        )
    }
}

pub fn extent(frame: &Frame, root: &Frame, width: f32) -> (f32, f32) {
    let x = |sample| ((sample - root.start) as f64 * f64::from(width) / root.samples as f64) as f32;
    (x(frame.start), x(frame.end()))
}

pub fn color(frame: &Frame, matched: bool) -> [u8; 3] {
    if matched {
        return HIGHLIGHT;
    }
    if frame.name.starts_with("[unknown") {
        return [177, 173, 184];
    }
    let hash = frame.name.bytes().fold(0_usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    });
    PALETTE[(hash ^ (hash >> 8)) % PALETTE.len()]
}

/// Write a plain flame graph rooted at the chosen frame.
pub fn write_svg(profile: &Profile, zoom: usize, output: impl Write) -> io::Result<()> {
    write_graph_svg(profile, zoom, None, output)
}

fn write_graph_svg(
    profile: &Profile,
    zoom: usize,
    differential: Option<&Differential>,
    output: impl Write,
) -> io::Result<()> {
    if !profile.has_frame(zoom) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Unknown flame graph frame",
        ));
    }
    let mut writer = BufWriter::new(output);
    const WIDTH: f32 = 1200.0;
    let root = profile.frame(zoom);
    let range = Range {
        zoom,
        first_depth: 0,
        rows: root.last_depth - root.depth + 1,
    };
    let height = range.height();
    writeln!(
        writer,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" viewBox="0 0 {WIDTH} {height}" role="img">"#
    )?;
    let title = if differential.is_some() {
        "Differential CPU flame graph"
    } else {
        "CPU flame graph"
    };
    writeln!(writer, "<title>{title} — {}</title>", xml(&root.name))?;
    if let Some(diff) = differential {
        writeln!(
            writer,
            "<desc>{}. Red increased, blue decreased, gray unchanged. Colors show change in exact-stack sample share.</desc>",
            diff.side.widths()
        )?;
    }
    writeln!(
        writer,
        r##"<rect width="100%" height="100%" fill="#1a1a1c"/><g font-family="Segoe UI,sans-serif" font-size="12" fill="#1a1410">"##
    )?;
    for row in 0..range.rows {
        let y = range.y(row);
        for &id in profile.row(zoom, row) {
            let frame = profile.frame(id);
            let (left, right) = extent(frame, root, WIDTH);
            let width = right - left;
            if width < MIN_WIDTH {
                continue;
            }
            let [r, g, b] = differential.map_or_else(|| color(frame, false), |diff| diff.color(id));
            let name = xml(&frame.name);
            let detail = differential
                .map_or_else(String::new, |diff| format!(" · {}", xml(&diff.detail(id))));
            writeln!(
                writer,
                r#"<g><title>{name} — {} samples ({:.1}% of capture){detail}</title><rect x="{left:.2}" y="{y}" width="{:.2}" height="{}" fill="rgb({r},{g},{b})"/>"#,
                frame.samples,
                share(frame.samples, profile.frame(0).samples),
                width - 0.6,
                ROW_HEIGHT - 1.0
            )?;
            if width >= 24.0 {
                writeln!(
                    writer,
                    r#"<svg x="{:.2}" y="{y}" width="{:.2}" height="{}" overflow="hidden"><text x="0" y="15">{name}</text></svg>"#,
                    left + 4.0,
                    width - 8.0,
                    ROW_HEIGHT - 1.0
                )?;
            }
            writeln!(writer, "</g>")?;
        }
    }
    writeln!(writer, "</g></svg>")?;
    writer.flush()
}

fn xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_uses_the_zoomed_branch_and_escapes_symbols() {
        let profile =
            Profile::parse("process;main;hot 70\nprocess;main 20\nprocess;wait 10").unwrap();
        let main = profile.row(0, 2)[0];
        let mut svg = Vec::new();
        write_svg(&profile, main, &mut svg).unwrap();
        let svg = String::from_utf8(svg).unwrap();
        assert!(svg.contains("hot — 70 samples (70.0% of capture)"));
        assert!(!svg.contains("wait"));
        assert_eq!(xml("Func<A&B>"), "Func&lt;A&amp;B&gt;");
    }

    #[test]
    fn differential_colors_are_normalized_and_keep_the_same_direction_on_both_sides() {
        let before = Arc::new(Profile::parse("root;hot 4\nroot;removed 2\nroot;steady 2").unwrap());
        let after = Arc::new(Profile::parse("root;hot 4\nroot;added 8\nroot;steady 4").unwrap());
        let comparison = Arc::new(Comparison::new([before, after], Default::default()));
        let color = |side: ProfileSide, symbol: &str| {
            let profile = &comparison.profiles()[side.index()];
            let id = profile
                .row(0, 2)
                .iter()
                .copied()
                .find(|&id| profile.frame(id).name == symbol)
                .unwrap();
            Differential {
                comparison: comparison.clone(),
                side,
            }
            .color(id)
        };
        assert_eq!(color(ProfileSide::After, "added"), INCREASE);
        assert_eq!(
            color(ProfileSide::Before, "hot"),
            color(ProfileSide::After, "hot")
        );
        assert_ne!(color(ProfileSide::Before, "removed"), NEUTRAL);
        assert_eq!(color(ProfileSide::Before, "steady"), NEUTRAL);
        assert_eq!(color(ProfileSide::After, "steady"), NEUTRAL);
    }
}
