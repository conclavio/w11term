use super::*;
use sha2::{Digest, Sha256};
use w11term::sftp::{DotfileBundle, DotfileShell, Operation};

fn bundle(shell: DotfileShell, zdotdir: Option<&str>) -> Operation {
    Operation::PreviewDotfiles {
        bundle: DotfileBundle {
            version: 1,
            environment: Some("export PROJECT=local\r\n".into()),
            bash: Some("PS1='managed> '".into()),
            zsh: Some("PROMPT='managed> '".into()),
        },
        shell,
        zdotdir: zdotdir.map(String::from),
    }
}

fn file<'a>(preview: &'a Value, path: &str) -> &'a Value {
    preview["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|file| file["path"] == path)
        .unwrap_or_else(|| panic!("Missing planned file {path}: {preview}"))
}

fn digest(text: &str) -> String {
    Sha256::digest(text)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_dotfiles_preserves_existing_startup_files_and_reports_hook_drift() {
    let (pane, _, root, opens, server) = fixture(false).await;
    let root = root.path();
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    fs::write(root.join(".bash_profile"), "source ~/.bashrc").unwrap();
    fs::write(root.join(".bashrc"), "# private existing content\n").unwrap();
    fs::write(root.join(".profile"), [0xff]).unwrap();
    let preview = || bundle(DotfileShell::Bash, None);
    let plan = guarded_job(&pane, None, preview()).await.unwrap();
    assert_eq!(plan["home"], "/");
    assert_eq!(plan["shell"], "bash");
    assert_eq!(plan["supports_atomic_replace"], true);
    assert_eq!(
        plan["directories_to_create"],
        json!(["/.config", "/.config/w11term", "/.config/w11term/dotfiles"])
    );
    let environment_path = ".config/w11term/dotfiles/bash-environment.sh";
    let environment = file(&plan, &format!("/{environment_path}"));
    assert_eq!(environment["action"], "create");
    assert_eq!(
        environment["after_sha256"],
        digest("export PROJECT=local\n")
    );
    let login = file(&plan, "/.bash_profile");
    let login_hook = login["hook"].as_str().unwrap();
    assert_eq!(login["action"], "append-hook");
    assert_eq!(
        login["after_sha256"],
        digest(&format!("source ~/.bashrc\n{login_hook}"))
    );
    let rc = file(&plan, "/.bashrc");
    let rc_hook = rc["hook"].as_str().unwrap();
    assert!(rc_hook.contains("case $- in *i*)"));
    assert!(rc_hook.contains(". \"$HOME/.config/w11term/dotfiles/bash-prompt.sh\""));
    assert!(!plan.to_string().contains("private existing content"));
    assert!(!root.join(".config").exists());
    assert_eq!(
        fs::read(root.join(".bash_profile")).unwrap(),
        b"source ~/.bashrc"
    );

    fs::create_dir_all(root.join(".config/w11term/dotfiles")).unwrap();
    fs::write(root.join(environment_path), "export PROJECT=local\n").unwrap();
    let installed = format!("# private existing content\n{rc_hook}");
    fs::write(root.join(".bashrc"), &installed).unwrap();
    let edited = login_hook.replace("bash-prompt.sh", "reviewed-elsewhere.sh");
    fs::write(root.join(".bash_profile"), &edited).unwrap();
    let plan = guarded_job(&pane, None, preview()).await.unwrap();
    assert_eq!(plan["directories_to_create"], json!([]));
    assert_eq!(file(&plan, "/.bashrc")["action"], "unchanged");
    let login = file(&plan, "/.bash_profile");
    assert_eq!(login["action"], "hook-conflict");
    assert!(login["after_sha256"].is_null());
    assert_eq!(
        file(&plan, &format!("/{environment_path}"))["action"],
        "unchanged"
    );
    assert_eq!(
        fs::read(root.join(".bashrc")).unwrap(),
        installed.as_bytes()
    );
    assert_eq!(
        fs::read(root.join(".bash_profile")).unwrap(),
        edited.as_bytes()
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        2,
        "one shell and one reused SFTP channel"
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preview_dotfiles_uses_the_selected_zsh_startup_directory() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::create_dir_all(root.join("config/zsh")).unwrap();
    let plan = guarded_job(&pane, None, bundle(DotfileShell::Zsh, Some("config/zsh")))
        .await
        .unwrap();
    let profile = file(&plan, "/config/zsh/.zprofile")["hook"]
        .as_str()
        .unwrap();
    let rc = file(&plan, "/config/zsh/.zshrc")["hook"].as_str().unwrap();
    assert!(profile.contains("zsh-environment.sh"));
    assert!(!profile.contains("zsh-prompt.sh"));
    assert!(rc.contains("zsh-prompt.sh"));
    assert!(!root.join("config/zsh/.zshrc").exists());

    let error = guarded_job(&pane, None, bundle(DotfileShell::Zsh, Some("../other")))
        .await
        .unwrap_err();
    assert!(error.contains("relative directory"), "{error}");
    fs::write(root.join(".config"), "not a directory").unwrap();
    let error = guarded_job(&pane, None, bundle(DotfileShell::Zsh, None))
        .await
        .unwrap_err();
    assert!(error.contains("Expected a real directory"), "{error}");
    server.abort();
}
