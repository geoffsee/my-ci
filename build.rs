use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ui_dir = manifest_dir.join("ui");

    if !ui_dir.is_dir() {
        println!("cargo:warning=ui/ directory not found; skipping UI build");
        return;
    }

    for rel in [
        "ui/package.json",
        "ui/bun.lock",
        "ui/tsconfig.json",
        "ui/vite.config.js",
        "ui/index.html",
        "ui/src",
    ] {
        println!("cargo:rerun-if-changed={rel}");
    }
    println!("cargo:rerun-if-env-changed=MY_CI_SKIP_UI_BUILD");

    if std::env::var_os("MY_CI_SKIP_UI_BUILD").is_some() {
        println!("cargo:warning=MY_CI_SKIP_UI_BUILD set; skipping UI build");
        return;
    }

    let bun = match which("bun") {
        Some(path) => path,
        None => {
            panic!(
                "bun not found on PATH. Install bun (https://bun.sh) or set MY_CI_SKIP_UI_BUILD=1 to skip the UI build."
            );
        }
    };

    if !ui_dir.join("node_modules").is_dir() {
        run(&bun, &["install"], &ui_dir, "bun install");
    }

    run(&bun, &["run", "build"], &ui_dir, "bun run build");
}

fn run(program: &Path, args: &[&str], cwd: &Path, label: &str) {
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap_or_else(|err| panic!("failed to spawn `{label}`: {err}"));
    if !status.success() {
        panic!("`{label}` failed with status {status}");
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(cmd);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
