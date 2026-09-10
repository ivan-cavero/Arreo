//! `arreo` CLI binary. Real verbs (`attach`, `metrics`, …) land in T-0005.

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("arreo {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    println!("arreo: not implemented (CLI lands in T-0005)");
}
