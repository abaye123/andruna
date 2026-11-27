//! The andruna-sysd executable. A windows-subsystem program: it never opens
//! a console.

#![windows_subsystem = "windows"]

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(andruna_sysd::main_with_args(&args));
}
