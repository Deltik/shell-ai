mod bench;
mod package;

use anyhow::{bail, Result};
use std::env;

fn main() -> Result<()> {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: xtask <command> [args...]");
        eprintln!("Commands:");
        eprintln!("  package <target> [target...]               - Package built binaries for the given targets");
        eprintln!("  bench-integration [--keep] [sample_count]  - Benchmark shell integration overhead");
        std::process::exit(1);
    }

    match args[1].as_str() {
        "package" => {
            if args.len() < 3 {
                bail!("Usage: xtask package <target> [target...]");
            }
            let targets: Vec<&str> = args[2..].iter().map(|s| s.as_str()).collect();
            package::run(&targets)
        }
        "bench-integration" => {
            let mut samples = 100;
            let mut keep_results = false;

            for arg in &args[2..] {
                if arg == "--keep" || arg == "-k" {
                    keep_results = true;
                } else if let Ok(n) = arg.parse::<usize>() {
                    samples = n;
                }
            }

            bench::run(samples, keep_results)
        }
        cmd => bail!("Unknown command: {}", cmd),
    }
}