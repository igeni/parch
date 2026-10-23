mod args;
mod commands;
mod registry;

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;
use parch_core::report::Reporter;

fn main() -> ExitCode {
    let cli = args::Cli::parse_from(args::preprocess(std::env::args_os()));
    let g = &cli.global;
    let progress = !g.no_progress && (g.progress || std::io::stderr().is_terminal()) && !matches!(cli.cmd, args::Command::List(_));
    let report = Reporter::new(g.quiet, g.verbose, progress);

    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let _ = ctrlc::set_handler(move || {
        if flag.swap(true, Ordering::SeqCst) {
            std::process::exit(130);
        }
    });

    let result = commands::run(cli, &report, &cancel);
    report.finish();
    match result {
        Ok(()) if cancel.load(Ordering::SeqCst) => ExitCode::from(130),
        Ok(()) => ExitCode::from(report.exit_code()),
        Err(e) => {
            if matches!(e.downcast_ref::<parch_core::Error>(), Some(parch_core::Error::Cancelled)) {
                eprintln!("parch: interrupted");
                return ExitCode::from(130);
            }
            eprintln!("parch: error: {e:#}");
            ExitCode::from(2)
        }
    }
}
