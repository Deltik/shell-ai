use anyhow::{bail, Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

const BINARY_NAME: &str = "shell-ai";

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: xtask <command> [args...]");
        eprintln!("Commands:");
        eprintln!("  package <target> [target...]  - Package built binaries for the given targets");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "package" => {
            if args.len() < 3 {
                bail!("Usage: xtask package <target> [target...]");
            }
            let targets: Vec<&str> = args[2..].iter().map(|s| s.as_str()).collect();
            package_targets(&targets)
        }
        cmd => bail!("Unknown command: {}", cmd),
    }
}

fn package_targets(targets: &[&str]) -> Result<()> {
    let artifacts_dir = Path::new("artifacts");
    fs::create_dir_all(artifacts_dir)?;

    for target in targets {
        package_target(target, artifacts_dir)?;
    }

    // List artifacts
    println!("\nArtifacts:");
    for entry in fs::read_dir(artifacts_dir)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        println!(
            "  {} ({} bytes)",
            entry.file_name().to_string_lossy(),
            metadata.len()
        );
    }

    Ok(())
}

fn package_target(target: &str, artifacts_dir: &Path) -> Result<()> {
    let release_dir = Path::new("target").join(target).join("release");

    if !release_dir.exists() {
        println!("Skipping {} (build not found)", target);
        return Ok(());
    }

    let is_windows = target.contains("windows");
    let binary_name = if is_windows {
        format!("{}.exe", BINARY_NAME)
    } else {
        BINARY_NAME.to_string()
    };

    let binary_path = release_dir.join(&binary_name);
    if !binary_path.exists() {
        println!("Skipping {} (binary not found)", target);
        return Ok(());
    }

    println!("Packaging {}...", target);

    // Copy standalone binary
    let standalone_name = if is_windows {
        format!("{}-{}.exe", BINARY_NAME, target)
    } else {
        format!("{}-{}", BINARY_NAME, target)
    };
    fs::copy(&binary_path, artifacts_dir.join(&standalone_name))
        .context("Failed to copy standalone binary")?;

    // Create archive
    if is_windows {
        create_zip(target, &binary_path, artifacts_dir)?;
    } else {
        create_tarball(target, &binary_path, artifacts_dir)?;
    }

    Ok(())
}

fn create_zip(target: &str, binary_path: &Path, artifacts_dir: &Path) -> Result<()> {
    let archive_name = format!("{}-{}.zip", BINARY_NAME, target);
    let archive_path = artifacts_dir.join(&archive_name);
    let file = File::create(&archive_path).context("Failed to create zip file")?;
    let mut zip = ZipWriter::new(file);

    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    // Read binary
    let mut binary_data = Vec::new();
    File::open(binary_path)?.read_to_end(&mut binary_data)?;

    // Add shell-ai.exe
    zip.start_file(format!("{}.exe", BINARY_NAME), options)?;
    zip.write_all(&binary_data)?;

    // Add shai.exe (copy of binary)
    zip.start_file("shai.exe", options)?;
    zip.write_all(&binary_data)?;

    zip.finish()?;
    Ok(())
}

fn create_tarball(target: &str, binary_path: &Path, artifacts_dir: &Path) -> Result<()> {
    let archive_name = format!("{}-{}.tar.gz", BINARY_NAME, target);
    let archive_path = artifacts_dir.join(&archive_name);
    let file = File::create(&archive_path).context("Failed to create tarball")?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = tar::Builder::new(encoder);

    // Read binary
    let mut binary_data = Vec::new();
    File::open(binary_path)?.read_to_end(&mut binary_data)?;

    // Add shell-ai
    let mut header = tar::Header::new_gnu();
    header.set_size(binary_data.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_data(&mut header, BINARY_NAME, binary_data.as_slice())?;

    // Add shai as symlink
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o755);
    header.set_cksum();
    tar.append_link(&mut header, "shai", BINARY_NAME)?;

    tar.finish()?;
    Ok(())
}