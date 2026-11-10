use std::{ffi::OsStr, io, path::Path, process::ExitCode};

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args == [OsStr::new("--help")] {
        eprintln!(
            "Usage: mtp-worker --backend fake | --backend kalam --library /absolute/kalam.dylib\nBoth backends are currently read-only. Kalam never loads in the UI process."
        );
        return Ok(());
    }
    match args.as_slice() {
        [flag, backend] if flag == "--backend" && backend == "fake" => {
            mtp_worker::serve(&mut io::stdin().lock(), &mut io::stdout().lock())?;
        }
        [flag, backend, library, path]
            if flag == "--backend"
                && backend == "kalam"
                && library == "--library"
                && Path::new(path).is_absolute() =>
        {
            // Native Go/C stdout cannot corrupt the framed control transport.
            let mut protocol = mtp_backend_kalam::isolate_native_stdout()?;
            mtp_worker::kalam::serve_kalam(
                &mut io::stdin().lock(),
                &mut protocol,
                Path::new(path),
            )?;
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "explicit backend and absolute Kalam library path required",
            )
            .into());
        }
    }
    Ok(())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("worker terminated: {error}");
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::InvalidInput)
            {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}
