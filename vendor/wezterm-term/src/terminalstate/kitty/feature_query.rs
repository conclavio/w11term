use std::fmt::Write;
use wezterm_escape_parser::apc::KittyImageVerbosity;

pub(super) fn response(
    image_id: u32,
    action: char,
    options: &[(&str, String)],
    verbosity: KittyImageVerbosity,
) -> Option<String> {
    if image_id == 0 || !matches!(verbosity, KittyImageVerbosity::Verbose) {
        return None;
    }
    let mut response = format!("\x1b_Gi={image_id}");
    if matches!(action, 't' | 'T' | 'q' | 'p' | 'd' | 'f' | 'a' | 'c') {
        let _ = write!(response, ",e={action}");
        let delete = options
            .iter()
            .find(|(key, _)| *key == "d")
            .map(|(_, value)| value.as_str());
        for (key, value) in options {
            if supports(action, key, value, delete) {
                let _ = write!(response, ",{key}={value}");
            }
        }
    }
    response.push_str(";OK\x1b\\");
    Some(response)
}

fn supports(action: char, key: &str, value: &str, delete: Option<&str>) -> bool {
    // Requests can test options individually before they have an image to
    // attach. Accept valid non-default scalars for each action.
    let uint = || value.parse::<u32>().is_ok_and(|n| n != 0);
    let signed = || value.parse::<i32>().is_ok_and(|n| n != 0);
    let frame = || value.parse::<u32>().is_ok_and(|n| (1..=256).contains(&n));
    match (action, key) {
        ('t' | 'T' | 'q' | 'f', "t") => value == "d",
        ('t' | 'T' | 'q' | 'f', "o") => value == "z",
        ('t' | 'T' | 'q' | 'f', "f") => matches!(value, "24" | "32" | "100"),
        ('t' | 'T' | 'q' | 'f', "s" | "v") => value
            .parse::<u32>()
            .is_ok_and(|n| (1..=32 * 1024 * 1024 / 4).contains(&n)),
        ('T' | 'p', "x" | "y" | "w" | "h" | "X" | "Y" | "c" | "r" | "p" | "P" | "Q") => uint(),
        ('T' | 'p', "H" | "V" | "z") => signed(),
        ('T' | 'p', "C" | "U") => value == "1",
        ('d', "d") => matches!(
            value,
            "a" | "A"
                | "i"
                | "I"
                | "n"
                | "N"
                | "c"
                | "C"
                | "f"
                | "F"
                | "p"
                | "P"
                | "q"
                | "Q"
                | "r"
                | "R"
                | "x"
                | "X"
                | "y"
                | "Y"
                | "z"
                | "Z"
        ),
        ('d', "x" | "y" | "p") => uint(),
        ('d', "z") => signed(),
        ('d', "r") => matches!(delete, Some("f" | "F")) && uint(),
        ('f', "x" | "y" | "Y") | ('a', "v") | ('c', "x" | "y" | "w" | "h" | "X" | "Y") => uint(),
        ('a' | 'f' | 'c', "c" | "r") => frame(),
        ('a' | 'f', "z") => signed(),
        ('f', "X") | ('c', "C") => value == "1",
        ('a', "s") => matches!(value, "1" | "2" | "3"),
        // Local file and shared-memory transports, indexed pixels, and usage
        // hints are unavailable. Do not echo their parameters.
        _ => false,
    }
}
