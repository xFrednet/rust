// issue: 113903

#![cfg_attr(bootstrap, feature(lint_reasons))]

use std::ops::Deref;

pub trait Tr {
    fn w() -> impl Deref<Target = Missing<impl Sized>>;
    //~^ ERROR cannot find type `Missing` in this scope
}

impl Tr for () {
    #[expect(refining_impl_trait)]
    fn w() -> &'static () {
        &()
    }
}

fn main() {}
