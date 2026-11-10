use std::{io, process::ExitCode};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] {
        eprintln!(
            "Usage: mtp-worker --backend fake\nOnly the simulated read-only backend is currently implemented."
        );
        return ExitCode::SUCCESS;
    }
    if args != ["--backend", "fake"] {
        eprintln!("explicit --backend fake is required; Kalam and real transfers are not enabled");
        return ExitCode::from(2);
    }
    match mtp_worker::serve(&mut io::stdin().lock(), &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("worker transport terminated: {error}");
            ExitCode::from(1)
        }
    }
}
