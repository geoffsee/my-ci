use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ui_dir = manifest_dir.join("ui");
    let dist_dir = ui_dir.join("dist");
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR set by cargo"));
    let archive_path = out_dir.join("ui-dist.tar.gz");

    println!("cargo:rerun-if-env-changed=MY_CI_SKIP_UI_BUILD");
    for rel in [
        "ui/package.json",
        "ui/bun.lock",
        "ui/tsconfig.json",
        "ui/vite.config.ts",
        "ui/index.html",
        "ui/src",
        "ui/dist",
    ] {
        println!("cargo:rerun-if-changed={rel}");
    }

    if !ui_dir.is_dir() {
        println!("cargo:warning=ui/ directory not found; embedding empty UI archive");
        write_empty_archive(&archive_path);
        return;
    }

    let in_publish_verify = manifest_dir
        .components()
        .any(|c| c.as_os_str() == "package")
        && manifest_dir.components().any(|c| c.as_os_str() == "target");
    let skip_bun = std::env::var_os("MY_CI_SKIP_UI_BUILD").is_some() || in_publish_verify;

    let dist_ready = dist_dir.join("index.html").is_file();

    if !dist_ready && !skip_bun {
        let bun = which("bun").unwrap_or_else(|| {
            panic!(
                "bun not found on PATH. Install bun (https://bun.sh) or pre-build ui/dist/, or set MY_CI_SKIP_UI_BUILD=1 to embed an empty UI."
            );
        });
        if !ui_dir.join("node_modules").is_dir() {
            run(&bun, &["install"], &ui_dir, "bun install");
        }
        run(&bun, &["run", "build"], &ui_dir, "bun run build");
    }

    if !dist_dir.join("index.html").is_file() {
        println!("cargo:warning=ui/dist not present; embedding empty UI archive");
        write_empty_archive(&archive_path);
        return;
    }

    pack(&dist_dir, &archive_path).expect("pack ui/dist into archive");
}

fn pack(dist_dir: &Path, archive_path: &Path) -> std::io::Result<()> {
    let file = File::create(archive_path)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::best());
    let mut builder = tar::Builder::new(encoder);
    builder.follow_symlinks(false);
    builder.append_dir_all(".", dist_dir)?;
    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
}

fn write_empty_archive(archive_path: &Path) {
    // Still emit a valid (empty) gzip-compressed tar so include_bytes! has a target.
    let file = File::create(archive_path).expect("create empty UI archive");
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    // Two zeroed 512-byte blocks form a valid empty tar end-of-archive marker.
    let zeros = [0u8; 1024];
    encoder.write_all(&zeros).expect("write empty tar");
    encoder.finish().expect("finalize empty UI archive");
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
