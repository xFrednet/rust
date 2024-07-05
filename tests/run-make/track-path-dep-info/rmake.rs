// This test checks the functionality of `tracked_path::path`, a procedural macro
// feature that adds a dependency to another file inside the procmacro. In this case,
// the text file is added through this method, and the test checks that the compilation
// output successfully added the file as a dependency.
// See https://github.com/rust-lang/rust/pull/84029

//FIXME(Oneirical): Try it on musl

use run_make_support::{bare_rustc, fs_wrapper, rustc};

fn main() {
    bare_rustc().input("macro_def.rs").run();
    rustc().env("EXISTING_PROC_MACRO_ENV", "1").emit("dep-info").input("macro_use.rs").run();
    assert!(fs_wrapper::read_to_string("macro_use.d").contains("emojis.txt:"));
}
