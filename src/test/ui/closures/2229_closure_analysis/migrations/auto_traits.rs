// run-rustfix
#![deny(rust_2021_incompatible_closure_captures)]

use std::thread;

/* Test Send Trait Migration */
struct SendPointer(*mut i32);
unsafe impl Send for SendPointer {}

fn test_send_trait() {
    let mut f = 10;
    let fptr = SendPointer(&mut f as *mut i32);
    thread::spawn(move || unsafe {
        //~^ ERROR: `Send` trait implementation
        //~| HELP: add a dummy let to cause `fptr` to be fully captured
        *fptr.0 = 20;
    });
}

/* Test Sync Trait Migration */
struct CustomInt(*mut i32);
struct SyncPointer(CustomInt);
unsafe impl Sync for SyncPointer {}
unsafe impl Send for CustomInt {}

fn test_sync_trait() {
    let mut f = 10;
    let f = CustomInt(&mut f as *mut i32);
    let fptr = SyncPointer(f);
    thread::spawn(move || unsafe {
        //~^ ERROR: `Sync`, `Send` trait implementation
        //~| HELP: add a dummy let to cause `fptr` to be fully captured
        *fptr.0.0 = 20;
    });
}

/* Test Clone Trait Migration */
struct S(String);
struct T(i32);

struct U(S, T);

impl Clone for U {
    fn clone(&self) -> Self {
        U(S(String::from("Hello World")), T(0))
    }
}

fn test_clone_trait() {
    let f = U(S(String::from("Hello World")), T(0));
    let c = || {
        //~^ ERROR: `Clone` trait implementation, and drop order
        //~| HELP: add a dummy let to cause `f` to be fully captured
        let f_1 = f.1;
        println!("{:?}", f_1.0);
    };

    let c_clone = c.clone();

    c_clone();
}

fn main() {
    test_send_trait();
    test_sync_trait();
    test_clone_trait();
}
