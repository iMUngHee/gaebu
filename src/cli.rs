//! Command line. Returns the process exit code.

use std::ffi::OsString;

pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    let _ = args.into_iter().count();
    eprintln!("gaebu: not implemented yet");
    2
}
