use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static INTERRUPT_COUNT: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy)]
enum Shell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
}

impl Shell {
    fn name(&self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
            Shell::PowerShell => "powershell",
        }
    }

    fn display_name(&self) -> &'static str {
        match self {
            Shell::Bash => "Bash",
            Shell::Zsh => "Zsh",
            Shell::Fish => "Fish",
            Shell::PowerShell => "PowerShell",
        }
    }

    fn command(&self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
            Shell::PowerShell => "pwsh",
        }
    }

    fn extension(&self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
            Shell::PowerShell => "ps1",
        }
    }

    fn is_available(&self) -> bool {
        Command::new("which")
            .arg(self.command())
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn all() -> &'static [Shell] {
        &[Shell::Bash, Shell::Zsh, Shell::Fish, Shell::PowerShell]
    }
}

struct BenchmarkStats {
    n: usize,
    min: f64,
    q1: f64,
    median: f64,
    q3: f64,
    max: f64,
    mean: f64,
    stdev: f64,
}

impl BenchmarkStats {
    fn from_times(times: &[f64]) -> Self {
        let mut sorted = times.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let n = sorted.len();
        let sum: f64 = sorted.iter().sum();
        let mean = sum / n as f64;

        let variance: f64 =
            sorted.iter().map(|t| (t - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
        let stdev = variance.sqrt();

        BenchmarkStats {
            n,
            min: sorted[0],
            q1: sorted[n / 4],
            median: sorted[n / 2],
            q3: sorted[3 * n / 4],
            max: sorted[n - 1],
            mean,
            stdev,
        }
    }
}

pub fn run(samples: usize, keep_results: bool) -> Result<()> {
    ctrlc::set_handler(|| {
        let count = INTERRUPT_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        if count == 1 {
            eprintln!("\nInterrupted! Finishing current sample and outputting partial results...");
            eprintln!("Press Ctrl+C again to exit immediately.");
        } else {
            eprintln!("\nForce exit.");
            std::process::exit(130);
        }
    })
    .ok();

    println!("Shell Integration Benchmark");
    println!("===========================");
    println!("Samples per scenario: {}", samples);
    println!();

    let available_shells: Vec<Shell> = Shell::all()
        .iter()
        .copied()
        .filter(|s| s.is_available())
        .collect();

    if available_shells.is_empty() {
        bail!("No supported shells found (bash, zsh, fish, pwsh)");
    }

    println!(
        "Available shells: {}",
        available_shells
            .iter()
            .map(|s| s.name())
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!();

    println!("Building release binary...");
    let status = Command::new("cargo")
        .args(["build", "--release"])
        .status()
        .context("Failed to run cargo build")?;

    if !status.success() {
        bail!("Failed to build release binary");
    }

    let binary_path = find_release_binary()?;
    println!("Using binary: {}", binary_path.display());

    let temp_dir = env::temp_dir().join("shai-bench");
    fs::create_dir_all(&temp_dir)?;
    println!("Working directory: {}", temp_dir.display());
    println!();

    println!("Generating integration files...");
    let presets = ["minimal", "standard", "full"];

    for shell in &available_shells {
        let blank_path = temp_dir.join(format!("blank.{}", shell.extension()));
        fs::write(&blank_path, "# empty\n")?;

        for preset in &presets {
            let output = Command::new(&binary_path)
                .args([
                    "integration",
                    "generate",
                    shell.name(),
                    "--preset",
                    preset,
                    "--stdout",
                ])
                .output()
                .context("Failed to generate integration")?;

            let file_path = temp_dir.join(format!(
                "{}_{}.{}",
                shell.name(),
                preset,
                shell.extension()
            ));
            fs::write(&file_path, &output.stdout)?;
        }
    }

    let mut all_results: Vec<(Shell, String, BenchmarkStats)> = Vec::new();
    let mut raw_data: Vec<(Shell, String, Vec<f64>)> = Vec::new();
    let mut interrupted = false;

    'outer: for shell in &available_shells {
        println!("\nBenchmarking {}...", shell.name());

        let scenarios = ["blank", "minimal", "standard", "full"];

        for scenario in &scenarios {
            if INTERRUPT_COUNT.load(Ordering::SeqCst) > 0 {
                interrupted = true;
                break 'outer;
            }

            let file_name = if *scenario == "blank" {
                format!("blank.{}", shell.extension())
            } else {
                format!("{}_{}.{}", shell.name(), scenario, shell.extension())
            };
            let file_path = temp_dir.join(&file_name);

            print!("  {}: ", scenario);
            std::io::stdout().flush()?;

            let times = run_cold_benchmark(*shell, &file_path, samples)?;

            if times.is_empty() {
                interrupted = true;
                println!("skipped");
                break 'outer;
            }

            let stats = BenchmarkStats::from_times(&times);
            println!("{:.2}ms mean ({:.2}ms median)", stats.mean, stats.median);
            raw_data.push((*shell, scenario.to_string(), times));
            all_results.push((*shell, scenario.to_string(), stats));
        }
    }

    if interrupted {
        println!("\n--- Benchmark interrupted, showing partial results ---");
    }

    if !all_results.is_empty() {
        let csv_path = temp_dir.join("results.csv");
        save_raw_data_csv(&csv_path, &raw_data)?;
        println!("\nRaw data saved to: {}", csv_path.display());

        println!();
        print_results(&all_results);
    } else {
        println!("\nNo benchmark data collected.");
    }

    if keep_results {
        println!("\nResults preserved in: {}", temp_dir.display());
    } else {
        fs::remove_dir_all(&temp_dir).ok();
        println!("\nTemporary files cleaned up. Use --keep to preserve.");
    }

    Ok(())
}

fn find_release_binary() -> Result<PathBuf> {
    let target_dir = Path::new("target");
    if !target_dir.exists() {
        bail!("No target/ directory found. Run 'cargo build --release' first.");
    }

    let binary_name = if cfg!(windows) {
        "shell-ai.exe"
    } else {
        "shell-ai"
    };

    let mut candidates: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    find_binaries_recursive(target_dir, binary_name, &mut candidates)?;

    if candidates.is_empty() {
        bail!(
            "Could not find {} in target/. Run 'cargo build --release' first.",
            binary_name
        );
    }

    candidates.sort_by(|a, b| b.1.cmp(&a.1));
    Ok(candidates[0].0.canonicalize()?)
}

fn find_binaries_recursive(
    dir: &Path,
    binary_name: &str,
    results: &mut Vec<(PathBuf, std::time::SystemTime)>,
) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            find_binaries_recursive(&path, binary_name, results)?;
        } else if path.file_name().map(|n| n == binary_name).unwrap_or(false) {
            if let Ok(metadata) = path.metadata() {
                if let Ok(modified) = metadata.modified() {
                    results.push((path, modified));
                }
            }
        }
    }
    Ok(())
}

