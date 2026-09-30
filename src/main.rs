//! rg1: grep, but the pattern is a description.
//!
//! Ripgrep's engine (in-process crates) finds candidate records; the
//! laya-studio decision engine judges each candidate with a yes/no question.

mod cache;
mod cli;
mod code;
mod color;
mod data;
mod diff;
mod emit;
mod estimate;
mod gitctx;
mod inputs;
mod judge;
mod laya;
mod prefilter;
mod record;
mod search;
mod stats;

use clap::Parser;

fn main() {
    let args = cli::Args::parse();
    if let Err(e) = args.validate() {
        eprintln!("rg1: {e}");
        eprintln!("run 'rg1 --help' for usage");
        std::process::exit(2);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    let code = rt.block_on(async move {
        match judge::run(args).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("rg1: {e}");
                2
            }
        }
    });
    rt.shutdown_timeout(std::time::Duration::from_millis(100));
    std::process::exit(code);
}
