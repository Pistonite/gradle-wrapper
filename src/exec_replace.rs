//! Replace the current process with a child, as closely as each platform allows.
//!
//! Ported from https://github.com/Pistonite/shaft (packages/shaftim/src/lib.rs),
//! which in turn borrows from cargo-util.
//!
// Reference
// https://github.com/rust-lang/cargo/blob/master/crates/cargo-util/src/process_builder.rs

pub use imp::exec_replace;

#[cfg(unix)]
mod imp {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, ExitCode};

    /// `execvp`: the current process is replaced, so exit status and signal
    /// handling pass through to the caller for free. Only returns on failure.
    #[inline(always)]
    pub fn exec_replace(mut command: Command) -> ExitCode {
        let error = command.exec();
        eprintln!("execvp failed: {error}");
        ExitCode::from(255)
    }
}

#[cfg(windows)]
mod imp {
    use std::process::{Command, ExitCode};

    use windows_sys::Win32::Foundation::{FALSE, TRUE};
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
    use windows_sys::core::BOOL;

    /// Windows has no `exec`, so spawn and wait instead. The console control
    /// handler is installed so Ctrl-C reaches the child rather than killing us
    /// first and orphaning it.
    #[inline(always)]
    pub fn exec_replace(mut command: Command) -> ExitCode {
        let success = unsafe { SetConsoleCtrlHandler(Some(ctrlc_handler), TRUE) };
        if success == FALSE {
            eprintln!("execvp: failed to set ctrl-c handler");
            return ExitCode::from(254);
        }
        let mut child = match command.spawn() {
            Ok(x) => x,
            Err(_) => {
                eprintln!("execvp failed: spawn failed");
                return ExitCode::from(255);
            }
        };
        let exit_status = match child.wait() {
            Ok(x) => x,
            Err(_) => {
                eprintln!("execvp failed: wait failed");
                return ExitCode::from(253);
            }
        };
        let code = exit_status.code().unwrap_or(255) as u8;
        ExitCode::from(code)
    }

    unsafe extern "system" fn ctrlc_handler(_: u32) -> BOOL {
        TRUE
    }
}