fn run_cold_benchmark(shell: Shell, file_path: &Path, samples: usize) -> Result<Vec<f64>> {
    let file_path_str = file_path.to_string_lossy();
    let mut times = Vec::with_capacity(samples);

    for _ in 0..samples {
        if INTERRUPT_COUNT.load(Ordering::SeqCst) > 0 {
            break;
        }

        let output = match shell {
            Shell::Bash => Command::new("bash")
                .args([
                    "-c",
                    &format!(
                        r#"start=$(date +%s%N); [ -f "{0}" ] && source "{0}"; end=$(date +%s%N); echo $((end - start))"#,
                        file_path_str
                    ),
                ])
                .output()?,
            Shell::Zsh => Command::new("zsh")
                .args([
                    "-c",
                    &format!(
                        r#"start=$(date +%s%N); [ -f "{0}" ] && source "{0}"; end=$(date +%s%N); echo $((end - start))"#,
                        file_path_str
                    ),
                ])
                .output()?,
            Shell::Fish => Command::new("fish")
                .args([
                    "-c",
                    &format!(
                        r#"set start (date +%s%N); [ -f "{0}" ] && source "{0}"; set end (date +%s%N); echo (math $end - $start)"#,
                        file_path_str
                    ),
                ])
                .output()?,
            Shell::PowerShell => Command::new("pwsh")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        r#"$sw = [System.Diagnostics.Stopwatch]::StartNew(); if (Test-Path '{0}') {{ . '{0}' }}; $sw.Stop(); Write-Output $sw.Elapsed.TotalNanoseconds"#,
                        file_path_str
                    ),
                ])
                .output()?,
        };

        if let Ok(time_ns) = parse_time_output(&output.stdout) {
            times.push(time_ns / 1_000_000.0);
        } else if INTERRUPT_COUNT.load(Ordering::SeqCst) > 0 {
            break;
        } else {
            parse_time_output(&output.stdout)?;
        }
    }

    Ok(times)
}

