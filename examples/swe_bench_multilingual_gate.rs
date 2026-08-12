#[path = "support/swe_bench_gate.rs"]
mod swe_bench_gate;

fn main() {
    if let Err(error) = swe_bench_gate::run() {
        eprintln!("swe_bench_multilingual_gate failed: {error}");
        std::process::exit(1);
    }
}
