//! Windows app-color preference shared by panes that follow the desktop.
use std::sync::{Arc, Mutex, mpsc};
use tokio::sync::{Notify, watch};
use windows::{
    Foundation::TypedEventHandler,
    UI::{
        Color,
        ViewManagement::{UIColorType, UISettings},
    },
    Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsAppearance {
    Light,
    Dark,
}

impl WindowsAppearance {
    // Microsoft's Win32 dark-mode guidance classifies a light *foreground* as
    // a dark app theme. This describes polarity, including in contrast themes.
    fn from_foreground(color: Color) -> Self {
        if 5 * u16::from(color.G) + 2 * u16::from(color.R) + u16::from(color.B) > 8 * 128 {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

type SharedSource = Mutex<Option<watch::Sender<WindowsAppearance>>>;
static SOURCE: SharedSource = Mutex::new(None);

/// The initial sample is available on return. The last receiver's drop wakes
/// the monitor and revokes its native subscription. A later pane starts afresh.
/// If the Windows API is unavailable, the source defaults to dark.
pub fn subscribe() -> watch::Receiver<WindowsAppearance> {
    let mut source = SOURCE.lock().unwrap();
    if let Some(sender) = source.as_ref() {
        return sender.subscribe();
    }
    let (sender, mut receiver) = watch::channel(WindowsAppearance::Dark);
    let (ready, initialized) = mpsc::sync_channel(0);
    let publisher = sender.clone();
    match std::thread::Builder::new()
        .name("w11term-windows-appearance".into())
        .spawn(move || monitor(&SOURCE, publisher, ready))
    {
        Ok(_) if initialized.recv().is_ok() => *source = Some(sender),
        Ok(_) => log::warn!("Windows app appearance monitor exited before its initial sample"),
        Err(error) => log::warn!("Starting Windows app appearance monitor: {error}"),
    }
    receiver.mark_unchanged();
    receiver
}

struct Apartment;

impl Drop for Apartment {
    fn drop(&mut self) {
        // Matches the successful RoInitialize on this same dedicated thread.
        unsafe { RoUninitialize() }
    }
}

struct Colors {
    settings: UISettings,
    token: Option<i64>,
    _apartment: Apartment,
}

impl Colors {
    fn open(changed: Arc<Notify>) -> windows::core::Result<Self> {
        unsafe { RoInitialize(RO_INIT_MULTITHREADED)? };
        let apartment = Apartment;
        let settings = UISettings::new()?;
        let event = TypedEventHandler::new(move |_, _| {
            changed.notify_one();
            Ok(())
        });
        // Register before the initial read to cover a preference change at startup.
        let token = settings
            .ColorValuesChanged(&event)
            .inspect_err(|error| log::warn!("Listening for Windows app color changes: {error}"))
            .ok();
        Ok(Self {
            settings,
            token,
            _apartment: apartment,
        })
    }

    fn refresh(&self, sender: &watch::Sender<WindowsAppearance>) {
        match self.settings.GetColorValue(UIColorType::Foreground) {
            Ok(color) => publish(sender, color),
            Err(error) => log::warn!("Reading the Windows app foreground color: {error}"),
        }
    }
}

impl Drop for Colors {
    fn drop(&mut self) {
        if let Some(token) = self.token
            && let Err(error) = self.settings.RemoveColorValuesChanged(token)
        {
            log::warn!("Removing the Windows app color subscription: {error}");
        }
    }
}

fn publish(sender: &watch::Sender<WindowsAppearance>, foreground: Color) {
    let next = WindowsAppearance::from_foreground(foreground);
    sender.send_if_modified(|current| {
        let changed = *current != next;
        *current = next;
        changed
    });
}

fn monitor(
    source: &SharedSource,
    sender: watch::Sender<WindowsAppearance>,
    ready: mpsc::SyncSender<()>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            log::warn!("Creating Windows app appearance runtime: {error}");
            return;
        }
    };
    let changed = Arc::new(Notify::new());
    let colors = Colors::open(changed.clone())
        .inspect_err(|error| log::warn!("Opening Windows app appearance settings: {error}"))
        .ok();
    if let Some(colors) = &colors {
        colors.refresh(&sender);
    }
    let _ = ready.send(());
    runtime.block_on(async {
        loop {
            tokio::select! {
                _ = changed.notified() => {
                    if let Some(colors) = &colors {
                        colors.refresh(&sender);
                    }
                }
                _ = sender.closed() => {
                    // A new pane can subscribe while this task is waking up.
                    let mut source = source.lock().unwrap();
                    if sender.is_closed() {
                        *source = None;
                        break;
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color {
            A: 255,
            R: r,
            G: g,
            B: b,
        }
    }

    #[test]
    fn app_theme_uses_weighted_foreground_brightness() {
        use WindowsAppearance::{Dark, Light};
        for (foreground, expected) in [
            (rgb(0, 0, 0), Light),
            (rgb(255, 255, 255), Dark),
            (rgb(128, 128, 128), Light),
            (rgb(128, 128, 129), Dark),
            (rgb(255, 0, 255), Light),
            (rgb(0, 255, 0), Dark),
        ] {
            assert_eq!(WindowsAppearance::from_foreground(foreground), expected);
        }
    }

    #[test]
    fn color_notifications_wake_followers_only_when_app_mode_changes() {
        let (sender, mut first) = watch::channel(WindowsAppearance::Dark);
        let mut other = sender.subscribe();
        publish(&sender, rgb(224, 224, 224));
        assert!(!first.has_changed().unwrap());
        publish(&sender, rgb(20, 20, 20));
        assert!(first.has_changed().unwrap() && other.has_changed().unwrap());
        assert_eq!(*first.borrow_and_update(), WindowsAppearance::Light);
        assert_eq!(*other.borrow_and_update(), WindowsAppearance::Light);
        publish(&sender, rgb(30, 30, 30));
        assert!(!first.has_changed().unwrap() && !other.has_changed().unwrap());
    }

    #[test]
    fn dropping_the_last_subscriber_wakes_and_retires_the_monitor() {
        let (publisher, first) = watch::channel(WindowsAppearance::Dark);
        let source = Mutex::new(Some(publisher.clone()));
        let (ready, initialized) = mpsc::sync_channel(0);
        std::thread::scope(|threads| {
            let monitor = threads.spawn(|| monitor(&source, publisher, ready));
            initialized.recv().unwrap();
            let second = source.lock().unwrap().as_ref().unwrap().subscribe();
            drop(first);
            assert!(!monitor.is_finished());
            drop(second);
            monitor.join().unwrap();
        });
        assert!(source.into_inner().unwrap().is_none());
    }
}
