// check-pass
// compile-flags: -Dunused_attributes

#![deny(unused_crate_dependencies)]
#![cfg_attr(bootstrap, feature(lint_reasons))]

fn main() {}
