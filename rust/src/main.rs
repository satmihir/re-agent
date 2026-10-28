fn main() {
    std::process::exit(reagent::cli::main_os(std::env::args_os().skip(1).collect()));
}
