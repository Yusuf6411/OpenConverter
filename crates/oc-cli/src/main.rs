fn main() -> std::process::ExitCode {
    oc_cli::run(std::env::args().skip(1).collect())
}
