use super::*;

pub(super) const MENU_KEY: &str = "pane-color-scheme";
const CURRENT: &str = " (current)";

pub(super) fn available(live: &LiveTab) -> bool {
    !live.browsing_sessions() && !live.session.input.is_closed()
}

pub(super) fn choices(current: ColorScheme) -> impl Iterator<Item = (ColorScheme, String)> {
    ColorScheme::ALL.into_iter().map(move |scheme| {
        let label = scheme.label();
        let label = if scheme == current {
            format!("{label}{CURRENT}")
        } else {
            label.to_owned()
        };
        (scheme, label)
    })
}

pub(super) fn menu(current: ColorScheme) -> MenuItem {
    MenuItem::submenu(
        MENU_KEY,
        "Color scheme",
        choices(current).map(|(scheme, label)| MenuItem::item(scheme.label(), label)),
    )
}

pub(super) fn from_menu(label: &str) -> Option<ColorScheme> {
    let label = label.strip_suffix(CURRENT).unwrap_or(label);
    ColorScheme::ALL
        .into_iter()
        .find(|scheme| scheme.label() == label)
}

pub(super) fn apply(active: Option<&mut AppTab>, pane: u64, scheme: ColorScheme) -> bool {
    let Some(AppTab {
        page: Page::Terminal(live),
        ..
    }) = active
    else {
        return false;
    };
    if live.session.pane_id != pane || !available(live) || live.color_scheme == scheme {
        return false;
    }
    if live
        .session
        .input
        .send(Command::SetColorScheme(scheme))
        .is_err()
    {
        return false;
    }
    live.color_scheme = scheme;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_and_palette_mark_one_current_scheme_and_keep_it_selectable() {
        for current in ColorScheme::ALL {
            let items: Vec<_> = choices(current).collect();
            assert_eq!(items.len(), ColorScheme::ALL.len());
            for (scheme, label) in items {
                assert_eq!(label.ends_with(CURRENT), scheme == current);
                assert_eq!(from_menu(&label), Some(scheme));
            }
        }
        assert_eq!(from_menu("Other profile"), None);
    }

    #[test]
    fn queued_scheme_changes_require_the_offered_active_pane_and_keep_its_launch() {
        let mut original = live_tab("first".into(), Source::Demo, false);
        let pane = original.pane_id().unwrap();
        let mut neighbor = live_tab("second".into(), Source::Demo, false);
        let scheme = ColorScheme::OneHalfLight;
        assert!(!apply(Some(&mut neighbor), pane, scheme));
        assert!(!apply(None, pane, scheme));
        let Page::Terminal(live) = &mut original.page else {
            unreachable!()
        };
        live.read_only.set(true);
        assert!(apply(Some(&mut original), pane, scheme));
        assert!(!apply(Some(&mut original), pane, scheme));
        let Page::Terminal(live) = &original.page else {
            unreachable!()
        };
        assert_eq!(live.color_scheme, scheme);
        assert_eq!(live.source.color_scheme(), ColorScheme::Default);
        let Page::Terminal(live) = &neighbor.page else {
            unreachable!()
        };
        assert_eq!(live.color_scheme, ColorScheme::Default);
    }

    #[test]
    fn a_stopped_worker_cannot_advertise_or_accept_a_scheme_change() {
        let mut tab = live_tab("stopped".into(), Source::Demo, false);
        let pane = tab.pane_id().unwrap();
        let Page::Terminal(live) = &mut tab.page else {
            unreachable!()
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        drop(receiver);
        live.session.input = sender;
        assert!(!available(live));
        assert!(!apply(Some(&mut tab), pane, ColorScheme::OneHalfDark));
        let Page::Terminal(live) = &tab.page else {
            unreachable!()
        };
        assert_eq!(live.color_scheme, ColorScheme::Default);
    }
}
