//! `rec-host`: the command line of [`frontend`], without extensions.

fn main() -> std::process::ExitCode {
    frontend::main_with(None)
}
