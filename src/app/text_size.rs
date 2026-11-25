use super::UiWork;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::SyncSender,
};
use windows::{Foundation::TypedEventHandler, UI::ViewManagement::UISettings};

pub(super) struct Monitor {
    settings: UISettings,
    token: Option<i64>,
    changed: Arc<AtomicBool>,
}

impl Monitor {
    pub(super) fn open(wake: SyncSender<UiWork>) -> windows::core::Result<Self> {
        let settings = UISettings::new()?;
        let changed = Arc::new(AtomicBool::new(false));
        let dirty = changed.clone();
        let handler = TypedEventHandler::new(move |_, _| {
            if !dirty.swap(true, Ordering::AcqRel) {
                // A full queue will already wake the UI, which also checks the flag.
                let _ = wake.try_send(UiWork::TextSizeChanged);
            }
            Ok(())
        });
        let token = settings
            .TextScaleFactorChanged(&handler)
            .inspect_err(|error| log::warn!("Listening for Windows text size changes: {error}"))
            .ok();
        Ok(Self {
            settings,
            token,
            changed,
        })
    }

    pub(super) fn current(&self) -> Option<f64> {
        self.settings
            .TextScaleFactor()
            .inspect_err(|error| log::warn!("Reading Windows text size: {error}"))
            .ok()
    }

    pub(super) fn take_change(&self) -> Option<f64> {
        if self.changed.swap(false, Ordering::AcqRel) {
            self.current()
        } else {
            None
        }
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        if let Some(token) = self.token
            && let Err(error) = self.settings.RemoveTextScaleFactorChanged(token)
        {
            log::warn!("Removing the Windows text size subscription: {error}");
        }
    }
}
