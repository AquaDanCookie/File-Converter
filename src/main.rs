mod cli;
mod engine;
mod gui;
mod matrix;
mod presets;
mod probe;

fn main() {
    std::process::exit(cli::main());
}
