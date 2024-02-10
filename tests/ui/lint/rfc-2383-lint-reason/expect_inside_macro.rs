// check-pass

#![cfg_attr(bootstrap, feature(lint_reasons))]

#![warn(unused)]

macro_rules! expect_inside_macro {
    () => {
        #[expect(unused_variables)]
        let x = 0;
    };
}

fn main() {
    expect_inside_macro!();
}
