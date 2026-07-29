fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let Some(identity) = arguments.first() else {
        eprintln!("usage: fixture-runner <domain>/<case> [--bless]");
        std::process::exit(2);
    };
    let bless = match arguments.as_slice() {
        [_] => false,
        [_, flag] if flag == "--bless" => true,
        _ => {
            eprintln!("usage: fixture-runner <domain>/<case> [--bless]");
            std::process::exit(2);
        }
    };
    if let Err(error) = leantoken_test_suite::run_fixture(identity, bless) {
        eprintln!("fixture failed: {error}");
        std::process::exit(1);
    }
}
