use super::*;
use sha2::{Digest, Sha256};
use w11term::{
    options::DotfileSync,
    sftp::{DotfileBundle, DotfileShell, DotfileTarget, Operation},
};

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

fn switch(id: Option<&str>, preview: Option<&Value>) -> Operation {
    dotfile_switch(
        id.map_or(DotfileTarget::Disabled, |id| DotfileTarget::Bundle {
            sha256: id.into(),
        }),
        preview,
    )
}

fn dotfile_switch(target: DotfileTarget, preview: Option<&Value>) -> Operation {
    Operation::SwitchDotfiles {
        target,
        shell: DotfileShell::Bash,
        zdotdir: None,
        review: preview.map(|plan| plan["review"].as_str().unwrap().into()),
    }
}

fn repair_hooks(shell: DotfileShell, zdotdir: Option<&str>, preview: Option<&Value>) -> Operation {
    Operation::RepairDotfileHooks {
        shell,
        zdotdir: zdotdir.map(String::from),
        review: preview.map(|plan| plan["review"].as_str().unwrap().into()),
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
        plan["fragment_changes"],
        json!({
            "baseline": "inactive",
            "fragments": [
                {"role": "environment", "before": null, "after": "export PROJECT=local\n"},
                {"role": "prompt", "before": null, "after": "PS1='managed> '\n"},
            ]
        })
    );
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
    assert_eq!(login["add_newline_before_hook"], true);
    assert_eq!(
        login["after_sha256"],
        digest(&format!("source ~/.bashrc\n{login_hook}"))
    );
    let rc = file(&plan, "/.bashrc");
    let rc_hook = rc["hook"].as_str().unwrap();
    assert!(rc.get("add_newline_before_hook").is_none());
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
    fs::remove_file(root.join("config/zsh/.zshrc")).unwrap();
    let repair =
        |preview: Option<&Value>| repair_hooks(DotfileShell::Zsh, Some("config/zsh"), preview);
    let reviewed = guarded_job(&pane, None, repair(None)).await.unwrap();
    assert_eq!(reviewed["supports_atomic_replace"], false);
    assert_eq!(reviewed["ready"], true);
    assert_eq!(file(&reviewed, "/config/zsh/.zshrc")["action"], "create");
    let repaired = guarded_job(&pane, None, repair(Some(&reviewed)))
        .await
        .unwrap();
    assert_eq!(repaired["changed_files"], json!(["/config/zsh/.zshrc"]));
    assert_eq!(
        fs::read_to_string(root.join("config/zsh/.zshrc")).unwrap(),
        rc
    );
    assert_eq!(
        fs::read_to_string(managed.join("zsh-current")).unwrap(),
        format!("{id}\n")
    );
    assert!(!root.join(".zshrc").exists());
    let edited = rc.replace("/prompt.sh", "/custom-prompt.sh");
    fs::write(root.join("config/zsh/.zshrc"), &edited).unwrap();
    let blocked = guarded_job(&pane, None, repair(None)).await.unwrap();
    assert_eq!(
        file(&blocked, "/config/zsh/.zshrc")["action"],
        "replace-hook"
    );
    assert_eq!(blocked["ready"], false);
    let error = guarded_job(&pane, None, repair(Some(&blocked)))
        .await
        .unwrap_err();
    assert!(error.contains("cannot atomically replace"), "{error}");
    assert_eq!(
        fs::read_to_string(root.join("config/zsh/.zshrc")).unwrap(),
        edited
    );
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn startup_hook_repair_replaces_only_the_reviewed_managed_block() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let root = remote.path();
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    let login = root.join(".bash_profile");
    let unused_login = root.join(".profile");
    let startup = root.join(".bashrc");
    fs::write(&login, "# Bash login prefix\r\n").unwrap();
    fs::write(&unused_login, "# Unselected profile\n").unwrap();
    let preview = guarded_job(&pane, None, bundle(DotfileShell::Bash, None))
        .await
        .unwrap();
    guarded_job(&pane, None, apply("managed", &preview))
        .await
        .unwrap();
    let store = root.join(".config/w11term/dotfiles");
    let selector = store.join("bash-current");
    let held = format!("{}manual\n", fs::read_to_string(&selector).unwrap());
    fs::write(&selector, &held).unwrap();
    let active_prompt = store.join(format!("bash-{}/prompt.sh", held.lines().next().unwrap()));
    let original_payload = fs::read(&active_prompt).unwrap();
    let original_login = fs::read(&login).unwrap();
    let hook = fs::read_to_string(&startup).unwrap();

    let repair = |preview: Option<&Value>| repair_hooks(DotfileShell::Bash, None, preview);
    let edited = hook.replace("/prompt.sh", "/other-prompt.sh");
    let nested = edited.replace(
        "/other-prompt.sh\"\n",
        "/other-prompt.sh\"\n    # >>> w11term tool PATH another-block >>>\n",
    );
    assert_ne!(edited, nested);
    let malformed = edited
        .replace("dotfiles >>>", "dotfiles >>")
        .replace("dotfiles <<<", "dotfiles <<");
    for ambiguous in [
        nested,
        malformed,
        format!("{edited}# >>> w11term dotfiles >>>\n"),
    ] {
        fs::write(&startup, &ambiguous).unwrap();
        let blocked = guarded_job(&pane, None, repair(None)).await.unwrap();
        assert_eq!(blocked["ready"], false);
        assert_eq!(file(&blocked, "/.bashrc")["action"], "hook-conflict");
        let error = guarded_job(&pane, None, repair(Some(&blocked)))
            .await
            .unwrap_err();
        assert!(
            error.contains("Resolve the startup hook conflict at /.bashrc"),
            "{error}"
        );
        assert_eq!(fs::read_to_string(&startup).unwrap(), ambiguous);
        assert_eq!(fs::read(&login).unwrap(), original_login);
    }

    let prefix = "# private prefix\r\n";
    let suffix = "# private suffix without newline";
    let damaged = format!("{prefix}{edited}{suffix}");
    fs::write(&startup, &damaged).unwrap();
    let reviewed = guarded_job(&pane, None, repair(None)).await.unwrap();
    assert_eq!(reviewed["ready"], true);
    assert_eq!(file(&reviewed, "/.bash_profile")["action"], "unchanged");
    let change = file(&reviewed, "/.bashrc");
    assert_eq!(change["action"], "replace-hook");
    assert_eq!(change["replaced_hook"], edited);
    assert_eq!(change["hook"], hook);
    assert!(!reviewed.to_string().contains("private prefix"));
    assert!(!reviewed.to_string().contains("private suffix"));
    assert_eq!(fs::read_to_string(&startup).unwrap(), damaged);
    assert_eq!(fs::read_to_string(&selector).unwrap(), held);

    let new_prefix = "# updated prefix\r\n";
    let changed = format!("{new_prefix}{edited}{suffix}");
    fs::write(&startup, &changed).unwrap();
    let error = guarded_job(&pane, None, repair(Some(&reviewed)))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    assert_eq!(fs::read_to_string(&startup).unwrap(), changed);
    let fresh = guarded_job(&pane, None, repair(None)).await.unwrap();
    let repaired = guarded_job(&pane, None, repair(Some(&fresh)))
        .await
        .unwrap();
    assert_eq!(repaired["changed_files"], json!(["/.bashrc"]));
    assert_eq!(
        fs::read(&startup).unwrap(),
        format!("{new_prefix}{hook}{suffix}").as_bytes()
    );
    assert_eq!(fs::read_to_string(&selector).unwrap(), held);
    assert_eq!(fs::read(&login).unwrap(), original_login);
    assert_eq!(fs::read(&active_prompt).unwrap(), original_payload);
    assert_eq!(fs::read(&unused_login).unwrap(), b"# Unselected profile\n");
    let current = guarded_job(&pane, None, repair(None)).await.unwrap();
    let noop = guarded_job(&pane, None, repair(Some(&current)))
        .await
        .unwrap();
    assert_eq!(noop["changed_files"], json!([]));
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dotfiles_installation_rechecks_the_review_and_publishes_one_active_version() {
    let (pane, _, remote, opens, server) = fixture(false).await;
    let root = remote.path();
    let managed = root.join(".config/w11term/dotfiles");
    let selector = managed.join("bash-current");
    let startup = root.join(".bashrc");
    let history = || Operation::ListDotfileVersions {
        shell: DotfileShell::Bash,
    };
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    fs::write(&startup, "# original Bash configuration\n").unwrap();
    let empty = guarded_job(&pane, None, history()).await.unwrap();
    assert_eq!(empty["home"], "/");
    assert_eq!(empty["state"], "inactive");
    assert_eq!(empty["versions"], json!([]));
    assert!(!managed.exists());
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
    let prompt_delta = &no_changes["fragment_changes"]["fragments"][1];
    assert_eq!(no_changes["fragment_changes"]["baseline"], "verified");
    assert_eq!(prompt_delta["before"], prompt_delta["after"]);
    let checked = guarded_job(&pane, None, apply("first", &no_changes))
        .await
        .unwrap();
    assert_eq!(checked["changed_files"], json!([]));

    let second = guarded_job(&pane, None, preview("second")).await.unwrap();
    assert_eq!(
        second["fragment_changes"]["fragments"][1],
        json!({"role": "prompt", "before": "PS1='first> '\n", "after": "PS1='second> '\n"})
    );
    let first_prompt = version.join("prompt.sh");
    fs::write(&first_prompt, "# edited outside w11term\n").unwrap();
    let error = guarded_job(&pane, None, apply("second", &second))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    let without_baseline = guarded_job(&pane, None, preview("second")).await.unwrap();
    assert_eq!(
        without_baseline["fragment_changes"]["baseline"],
        "unavailable"
    );
    assert!(without_baseline["fragment_changes"]["fragments"][1]["before"].is_null());
    assert_eq!(
        without_baseline["fragment_changes"]["fragments"][1]["after"],
        "PS1='second> '\n"
    );
    assert!(
        !without_baseline
            .to_string()
            .contains("edited outside w11term")
    );
    assert!(
        !managed
            .join(format!(
                "bash-{}",
                second["bundle_sha256"].as_str().unwrap()
            ))
            .exists()
    );
    fs::write(&first_prompt, "PS1='first> '\n").unwrap();
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
    fs::create_dir(managed.join(format!("zsh-{}", "a".repeat(64)))).unwrap();
    fs::create_dir(managed.join("bash-unrelated")).unwrap();
    fs::write(managed.join(format!("bash-{}", "a".repeat(64))), []).unwrap();
    let listed = guarded_job(&pane, None, history()).await.unwrap();
    assert_eq!(listed["state"], "active");
    assert_eq!(listed["active_bundle_sha256"], new_id);
    let versions = listed["versions"].as_array().unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0]["bundle_sha256"], new_id);
    assert_eq!(versions[1]["bundle_sha256"], id);

    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("Older shell bundle.json");
    let export = |local| Operation::ExportDotfileBundle {
        shell: DotfileShell::Bash,
        bundle_sha256: id.into(),
        local,
    };
    let saved = guarded_job(&pane, None, export(snapshot.clone()))
        .await
        .unwrap();
    assert_eq!(
        saved,
        json!({"shell": "bash", "home": "/", "bundle_sha256": id, "local": snapshot})
    );
    let contents = fs::read(&snapshot).unwrap();
    let snapshot_bundle = DotfileBundle::load(&snapshot).unwrap();
    assert_eq!(snapshot_bundle.bash.as_deref(), Some("PS1='first> '\n"));
    assert_eq!(
        snapshot_bundle.environment.as_deref(),
        Some("export PROJECT=local\n")
    );
    assert!(snapshot_bundle.zsh.is_none());
    let roundtrip = guarded_job(
        &pane,
        None,
        Operation::PreviewDotfiles {
            bundle: snapshot_bundle,
            shell: DotfileShell::Bash,
            zdotdir: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(roundtrip["bundle_sha256"], id);
    assert!(
        guarded_job(&pane, None, export(snapshot.clone()))
            .await
            .is_err()
    );
    assert_eq!(fs::read(&snapshot).unwrap(), contents);
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{new_id}\n")
    );

    let restore = guarded_job(&pane, None, switch(Some(id), None))
        .await
        .unwrap();
    assert_eq!(restore["active_bundle_sha256"], new_id);
    assert_eq!(restore["bundle_sha256"], id);
    assert_eq!(restore["selector"]["action"], "replace");
    assert_eq!(
        restore["fragment_changes"]["fragments"][1],
        json!({"role": "prompt", "before": "PS1='second> '\n", "after": "PS1='first> '\n"})
    );
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{new_id}\n")
    );
    fs::write(&first_prompt, "PS1='drift> '\n").unwrap();
    let corrupt = snapshots.path().join("Corrupt retained shell bundle.json");
    let error = guarded_job(&pane, None, export(corrupt.clone()))
        .await
        .unwrap_err();
    assert!(error.contains("do not match bundle"), "{error}");
    assert!(!corrupt.exists());
    let error = guarded_job(&pane, None, switch(Some(id), Some(&restore)))
        .await
        .unwrap_err();
    assert!(error.contains("do not match bundle"), "{error}");
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{new_id}\n")
    );
    fs::write(&first_prompt, "PS1='first> '\n").unwrap();

    let disable = guarded_job(&pane, None, switch(None, None)).await.unwrap();
    assert_eq!(
        disable["fragment_changes"]["fragments"][1],
        json!({"role": "prompt", "before": "PS1='second> '\n", "after": null})
    );
    let disabled = guarded_job(&pane, None, switch(None, Some(&disable)))
        .await
        .unwrap();
    assert_eq!(disabled["changed"], true);
    assert!(disabled["bundle_sha256"].is_null());
    assert_eq!(fs::read_to_string(&selector).unwrap(), "disabled\n");
    let listed = guarded_job(&pane, None, history()).await.unwrap();
    assert_eq!(listed["state"], "disabled");
    assert!(listed["active_bundle_sha256"].is_null());
    assert_eq!(listed["versions"].as_array().unwrap().len(), 2);
    let error = guarded_job(&pane, None, switch(Some(id), Some(&restore)))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    assert_eq!(fs::read_to_string(&selector).unwrap(), "disabled\n");

    let restore = guarded_job(&pane, None, switch(Some(id), None))
        .await
        .unwrap();
    assert!(restore["active_bundle_sha256"].is_null());
    assert_eq!(restore["fragment_changes"]["baseline"], "inactive");
    assert!(restore["fragment_changes"]["fragments"][1]["before"].is_null());
    let restored = guarded_job(&pane, None, switch(Some(id), Some(&restore)))
        .await
        .unwrap();
    assert_eq!(restored["changed"], true);
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{id}\nmanual\n")
    );
    let listed = guarded_job(&pane, None, history()).await.unwrap();
    assert_eq!(listed["state"], "paused");
    assert_eq!(listed["active_bundle_sha256"], id);
    assert_eq!(listed["versions"][0]["bundle_sha256"], id);
    let repeat = guarded_job(&pane, None, switch(Some(id), None))
        .await
        .unwrap();
    let repeated = guarded_job(&pane, None, switch(Some(id), Some(&repeat)))
        .await
        .unwrap();
    assert_eq!(repeated["changed"], false);

    let resume = |plan: Option<&Value>| dotfile_switch(DotfileTarget::Automatic, plan);
    let reviewed = guarded_job(&pane, None, resume(None)).await.unwrap();
    assert_eq!(reviewed["target"]["kind"], "automatic");
    assert_eq!(reviewed["active_bundle_sha256"], id);
    assert_eq!(reviewed["bundle_sha256"], id);
    assert_eq!(reviewed["selector"]["action"], "replace");
    assert_eq!(
        reviewed["fragment_changes"]["fragments"][1],
        json!({"role": "prompt", "before": "PS1='first> '\n", "after": "PS1='first> '\n"})
    );
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{id}\nmanual\n")
    );

    fs::write(&selector, "disabled\n").unwrap();
    let error = guarded_job(&pane, None, resume(Some(&reviewed)))
        .await
        .unwrap_err();
    assert!(error.contains("review is stale"), "{error}");
    assert_eq!(fs::read_to_string(&selector).unwrap(), "disabled\n");
    fs::write(&selector, format!("{id}\nmanual\n")).unwrap();

    fs::write(&first_prompt, "PS1='edited> '\n").unwrap();
    let error = guarded_job(&pane, None, resume(Some(&reviewed)))
        .await
        .unwrap_err();
    assert!(error.contains("do not match bundle"), "{error}");
    assert_eq!(
        fs::read_to_string(&selector).unwrap(),
        format!("{id}\nmanual\n")
    );
    fs::write(&first_prompt, "PS1='first> '\n").unwrap();

    let resumed = guarded_job(&pane, None, resume(Some(&reviewed)))
        .await
        .unwrap();
    assert_eq!(resumed["target"]["kind"], "automatic");
    assert_eq!(resumed["bundle_sha256"], id);
    assert!(resumed["changed"].as_bool().unwrap());
    assert_eq!(fs::read_to_string(&selector).unwrap(), format!("{id}\n"));
    let listed = guarded_job(&pane, None, history()).await.unwrap();
    assert_eq!(listed["state"], "active");
    assert_eq!(listed["versions"][0]["bundle_sha256"], id);
    let error = guarded_job(&pane, None, resume(None)).await.unwrap_err();
    assert!(error.contains("Refresh dotfile versions"), "{error}");

    assert_eq!(fs::read_to_string(&startup).unwrap(), installed);
    assert_eq!(
        fs::read_to_string(&first_prompt).unwrap(),
        "PS1='first> '\n"
    );
    assert_eq!(opens.load(Ordering::Relaxed), 2);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exported_dotfile_bundle_round_trips_a_fragment_at_its_size_limit() {
    let (pane, _, remote, _, server) = fixture(false).await;
    let environment = format!("# {}", "x".repeat(16 * 1024 - 2));
    let bundle = DotfileBundle {
        version: 1,
        environment: Some(environment.clone()),
        bash: None,
        zsh: Some("PROMPT='portable> '\r\n".into()),
    };
    let preview = |bundle| Operation::PreviewDotfiles {
        bundle,
        shell: DotfileShell::Zsh,
        zdotdir: None,
    };
    let plan = guarded_job(&pane, None, preview(bundle.clone()))
        .await
        .unwrap();
    let id = plan["bundle_sha256"].as_str().unwrap();
    guarded_job(
        &pane,
        None,
        Operation::ApplyDotfiles {
            bundle,
            shell: DotfileShell::Zsh,
            zdotdir: None,
            review: plan["review"].as_str().unwrap().into(),
        },
    )
    .await
    .unwrap();
    let remote_environment = remote
        .path()
        .join(format!(".config/w11term/dotfiles/zsh-{id}/environment.sh"));
    assert_eq!(
        fs::read_to_string(remote_environment).unwrap(),
        format!("{environment}\n")
    );
    let snapshots = tempfile::tempdir().unwrap();
    let snapshot = snapshots.path().join("Maximum Zsh fragment.json");
    guarded_job(
        &pane,
        None,
        Operation::ExportDotfileBundle {
            shell: DotfileShell::Zsh,
            bundle_sha256: id.into(),
            local: snapshot.clone(),
        },
    )
    .await
    .unwrap();
    let saved = DotfileBundle::load(&snapshot).unwrap();
    assert_eq!(saved.environment.as_deref(), Some(environment.as_str()));
    assert_eq!(saved.zsh.as_deref(), Some("PROMPT='portable> '\n"));
    assert!(saved.bash.is_none());
    assert_eq!(
        guarded_job(&pane, None, preview(saved)).await.unwrap()["bundle_sha256"],
        id
    );
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

pub(super) fn connect_options(source: Source, local: &std::path::Path) -> SshOptions {
    let Source::Ssh(mut options) = source else {
        unreachable!()
    };
    let settings = DotfileSync {
        bundle: "Fleet dotfiles.json".into(),
        shell: DotfileShell::Bash,
        zdotdir: None,
        host: options.host.clone(),
        user: options.user.clone(),
        port: options.port,
        fingerprint: options.host_key.clone().unwrap(),
        home: "/".into(),
    };
    options.dotfiles = Some(Box::new(settings));
    let mut options: SshOptions =
        serde_json::from_value(serde_json::to_value(options).unwrap()).unwrap();
    options
        .prepare_profile_connection(&local.join("profiles.json"))
        .unwrap();
    options
}

async fn login(options: &SshOptions, selector: &str, expected: &str) -> Session {
    let pane = Session::start_sized(
        Source::Ssh(options.clone()),
        wezterm_term::TerminalSize {
            cols: 120,
            ..Default::default()
        },
    );
    let probe = format!("dotfiles-at-shell:{selector}");
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let frame = pane.frames.load();
            if frame.connection_generation != 0
                && frame.rows.iter().any(|row| row.as_str().contains(&probe))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|error| {
        let frame = pane.frames.load();
        let text = frame
            .rows
            .iter()
            .take(4)
            .map(|row| row.as_str().into_owned())
            .collect::<Vec<_>>();
        panic!(
            "Waiting for {probe}: {error}; screen: {text:?}; {:?}",
            pane.events.since(0).events
        )
    });
    let batch = pane.events.since(0);
    let sync = batch
        .events
        .iter()
        .find(|e| e.kind == "dotfiles-sync")
        .unwrap();
    assert_eq!(sync.data["status"], expected, "{sync:?}");
    let generation = pane.ssh_connection_generation().unwrap();
    let report = pane.events.dotfile_sync(generation).unwrap();
    assert_eq!(report.shell, DotfileShell::Bash);
    assert_eq!(serde_json::to_value(&*report).unwrap(), sync.data);
    pane
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_sync_activates_before_the_shell_and_respects_manual_selections() {
    let (source, remote, _, server) = fixture_connections(false, 9).await;
    let root = remote.path();
    fs::write(root.join("posix-rename-version"), "1").unwrap();
    fs::write(root.join("dotfiles-shell-probe"), []).unwrap();
    let local = tempfile::tempdir().unwrap();
    let options = connect_options(source, local.path());
    let path = local.path().join("Fleet dotfiles.json");
    let write = |prompt| fs::write(&path, serde_json::to_vec(&manifest(prompt)).unwrap()).unwrap();
    let id = |prompt: &str| {
        let environment = "export PROJECT=local\n";
        let mut sha = Sha256::new_with_prefix(b"w11term-dotfiles-v1\0");
        sha.update((environment.len() as u64).to_le_bytes());
        sha.update(environment);
        sha.update(format!("PS1='{prompt}> '\n"));
        sha.finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    write("first");
    let first = id("first");
    drop(login(&options, &first, "updated").await);
    drop(login(&options, &first, "current").await);
    write("second");
    let second = id("second");
    let pane = login(&options, &second, "updated").await;
    let restore = guarded_job(&pane, None, switch(Some(&first), None))
        .await
        .unwrap();
    guarded_job(&pane, None, switch(Some(&first), Some(&restore)))
        .await
        .unwrap();
    drop(pane);
    let pane = login(&options, &first, "held").await;
    let resume = |plan: Option<&Value>| dotfile_switch(DotfileTarget::Automatic, plan);
    let reviewed = guarded_job(&pane, None, resume(None)).await.unwrap();
    guarded_job(&pane, None, resume(Some(&reviewed)))
        .await
        .unwrap();
    drop(pane);
    let pane = login(&options, &second, "updated").await;
    let disable = guarded_job(&pane, None, switch(None, None)).await.unwrap();
    guarded_job(&pane, None, switch(None, Some(&disable)))
        .await
        .unwrap();
    drop(pane);
    fs::remove_file(&path).unwrap();
    let pane = login(&options, "disabled", "held").await;
    let reviewed = guarded_job(&pane, None, resume(None)).await.unwrap();
    assert!(reviewed["active_bundle_sha256"].is_null());
    assert!(reviewed["bundle_sha256"].is_null());
    let resumed = guarded_job(&pane, None, resume(Some(&reviewed)))
        .await
        .unwrap();
    assert_eq!(resumed["changed"], true);
    let listed = guarded_job(
        &pane,
        None,
        Operation::ListDotfileVersions {
            shell: DotfileShell::Bash,
        },
    )
    .await
    .unwrap();
    assert_eq!(listed["state"], "awaiting-sync");
    assert!(listed["active_bundle_sha256"].is_null());
    drop(pane);
    drop(login(&options, "absent", "skipped").await);
    assert_eq!(
        fs::read_to_string(root.join(".config/w11term/dotfiles/bash-current")).unwrap(),
        "automatic\n"
    );

    write("second");
    drop(login(&options, &second, "updated").await);
    let active = root.join(".config/w11term/dotfiles");
    let prompt = active.join(format!("bash-{second}/prompt.sh"));
    fs::write(&prompt, "PS1='remote edit> '\n").unwrap();
    write("first");
    drop(login(&options, &second, "skipped").await);
    assert_eq!(
        fs::read_to_string(active.join("bash-current")).unwrap(),
        format!("{second}\n")
    );
    assert_eq!(fs::read_to_string(prompt).unwrap(), "PS1='remote edit> '\n");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn profile_sync_with_a_mismatched_approval_still_opens_a_shell() {
    let (source, remote, opens, server) = fixture_connections(false, 2).await;
    fs::write(remote.path().join("dotfiles-shell-probe"), []).unwrap();
    let local = tempfile::tempdir().unwrap();
    let mut options = connect_options(source, local.path());
    fs::write(
        &options.dotfiles.as_ref().unwrap().bundle,
        serde_json::to_vec(&manifest("first")).unwrap(),
    )
    .unwrap();
    let reviewed = options.dotfiles.clone();
    options.dotfiles.as_mut().unwrap().user = "another-account".into();
    drop(login(&options, "absent", "skipped").await);
    assert_eq!(opens.load(Ordering::Relaxed), 1, "only the shell channel");
    assert!(!remote.path().join(".config").exists());
    options.dotfiles = reviewed;
    options.dotfiles.as_mut().unwrap().home = "/another-home".into();
    drop(login(&options, "absent", "skipped").await);
    assert_eq!(
        opens.load(Ordering::Relaxed),
        3,
        "second login checks the SFTP home"
    );
    assert!(!remote.path().join(".config").exists());
    server.abort();
}
