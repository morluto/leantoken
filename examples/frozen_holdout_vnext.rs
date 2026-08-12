#[path = "support/frozen_holdout_vnext.rs"]
mod frozen_holdout_vnext;

fn main() {
    if let Err(error) = frozen_holdout_vnext::run() {
        eprintln!("frozen holdout vNext failed: {error}");
        std::process::exit(1);
    }
}
