use super::*;

const CURRENT: &str = " (current)";

fn choices() -> impl Iterator<Item = (Option<TabColor>, String)> {
    std::iter::once((None, "Use profile tab color".into())).chain(TabColor::ALL.into_iter().map(
        |color| {
            let label = match color {
                TabColor::None => "Hide tab color".into(),
                color => format!("Set tab color to {}", color.label()),
            };
            (Some(color), label)
        },
    ))
}

pub(super) fn menu(available: bool, current: Option<TabColor>) -> MenuItem {
    if !available {
        return MenuItem::disabled("tab-colors", "Tab color");
    }
    MenuItem::submenu(
        "tab-colors",
        "Tab color",
        choices().map(move |(color, label)| {
            let title = if color == current {
                format!("{label}{CURRENT}")
            } else {
                label.clone()
            };
            MenuItem::item(label, title)
        }),
    )
}

pub(super) fn from_menu(label: &str) -> Option<Option<TabColor>> {
    let label = label.strip_suffix(CURRENT).unwrap_or(label);
    choices().find_map(|(color, title)| (title == label).then_some(color))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_distinguishes_an_explicitly_hidden_color_from_the_profile_default() {
        assert_eq!(
            from_menu("Hide tab color (current)"),
            Some(Some(TabColor::None))
        );
        assert_eq!(from_menu("Use profile tab color (current)"), Some(None));
        assert_eq!(from_menu("Set tab color to Red"), Some(Some(TabColor::Red)));
        assert_eq!(from_menu("Rename tab"), None);
    }
}
