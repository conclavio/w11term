use super::*;
use sha2::{Digest, Sha256};
use w11term::sftp::{DotfileBundle, DotfileShell, Operation};

fn bundle(shell: DotfileShell, zdotdir: Option<&str>) -> Operation {
    Operation::PreviewDotfiles {
        bundle: manifest("managed"),
        shell,
        zdotdir: zdotdir.map(String::from),
    }
}

fn manifest(prompt: &str) -> DotfileBundle {
    DotfileBundle {
        version: 1,
        environment: Some("export PROJECT=local\r\n".into()),
        bash: Some(format!("PS1='{prompt}> '")),
        zsh: Some(format!("PROMPT='{prompt}> '")),
    }
}

fn apply(prompt: &str, plan: &Value) -> Operation {
    Operation::ApplyDotfiles {
        bundle: manifest(prompt),
        shell: DotfileShell::Bash,
        zdotdir: None,
        review: plan["review"].as_str().unwrap().to_owned(),
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
    let id = plan["bundle_sha256"].as_str().unwrap();
    let version_dir = format!(".config/w11term/dotfiles/bash-{id}");
    assert_eq!(
        plan["directories_to_create"],
        json!([
            "/.config",
            "/.config/w11term",
            "/.config/w11term/dotfiles",
            format!("/{version_dir}")
        ])
    );
    let environment_path = format!("{version_dir}/environment.sh");
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
    assert!(rc_hook.contains("/prompt.sh\""));
    assert!(rc_hook.contains("bash-current"));
    assert!(!plan.to_string().contains("private existing content"));
    assert!(!root.join(".config").exists());
    assert_eq!(
        fs::read(root.join(".bash_profile")).unwrap(),
        b"source ~/.bashrc"
    );

    fs::create_dir_all(root.join(version_dir)).unwrap();
    fs::write(root.join(&environment_path), "export PROJECT=local\n").unwrap();
    let installed = format!("# private existing content\n{rc_hook}");
    fs::write(root.join(".bashrc"), &installed).unwrap();
    let edited = login_hook.replace("/prompt.sh", "/reviewed-elsewhere.sh");
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
    assert!(profile.contains("zsh-current"));
    assert!(profile.contains("/environment.sh"));
    assert!(!profile.contains("/prompt.sh"));
    assert!(rc.contains("/prompt.sh"));
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
    fs::remove_file(root.join(".config")).unwrap();
    let installed = guarded_job(
        &pane,
        None,
        Operation::ApplyDotfiles {
            bundle: manifest("managed"),
            shell: DotfileShell::Zsh,
            zdotdir: Some("config/zsh".into()),
            review: plan["review"].as_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    let id = installed["bundle_sha256"].as_str().unwrap();
    let managed = root.join(".config/w11term/dotfiles");
    assert_eq!(
        fs::read_to_string(managed.join("zsh-current")).unwrap(),
        format!("{id}\n")
    );
    assert_eq!(
        fs::read_to_string(root.join("config/zsh/.zprofile")).unwrap(),
        profile
    );
    assert_eq!(
        fs::read_to_string(root.join("config/zsh/.zshrc")).unwrap(),
        rc
    );
    assert!(!root.join(".zshrc").exists());
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dotfiles_installation_rechecks_the_review_and_publishes_one_active_version() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let managed = root.join(".config/w11term/dotfiles");
    let selector = managed.join("bash-current");
    let startup = root.join(".bashrc");
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    fs::write(&startup, "# original Bash configuration\n").unwrap();
    let preview = |prompt| Operation::PreviewDotfiles {
        bundle: manifest(prompt),
        shell: DotfileShell::Bash,
        zdotdir: None,
    };
    let stale = guarded_job(&pane, None, preview("first")).await.unwrap();
    fs::write(&startup, "# corrected Bash configuration\n").unwrap();
    let error = guarded_job(&pane, None, apply("first", &stale))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    assert!(!managed.exists());
    let first = guarded_job(&pane, None, preview("first")).await.unwrap();
    let error = guarded_job(&pane, None, apply("different bundle", &first))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    assert!(!managed.exists());

    let result = guarded_job(&pane, None, apply("first", &first))
        .await
        .unwrap();
    let id = result["bundle_sha256"].as_str().unwrap();
    let version = managed.join(format!("bash-{id}"));
    assert_eq!(id, first["bundle_sha256"]);
    assert_eq!(
        result["changed_files"].as_array().unwrap().last().unwrap(),
        "/.config/w11term/dotfiles/bash-current"
    );
    assert_eq!(fs::read_to_string(&selector).unwrap(), format!("{id}\n"));
    assert_eq!(
        fs::read_to_string(version.join("environment.sh")).unwrap(),
        "export PROJECT=local\n"
    );
    assert_eq!(
        fs::read_to_string(version.join("prompt.sh")).unwrap(),
        "PS1='first> '\n"
    );
    let installed = fs::read_to_string(&startup).unwrap();
    assert!(installed.starts_with("# corrected Bash configuration\n"));
    assert_eq!(
        fs::read_to_string(root.join(".profile")).unwrap(),
        file(&first, "/.profile")["hook"].as_str().unwrap()
    );

    let no_changes = guarded_job(&pane, None, preview("first")).await.unwrap();
    let checked = guarded_job(&pane, None, apply("first", &no_changes))
        .await
        .unwrap();
    assert_eq!(checked["changed_files"], json!([]));

    let second = guarded_job(&pane, None, preview("second")).await.unwrap();
    fs::write(root.join("deny-editor-rename"), []).unwrap();
    let error = guarded_job(&pane, None, apply("second", &second))
        .await
        .unwrap_err();
    assert!(error.contains("bash-current"), "{error}");
    assert_eq!(fs::read_to_string(&selector).unwrap(), format!("{id}\n"));
    assert_eq!(fs::read_to_string(&startup).unwrap(), installed);
    fs::remove_file(root.join("deny-editor-rename")).unwrap();
    let second = guarded_job(&pane, None, preview("second")).await.unwrap();
    let result = guarded_job(&pane, None, apply("second", &second))
        .await
        .unwrap();
    let new_id = result["bundle_sha256"].as_str().unwrap();
    assert_ne!(id, new_id);
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{new_id}\n")
    );
    assert_eq!(fs::read_to_string(&startup).unwrap(), installed);
    assert_eq!(
        fs::read_to_string(version.join("prompt.sh")).unwrap(),
        "PS1='first> '\n"
    );
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dotfiles_review_cannot_be_used_on_another_ssh_pane() {
    let (first, _, _, _, first_server) = fixture(false).await;
    let (second, _, root, _, second_server) = fixture(false).await;
    let plan = guarded_job(&first, None, bundle(DotfileShell::Bash, None))
        .await
        .unwrap();
    let error = guarded_job(&second, None, apply("managed", &plan))
        .await
        .unwrap_err();
    assert!(error.contains("another SFTP connection"), "{error}");
    assert!(!root.path().join(".config").exists());
    let existing = root.path().join(".bashrc");
    fs::write(&existing, "# existing rc\n").unwrap();
    let local_review = guarded_job(&second, None, bundle(DotfileShell::Bash, None))
        .await
        .unwrap();
    let error = guarded_job(&second, None, apply("managed", &local_review))
        .await
        .unwrap_err();
    assert!(
        error.contains("cannot atomically replace /.bashrc"),
        "{error}"
    );
    assert_eq!(fs::read(existing).unwrap(), b"# existing rc\n");
    assert!(!root.path().join(".config").exists());
    first_server.abort();
    second_server.abort();
}
