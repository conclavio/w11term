use super::*;
use std::convert::TryFrom;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Stopped,
    Loading,
    Running,
}

#[derive(Debug, Default)]
pub(super) struct Playback {
    mode: Mode,
    frame: usize,
    loops: u32,
    completed: u32,
    pub(super) root_gap: Duration,
    deadline: Option<Instant>,
}

pub(super) struct Control {
    pub state: Option<u8>,
    pub edit_frame: Option<u32>,
    pub gap: Option<i32>,
    pub current: Option<u32>,
    pub loops: Option<u32>,
}

impl Playback {
    pub(super) fn control(
        &mut self,
        data: &mut ImageDataType,
        edit: Control,
        now: Instant,
    ) -> Result<(), &'static str> {
        let count = match data {
            ImageDataType::AnimRgba8 { frames, .. } => frames.len(),
            _ => 1,
        };
        if !matches!(edit.state, None | Some(0..=3)) {
            return Err("EINVAL");
        }
        if [edit.current, edit.edit_frame]
            .iter()
            .flatten()
            .any(|n| *n as usize > count)
        {
            return Err("ENOENT");
        }
        if let Some(gap) = edit.gap.filter(|&gap| gap != 0) {
            let frame = edit.edit_frame.filter(|&n| n != 0).unwrap_or(1) as usize - 1;
            let duration = Duration::from_millis(gap.max(0) as u64);
            if frame == 0 {
                self.root_gap = duration;
            }
            if let ImageDataType::AnimRgba8 { durations, .. } = data {
                durations[frame] = duration;
            }
        }
        if let Some(frame) = edit.current.filter(|&n| n != 0) {
            self.frame = frame as usize - 1;
        }
        if let Some(loops) = edit.loops.filter(|&n| n != 0) {
            self.loops = loops - 1;
        }
        match edit.state {
            Some(1) => {
                self.mode = Mode::Stopped;
                self.completed = 0;
            }
            Some(2) => self.mode = Mode::Loading,
            Some(3) => self.mode = Mode::Running,
            _ => {}
        }
        self.schedule(data, now);
        Ok(())
    }
    fn schedule(&mut self, data: &ImageDataType, now: Instant) {
        self.deadline = None;
        let ImageDataType::AnimRgba8 { durations, .. } = data else {
            return;
        };
        if self.mode == Mode::Stopped
            || (self.mode == Mode::Loading && self.frame + 1 == durations.len())
        {
            return;
        }
        if durations.iter().all(Duration::is_zero) {
            return;
        }
        self.deadline = Some(now + durations[self.frame]);
    }
    pub(super) fn frame_gap(&self, data: &ImageDataType) -> Option<Duration> {
        match data {
            ImageDataType::AnimRgba8 { durations, .. } => durations.get(self.frame).copied(),
            _ => None,
        }
    }
    pub(super) fn frames_changed(
        &mut self,
        data: &ImageDataType,
        now: Instant,
        old_gap: Option<Duration>,
    ) {
        if let (Some(deadline), Some(gap)) = (self.deadline, old_gap) {
            if self.frame_gap(data) != Some(gap) {
                // Edits take effect at their recorded timestamp. Keep time already spent
                // displaying this frame, without applying a shorter gap retroactively.
                self.schedule(data, deadline - gap);
                self.deadline = self.deadline.map(|next| next.max(now));
            }
        } else if self.deadline.is_none() {
            self.schedule(data, now);
        }
    }
    fn advance(&mut self, data: &ImageDataType, now: Instant, replay: bool) -> bool {
        let ImageDataType::AnimRgba8 { durations, .. } = data else {
            return false;
        };
        let Some(deadline) = self.deadline.filter(|&d| d <= now) else {
            return false;
        };
        let before = self.frame;
        if replay {
            self.advance_replay(durations, deadline, now);
            return before != self.frame;
        }
        for _ in 0..durations.len() {
            self.frame += 1;
            if self.frame == durations.len() {
                self.completed = self.completed.saturating_add(1);
                if self.loops > 0 && self.completed >= self.loops {
                    self.frame -= 1;
                    self.mode = Mode::Stopped;
                    self.completed = 0;
                    break;
                }
                self.frame = 0;
            }
            if !durations[self.frame].is_zero()
                || (self.mode == Mode::Loading && self.frame + 1 == durations.len())
            {
                break;
            }
        }
        self.schedule(data, now);
        before != self.frame
    }

    fn advance_replay(&mut self, durations: &[Duration], mut deadline: Instant, now: Instant) {
        // Finish the current lap from its original deadline, including any gapless frames.
        self.deadline = None;
        while self.frame + 1 < durations.len() {
            self.frame += 1;
            if self.mode == Mode::Loading && self.frame + 1 == durations.len() {
                return;
            }
            deadline += durations[self.frame];
            if deadline > now {
                self.deadline = Some(deadline);
                return;
            }
        }

        let period: Duration = durations.iter().sum();
        if period.is_zero() || self.mode == Mode::Loading {
            return;
        }
        let late = now.duration_since(deadline).as_nanos();
        let wraps = 1 + late / period.as_nanos();
        self.completed = self
            .completed
            .saturating_add(u32::try_from(wraps).unwrap_or(u32::MAX));
        if self.loops != 0 && self.completed >= self.loops {
            self.mode = Mode::Stopped;
            self.completed = 0;
            return;
        }

        // Only the phase within the final lap matters. Frame gaps are signed-32-bit
        // milliseconds on the wire and an image has at most 256 frames.
        let mut phase = Duration::from_nanos((late % period.as_nanos()) as u64);
        for (frame, &gap) in durations.iter().enumerate() {
            if phase < gap {
                self.frame = frame;
                self.deadline = Some(now + (gap - phase));
                return;
            }
            phase -= gap;
        }
    }
}
impl TerminalState {
    /// Use capture time for animation commands on this terminal. The replay driver
    /// calls advance_kitty_animations with the same clock when presentation is due.
    pub fn set_kitty_animation_time(&mut self, now: Option<Instant>) {
        self.kitty_animation_time = now;
    }

    pub fn kitty_animation_frame(&self, id: Option<u32>) -> usize {
        id.and_then(|id| self.kitty_img.playback.get(&id))
            .map_or(0, |p| p.frame)
    }
    pub fn kitty_animation_deadline_for(&self, id: u32) -> Option<Instant> {
        self.kitty_img.playback.get(&id)?.deadline
    }
    pub fn kitty_animation_deadline(&self) -> Option<Instant> {
        self.kitty_img
            .playback
            .values()
            .filter_map(|p| p.deadline)
            .min()
    }
    pub fn advance_kitty_animations(&mut self, now: Instant) -> bool {
        let mut changed = false;
        for (id, playback) in &mut self.kitty_img.playback {
            if playback.deadline.is_some_and(|d| d <= now) {
                if let Some(img) = self.kitty_img.id_to_data.get(id) {
                    changed |=
                        playback.advance(&img.data(), now, self.kitty_animation_time.is_some());
                }
            }
        }
        changed
    }
}
