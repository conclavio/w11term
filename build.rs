fn main() {
    windows_reactor_setup::as_self_contained();

    let script = std::path::Path::new("src/conpty/shell-integration.ps1");
    println!("cargo:rerun-if-changed={}", script.display());
    let out = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    // Cargo uses <target>/<profile>/build/<package>/out, including with --target.
    let profile = out.ancestors().nth(3).expect("Cargo profile directory");
    // The application and integration-test executables have different parents.
    for directory in [profile.to_owned(), profile.join("deps")] {
        std::fs::copy(script, directory.join("shell-integration.ps1"))
            .expect("stage the PowerShell integration script");
    }
}
