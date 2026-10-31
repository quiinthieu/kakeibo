use clap::Parser;
use kakeibo_core::product;

#[derive(Parser, Debug)]
#[command(
    name = product::BINARY_NAME,
    version = product::VERSION,
    about = "Token budget diary for your coding agents",
    long_about = None,
)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