fn parse_time_output(output: &[u8]) -> Result<f64> {
    let s = String::from_utf8_lossy(output);
    let s = s.trim();
    s.parse::<f64>()
        .with_context(|| format!("Failed to parse time output: {:?}", s))
}

fn save_raw_data_csv(path: &Path, data: &[(Shell, String, Vec<f64>)]) -> Result<()> {
    use std::io::BufWriter;

    let file = File::create(path).context("Failed to create CSV file")?;
    let mut writer = BufWriter::new(file);

    writeln!(writer, "shell,preset,sample,time_ms")?;

    for (shell, preset, times) in data {
        for (i, time) in times.iter().enumerate() {
            writeln!(
                writer,
                "{},{},{},{:.6}",
                shell.name(),
                preset,
                i + 1,
                time
            )?;
        }
    }

    writer.flush()?;
    Ok(())
}

fn print_results(results: &[(Shell, String, BenchmarkStats)]) {
    println!("### Baseline: Sourcing an Empty File\n");
    println!("| Shell | N | Min | Q1 | Median | Q3 | Max | Mean | Std Dev |");
    println!("|-------|--:|----:|---:|-------:|---:|----:|-----:|--------:|");

    let mut baselines: HashMap<&str, f64> = HashMap::new();

    for (shell, scenario, stats) in results {
        if scenario == "blank" {
            baselines.insert(shell.name(), stats.mean);
            println!(
                "| {} | {} | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms |",
                shell.display_name(),
                stats.n,
                stats.min,
                stats.q1,
                stats.median,
                stats.q3,
                stats.max,
                stats.mean,
                stats.stdev
            );
        }
    }

    println!("\n### Incremental Overhead (Above Baseline)\n");
    println!("| Shell | Preset | Overhead (Mean) |");
    println!("|-------|--------|----------------:|");

    for (shell, scenario, stats) in results {
        if scenario != "blank" {
            let baseline = baselines.get(shell.name()).unwrap_or(&0.0);
            let overhead = stats.mean - baseline;
            println!(
                "| {} | {} | +{:.2}ms |",
                shell.display_name(),
                scenario,
                overhead
            );
        }
    }

    println!("\n### Total Overhead (What Users Experience)\n");

    for shell in Shell::all() {
        let shell_results: Vec<_> = results
            .iter()
            .filter(|(s, _, _)| s.name() == shell.name())
            .collect();
        if shell_results.is_empty() {
            continue;
        }

        println!("**{}**\n", shell.display_name());
        println!("| Preset | N | Min | Q1 | Median | Q3 | Max | Mean | Std Dev |");
        println!("|--------|--:|----:|---:|-------:|---:|----:|-----:|--------:|");

        for (_, scenario, stats) in shell_results {
            let label = if scenario == "blank" {
                "blank (baseline)"
            } else {
                scenario.as_str()
            };
            println!(
                "| {} | {} | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms | {:.2}ms |",
                label,
                stats.n,
                stats.min,
                stats.q1,
                stats.median,
                stats.q3,
                stats.max,
                stats.mean,
                stats.stdev
            );
        }
        println!();
    }
}