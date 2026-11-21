use super::*;
use std::fs;
use tempfile::TempDir;

struct Files(TempDir);
impl Files {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn home(&self) -> &Path {
        self.0.path()
    }
    fn put(&self, name: &str, data: impl AsRef<[u8]>) -> PathBuf {
        let path = self.home().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }
    fn report(&self, text: &str) -> Result<ImportReport> {
        load(&self.put("chosen/config", text), self.home(), "local")
    }
}

#[test]
fn sorted_fragments_restore_global_scope_and_resolve_relative_to_ssh_home() {
    let files = Files::new();
    files.put(
        ".ssh/parts/20-prod.conf",
        "Host pilot\n HostName late.invalid\nHost beta\n",
    );
    files.put(
        ".ssh/parts/10-prod.conf",
        "\u{feff}Include nested.conf\nHost pilot\n Port 777\n",
    );
    files.put(".ssh/parts/.hidden.conf", "StrictHostKeyChecking no\n");
    files.put("chosen/nested.conf", "Host wrong-alias\n");
    files.put(
        ".ssh/nested.conf",
        "Host pilot\n HostName correct.example\n",
    );
    let extra = files.put("other dir/é-extra", "Host extra\n User extra-user\n");
    files.put("tail", "Host pilot\n Port 123\nHost final\n");
    let report = files
        .report(&format!(
            "Include \"parts/??-prod.conf\" \"{}\" ~/tail\nUser inherited\nHost from-root\n",
            extra.display()
        ))
        .unwrap();
    let aliases: Vec<_> = report.aliases.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(aliases, ["beta", "extra", "final", "from-root", "pilot"]);
    for a in report.aliases {
        let options = a.options.unwrap();
        assert_eq!(
            options.user,
            if a.name == "extra" {
                "extra-user"
            } else {
                "inherited"
            }
        );
        if a.name == "pilot" {
            assert_eq!(
                (options.host.as_str(), options.port),
                ("correct.example", 777)
            );
        }
    }
}

#[test]
fn unsupported_scope_and_trust_block_only_the_relevant_alias_with_source_line() {
    let files = Files::new();
    files.put(".ssh/fragment config","Host bad\n# comment\n HostKeyAlias unimportable\nHost nested\n Include corrupted\nHost port\n Port 0\nHost ok\n");
    files.put(".ssh/corrupted", [0xff]);
    let report = files.report("Include = \"fragment config\"\n").unwrap();
    for (alias, line) in [("bad", 3), ("nested", 5), ("port", 7)] {
        let found = report.aliases.iter().find(|a| a.name == alias).unwrap();
        assert!(found.options.is_none());
        assert!(
            found
                .warnings
                .iter()
                .any(|s| s.contains(&format!("fragment config:{line}"))),
            "{:?}",
            found.warnings
        );
    }
    assert!(
        report
            .aliases
            .iter()
            .find(|a| a.name == "ok")
            .unwrap()
            .options
            .is_some()
    );
    let report = files
        .report("Host one\nMatch exec this-never-runs\n Include corrupted")
        .unwrap();
    assert!(report.aliases[0].options.is_none());
}

#[test]
fn missing_fragments_and_repeat_visits_are_distinct_from_failed_imports() {
    let files = Files::new();
    files.put(".ssh/common", "Host shared\nUser shared\n");
    let report = files
        .report("Include missing missing/*.conf common common\nHost just-root")
        .unwrap();
    assert_eq!(report.aliases.len(), 2);
    assert!(report.aliases.iter().all(|a| a.options.is_some()));
    files.put(".ssh/bad.conf", [0xff]);
    let err = files.report("# first line\nInclude *.conf").unwrap_err();
    let err = format!("{err:#}");
    assert!(
        err.contains("config:2") && err.contains("bad.conf") && err.contains("UTF-8"),
        "{err}"
    );
    files.put(".ssh/bad.conf", "Host bad\n  User \"open quote\n");
    let err = format!("{:#}", files.report("Include bad.conf").unwrap_err());
    assert!(
        err.contains("bad.conf:2") && err.contains("unterminated"),
        "{err}"
    );
}

