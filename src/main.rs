use std::io::{ErrorKind, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    match agentgrep::run_cli() {
        Ok(result) => {
            let mut stdout = std::io::stdout().lock();
            let mut stderr = std::io::stderr().lock();

            if let Err(error) = stdout.write_all(&result.stdout) {
                if error.kind() == ErrorKind::BrokenPipe {
                    return ExitCode::SUCCESS;
                }
                eprintln!("agentgrep: failed to write stdout: {error}");
                return ExitCode::from(1);
            }
            if let Err(error) = stderr.write_all(&result.stderr) {
                if error.kind() == ErrorKind::BrokenPipe {
                    return ExitCode::SUCCESS;
                }
                eprintln!("agentgrep: failed to write stderr: {error}");
                return ExitCode::from(1);
            }

            ExitCode::from(result.exit_code as u8)
        }
        Err(error) => {
            eprintln!("agentgrep: {error:#}");
            ExitCode::from(2)
        }
    }
}
