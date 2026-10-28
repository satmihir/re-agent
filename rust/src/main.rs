fn main() {
    std::process::exit(reagent::cli::main(std::env::args().skip(1).collect()));
}