#[test]
fn filename_globs_match_unicode_scalars_and_respect_leading_dots_and_scan_limits() {
    let files = Files::new();
    files.put(".ssh/🐱.conf", "Host cat\n");
    files.put(".ssh/.private.conf", "Host hidden\n");
    files.put(".ssh/long.conf", "Host ordinary\n");
    let report = files.report("Include ?.conf").unwrap();
    assert_eq!(report.aliases.len(), 1);
    assert_eq!(report.aliases[0].name, "cat");
    let report = files.report("Include *.conf").unwrap();
    assert_eq!(report.aliases.len(), 2);
    let report = files.report("Include .*.conf").unwrap();
    assert_eq!(report.aliases[0].name, "hidden");
    let pattern = pattern("*.conf", files.home()).unwrap();
    let mut reader = Reader::new(files.home());
    reader.entries = MAX_DIRECTORY_ENTRIES;
    assert!(
        reader
            .include(&pattern)
            .unwrap_err()
            .to_string()
            .contains("directory entries")
    );
    let err = files
        .report(&format!("Include {}", "missing ".repeat(MAX_PATTERNS + 1)))
        .unwrap_err();
    assert!(err.to_string().contains("Include patterns"));
}

#[test]
fn only_unambiguous_local_filenames_are_supported() {
    let home = Path::new(r"C:\Users\tester");
    for name in [
        r"\\fileserver\config",
        "//server/share/file",
        r"\\?\C:\x",
        r"\\.\pipe\p",
        r"\SystemRoot\x",
        "C:relative",
        "file.conf:other",
        "~/C:/other",
        "~//foo",
        "~another/x",
        "%h",
        "${HOME}/key",
        "[ab].conf",
        "{a,b}.conf",
        "dir*/one",
        "CONIN$.log",
        "com¹.cfg",
        "AUX .txt",
        "x.",
    ] {
        assert!(pattern(name, home).is_err(), "{name}");
    }
    assert!(pattern("~/.ssh/🐱 ?.conf", home).is_ok());
    let files = Files::new();
    let report = files
        .report("Include ${USERPROFILE}/config\nHost simple\n")
        .unwrap();
    assert!(report.aliases[0].options.is_none());
    assert!(report.warnings.iter().any(|w| w.contains("config:1")));
}

#[test]
fn bounds_apply_across_fragments_and_cycles_use_actual_file_identity() {
    let files = Files::new();
    let target = files.put(".ssh/cycle", "Include other-name");
    fs::hard_link(target, files.home().join(".ssh/other-name")).unwrap();
    let error = format!("{:#}", files.report("Include cycle").unwrap_err());
    assert!(
        error.contains("recursive Include") && error.contains("cycle:1"),
        "{error}"
    );
    for i in 0..MAX_DEPTH {
        files.put(&format!(".ssh/{i}"), format!("Include {}", i + 1));
    }
    files.put(&format!(".ssh/{MAX_DEPTH}"), "Host deep");
    assert!(format!("{:#}", files.report("Include 0").unwrap_err()).contains("depth"));
    files.put(".ssh/tiny", "Host tiny");
    assert!(
        format!(
            "{:#}",
            files
                .report(&"Include tiny\n".repeat(MAX_FILES))
                .unwrap_err()
        )
        .contains("64 files")
    );
    files.put(".ssh/large", format!("#{}", "a".repeat(MAX_BYTES / 2)));
    assert!(format!("{:#}", files.report("Include large large").unwrap_err()).contains("1 MiB"));
    files.put(".ssh/lines", "\n".repeat(MAX_LINES / 2));
    assert!(
        format!("{:#}", files.report("Include lines lines").unwrap_err()).contains("16384 lines")
    );
}

#[test]
fn a_directory_junction_is_never_traversed_by_a_literal_or_glob() {
    use std::process::Command;
    let files = Files::new();
    files.put("outside/host.conf", "Host explicit-selection");
    fs::create_dir_all(files.home().join(".ssh")).unwrap();
    // All arguments are fixed fixture names, relative to the private temporary root.
    assert!(
        Command::new("cmd.exe")
            .current_dir(files.home())
            .args(["/d", "/c", "mklink", "/J", r".ssh\junction", "outside"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let picked = load(
        &files.home().join(".ssh/junction/host.conf"),
        files.home(),
        "local",
    )
    .unwrap();
    assert_eq!(picked.aliases[0].name, "explicit-selection");
    for name in ["junction/host.conf", "junction/*.conf"] {
        let result = files.report(&format!("Include {name}"));
        assert!(result.is_err(), "{name}: {result:?}");
    }
}
