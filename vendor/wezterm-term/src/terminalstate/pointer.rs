use super::*;

// Keep protocol names and X11 aliases together. Cursor families may share a
// Windows resource, while queries return the requested CSS name.
macro_rules! pointer_shapes {
    ($($shape:ident => $name:literal $(| $alias:literal)*),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub enum PointerShape { $($shape),+ }

        impl PointerShape {
            pub fn name(self) -> &'static str {
                match self { $(Self::$shape => $name),+ }
            }

            fn parse(name: &str) -> Option<Self> {
                match name {
                    $($name $(| $alias)* => Some(Self::$shape),)+
                    _ => None,
                }
            }
        }
    };
}

pointer_shapes! {
    Default => "default" | "arrow" | "left_ptr",
    Text => "text" | "xterm" | "ibeam",
    Pointer => "pointer" | "hand" | "hand2" | "pointing_hand",
    Help => "help" | "question_arrow",
    Wait => "wait" | "watch" | "clock",
    Progress => "progress" | "left_ptr_watch",
    Crosshair => "crosshair" | "tcross",
    Cell => "cell" | "cross" | "plus",
    Move => "move" | "fleur",
    East => "e-resize" | "right_side",
    West => "w-resize" | "left_side",
    EastWest => "ew-resize" | "sb_h_double_arrow",
    North => "n-resize" | "top_side",
    South => "s-resize" | "bottom_side",
    NorthSouth => "ns-resize" | "sb_v_double_arrow",
    Northeast => "ne-resize" | "top_right_corner",
    Southwest => "sw-resize" | "bottom_left_corner",
    NortheastSouthwest => "nesw-resize",
    Northwest => "nw-resize" | "top_left_corner",
    Southeast => "se-resize" | "bottom_right_corner",
    NorthwestSoutheast => "nwse-resize",
    NotAllowed => "not-allowed" | "crossed_circle",
    NoDrop => "no-drop" | "dnd-no-drop",
    Grab => "grab" | "openhand",
    Grabbing => "grabbing" | "closedhand",
}

const DEPTH: usize = 16;
pub(super) type Stack = VecDeque<Option<PointerShape>>;

impl TerminalState {
    pub fn application_pointer_shape(&self) -> Option<PointerShape> {
        self.pointer_shapes[usize::from(self.is_alt_screen_active())]
            .back()
            .copied()
            .flatten()
    }

    pub(super) fn perform_pointer_shape(&mut self, body: &[u8]) {
        let Ok(body) = std::str::from_utf8(body) else {
            return;
        };
        if let Some(names) = body.strip_prefix('?') {
            let current = self.application_pointer_shape();
            let response = names
                .split(',')
                .map(|name| match name {
                    "__current__" => current.map_or("0", PointerShape::name),
                    "__default__" | "__grabbed__" => PointerShape::Default.name(),
                    known if PointerShape::parse(known).is_some() => "1",
                    _ => "0",
                })
                .collect::<Vec<_>>()
                .join(",");
            write!(self.writer, "\x1b]22;{response}\x1b\\").ok();
            self.writer.flush().ok();
            return;
        }
        let alternate = usize::from(self.is_alt_screen_active());
        let stack = &mut self.pointer_shapes[alternate];
        if body.starts_with('<') {
            stack.pop_back();
        } else if let Some(names) = body.strip_prefix('>') {
            for shape in names.split(',').filter_map(PointerShape::parse) {
                if stack.len() == DEPTH {
                    stack.pop_front();
                }
                stack.push_back(Some(shape));
            }
        } else {
            for name in body.strip_prefix('=').unwrap_or(body).split(',') {
                let shape = PointerShape::parse(name);
                if shape.is_some() || name.is_empty() {
                    if let Some(top) = stack.back_mut() {
                        *top = shape;
                    } else {
                        stack.push_back(shape);
                    }
                }
            }
        }
    }
}
