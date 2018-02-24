use std::rc::Rc;
use std::cell::Cell;
use futures::{Async, Poll};
use futures::future::ok;
use futures::stream::Stream;
use stdweb::PromiseFuture;


// TODO add in Done to allow the Signal to end ?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State<A> {
    Changed(A),
    NotChanged,
}

impl<A> State<A> {
    #[inline]
    pub fn map<B, F>(self, f: F) -> State<B> where F: FnOnce(A) -> B {
        match self {
            State::Changed(value) => State::Changed(f(value)),
            State::NotChanged => State::NotChanged,
        }
    }
}


pub trait Signal {
    type Item;

    fn poll(&mut self) -> State<Self::Item>;

    #[inline]
    fn to_stream(self) -> SignalStream<Self>
        where Self: Sized {
        SignalStream {
            signal: self,
        }
    }

    #[inline]
    fn map<A, B>(self, callback: A) -> Map<Self, A>
        where A: FnMut(Self::Item) -> B,
              Self: Sized {
        Map {
            signal: self,
            callback,
        }
    }

    #[inline]
    fn map2<A, B, C>(self, other: A, callback: B) -> Map2<Self, A, B>
        where A: Signal,
              B: FnMut(&mut Self::Item, &mut A::Item) -> C,
              Self: Sized {
        Map2 {
            signal1: self,
            signal2: other,
            callback,
            left: None,
            right: None,
        }
    }

    #[inline]
    fn map_dedupe<A, B>(self, callback: A) -> MapDedupe<Self, A>
        where A: FnMut(&mut Self::Item) -> B,
              Self: Sized {
        MapDedupe {
            old_value: None,
            signal: self,
            callback,
        }
    }

    #[inline]
    fn filter_map<A, B>(self, callback: A) -> FilterMap<Self, A>
        where A: FnMut(Self::Item) -> Option<B>,
              Self: Sized {
        FilterMap {
            signal: self,
            callback,
            first: true,
        }
    }

    #[inline]
    fn flatten(self) -> Flatten<Self>
        where Self::Item: Signal,
              Self: Sized {
        Flatten {
            signal: self,
            inner: None,
        }
    }

    #[inline]
    fn switch<A, B>(self, callback: A) -> Flatten<Map<Self, A>>
        where A: FnMut(Self::Item) -> B,
              B: Signal,
              Self: Sized {
        self.map(callback).flatten()
    }

    // TODO file Rust bug about bad error message when `callback` isn't marked as `mut`
    // TODO make this more efficient
    fn for_each<A>(self, mut callback: A) -> DropHandle
        where A: FnMut(Self::Item) + 'static,
              Self: Sized + 'static {

        let (handle, stream) = drop_handle(self.to_stream());

        PromiseFuture::spawn(
            stream.for_each(move |value| {
                callback(value);
                ok(())
            })
        );

        handle
    }

    #[inline]
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}


pub struct Always<A> {
    value: Option<A>,
}

impl<A> Signal for Always<A> {
    type Item = A;

    #[inline]
    fn poll(&mut self) -> State<Self::Item> {
        match self.value.take() {
            Some(value) => State::Changed(value),
            None => State::NotChanged,
        }
    }
}

#[inline]
pub fn always<A>(value: A) -> Always<A> {
    Always {
        value: Some(value),
    }
}


// TODO figure out a more efficient way to implement this
#[inline]
fn drop_handle<A: Stream>(stream: A) -> (DropHandle, DropStream<A>) {
    let done: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    let drop_handle = DropHandle {
        done: done.clone(),
    };

    let drop_stream = DropStream {
        done,
        stream,
    };

    (drop_handle, drop_stream)
}


// TODO rename this to something else ?
#[must_use]
pub struct DropHandle {
    done: Rc<Cell<bool>>,
}

// TODO change this to use Drop, but it requires some changes to the after_remove callback system
impl DropHandle {
    #[inline]
    pub fn stop(self) {
        self.done.set(true);
    }
}


struct DropStream<A> {
    done: Rc<Cell<bool>>,
    stream: A,
}

impl<A: Stream> Stream for DropStream<A> {
    type Item = A::Item;
    type Error = A::Error;

    #[inline]
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        if self.done.get() {
            Ok(Async::Ready(None))

        } else {
            self.stream.poll()
        }
    }
}


pub struct SignalStream<A> {
    signal: A,
}

impl<A: Signal> Stream for SignalStream<A> {
    type Item = A::Item;
    // TODO use Void instead ?
    type Error = ();

    #[inline]
    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        Ok(match self.signal.poll() {
            State::Changed(value) => Async::Ready(Some(value)),
            State::NotChanged => Async::NotReady,
        })
    }
}


pub struct Map<A, B> {
    signal: A,
    callback: B,
}

impl<A, B, C> Signal for Map<A, B>
    where A: Signal,
          B: FnMut(A::Item) -> C {
    type Item = C;

    #[inline]
    fn poll(&mut self) -> State<Self::Item> {
        self.signal.poll().map(|value| (self.callback)(value))
    }
}


pub struct Map2<A: Signal, B: Signal, C> {
    signal1: A,
    signal2: B,
    callback: C,
    left: Option<A::Item>,
    right: Option<B::Item>,
}

impl<A, B, C, D> Signal for Map2<A, B, C>
    where A: Signal,
          B: Signal,
          C: FnMut(&mut A::Item, &mut B::Item) -> D {
    type Item = D;

    // TODO inline this ?
    fn poll(&mut self) -> State<Self::Item> {
        match self.signal1.poll() {
            State::Changed(mut left) => {
                let output = match self.signal2.poll() {
                    State::Changed(mut right) => {
                        let output = State::Changed((self.callback)(&mut left, &mut right));
                        self.right = Some(right);
                        output
                    },

                    State::NotChanged => match self.right {
                        Some(ref mut right) => State::Changed((self.callback)(&mut left, right)),
                        None => State::NotChanged,
                    },
                };

                self.left = Some(left);

                output
            },

            State::NotChanged => match self.left {
                Some(ref mut left) => match self.signal2.poll() {
                    State::Changed(mut right) => {
                        let output = State::Changed((self.callback)(left, &mut right));
                        self.right = Some(right);
                        output
                    },

                    State::NotChanged => State::NotChanged,
                },

                None => State::NotChanged,
            },
        }
    }
}


pub struct MapDedupe<A: Signal, B> {
    old_value: Option<A::Item>,
    signal: A,
    callback: B,
}

impl<A, B, C> Signal for MapDedupe<A, B>
    where A: Signal,
          A::Item: PartialEq,
          // TODO should this use Fn instead ?
          B: FnMut(&A::Item) -> C {

    type Item = C;

    // TODO should this use #[inline] ?
    fn poll(&mut self) -> State<Self::Item> {
        loop {
            match self.signal.poll() {
                State::Changed(mut value) => {
                    let has_changed = match self.old_value {
                        Some(ref old_value) => *old_value != value,
                        None => true,
                    };

                    if has_changed {
                        let output = (self.callback)(&mut value);
                        self.old_value = Some(value);
                        return State::Changed(output);
                    }
                },
                State::NotChanged => return State::NotChanged,
            }
        }
    }
}


pub struct FilterMap<A, B> {
    signal: A,
    callback: B,
    first: bool,
}

impl<A, B, C> Signal for FilterMap<A, B>
    where A: Signal,
          B: FnMut(A::Item) -> Option<C> {
    type Item = Option<C>;

    // TODO should this use #[inline] ?
    #[inline]
    fn poll(&mut self) -> State<Self::Item> {
        loop {
            match self.signal.poll() {
                State::Changed(value) => match (self.callback)(value) {
                    Some(value) => {
                        self.first = false;
                        return State::Changed(Some(value));
                    },
                    None => if self.first {
                        self.first = false;
                        return State::Changed(None);
                    },
                },
                State::NotChanged => return State::NotChanged,
            }
        }
    }
}


pub struct Flatten<A: Signal> {
    signal: A,
    inner: Option<A::Item>,
}

impl<A> Signal for Flatten<A>
    where A: Signal,
          A::Item: Signal {
    type Item = <<A as Signal>::Item as Signal>::Item;

    #[inline]
    fn poll(&mut self) -> State<Self::Item> {
        match self.signal.poll() {
            State::Changed(mut inner) => {
                let poll = inner.poll();
                self.inner = Some(inner);
                poll
            },

            State::NotChanged => match self.inner {
                Some(ref mut inner) => inner.poll(),
                None => State::NotChanged,
            },
        }
    }
}


// TODO verify that this is correct
pub mod unsync {
    use super::{Signal, State};
    use std::rc::{Rc, Weak};
    use std::cell::RefCell;
    use futures::task;


    struct Inner<A> {
        value: Option<A>,
        task: Option<task::Task>,
    }


    pub struct Sender<A> {
        inner: Weak<RefCell<Inner<A>>>,
    }

    impl<A> Sender<A> {
        pub fn set(&self, value: A) -> Result<(), A> {
            if let Some(inner) = self.inner.upgrade() {
                let mut inner = inner.borrow_mut();

                inner.value = Some(value);

                if let Some(task) = inner.task.take() {
                    drop(inner);
                    task.notify();
                }

                Ok(())

            } else {
                Err(value)
            }
        }
    }


    #[derive(Clone)]
    pub struct Receiver<A> {
        inner: Rc<RefCell<Inner<A>>>,
    }

    impl<A> Signal for Receiver<A> {
        type Item = A;

        #[inline]
        fn poll(&mut self) -> State<Self::Item> {
            let mut inner = self.inner.borrow_mut();

            // TODO is this correct ?
            match inner.value.take() {
                Some(value) => State::Changed(value),
                None => {
                    inner.task = Some(task::current());
                    State::NotChanged
                },
            }
        }
    }


    pub fn mutable<A>(initial_value: A) -> (Sender<A>, Receiver<A>) {
        let inner = Rc::new(RefCell::new(Inner {
            value: Some(initial_value),
            task: None,
        }));

        let sender = Sender {
            inner: Rc::downgrade(&inner),
        };

        let receiver = Receiver {
            inner,
        };

        (sender, receiver)
    }
}


/*map! {
    let foo = 1,
    let bar = 2,
    let qux = 3 => {
        let corge = 4;
    }
}*/


/*
map!(x, y => x + y)
*/


// TODO should this be hidden from the docs ?
#[doc(hidden)]
#[inline]
pub fn pair_clone<'a, 'b, A: Clone, B: Clone>(left: &'a mut A, right: &'b mut B) -> (A, B) {
    (left.clone(), right.clone())
}


#[doc(hidden)]
#[macro_export]
macro_rules! __internal_map_clone {
    ($name:ident) => {
        ::std::clone::Clone::clone($name)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __internal_map_rc_new {
    ($value:expr) => {
        $crate::signal::Signal::map($value, ::std::rc::Rc::new)
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __internal_map2 {
    ($f:expr, $old_pair:pat, $old_expr:expr, { $($lets:stmt);* }, let $name:ident: $t:ty = $value:expr;) => {
        $crate::signal::Signal::map2(
            $old_expr,
            __internal_map_rc_new!($value),
            |&mut $old_pair, $name| {
                $($lets;)*
                let $name: $t = __internal_map_clone!($name);
                $f
            }
        )
    };
    ($f:expr, $old_pair:pat, $old_expr:expr, { $($lets:stmt);* }, let $name:ident: $t:ty = $value:expr; $($args:tt)+) => {
        __internal_map2!(
            $f,
            ($old_pair, ref mut $name),
            $crate::signal::Signal::map2(
                $old_expr,
                __internal_map_rc_new!($value),
                $crate::signal::pair_clone
            ),
            { $($lets;)* let $name: $t = __internal_map_clone!($name) },
            $($args)+
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __internal_map {
    ($f:expr, let $name:ident: $t:ty = $value:expr;) => {
        $crate::signal::Signal::map($value, |$name| {
            let $name: $t = ::std::rc::Rc::new($name);
            $f
        })
    };
    ($f:expr, let $name1:ident: $t1:ty = $value1:expr;
              let $name2:ident: $t2:ty = $value2:expr;) => {
        $crate::signal::Signal::map2(
            __internal_map_rc_new!($value1),
            __internal_map_rc_new!($value2),
            |$name1, $name2| {
                let $name1: $t1 = __internal_map_clone!($name1);
                let $name2: $t2 = __internal_map_clone!($name2);
                $f
            }
        )
    };
    ($f:expr, let $name:ident: $t:ty = $value:expr; $($args:tt)+) => {
        __internal_map2!(
            $f,
            ref mut $name,
            __internal_map_rc_new!($value),
            { let $name: $t = __internal_map_clone!($name) },
            $($args)+
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __internal_map_lets {
    ($f:expr, { $($lets:tt)* },) => {
        __internal_map!($f, $($lets)*)
    };
    ($f:expr, { $($lets:tt)* }, let $name:ident: $t:ty = $value:expr, $($args:tt)*) => {
        __internal_map_lets!($f, { $($lets)* let $name: $t = $value; }, $($args)*)
    };
    ($f:expr, { $($lets:tt)* }, let $name:ident = $value:expr, $($args:tt)*) => {
        __internal_map_lets!($f, { $($lets)* let $name: ::std::rc::Rc<_> = $value; }, $($args)*)
    };
    ($f:expr, { $($lets:tt)* }, $name:ident, $($args:tt)*) => {
        __internal_map_lets!($f, { $($lets)* let $name: ::std::rc::Rc<_> = $name; }, $($args)*)
    };
}

// TODO this is pretty inefficient, it iterates over the token tree one token at a time
#[doc(hidden)]
#[macro_export]
macro_rules! __internal_map_split {
    (($($before:tt)*), => $f:expr) => {
        __internal_map_lets!($f, {}, $($before)*,)
    };
    (($($before:tt)*), $t:tt $($after:tt)*) => {
        __internal_map_split!(($($before)* $t), $($after)*)
    };
}

#[macro_export]
macro_rules! map_rc {
    ($($input:tt)*) => { __internal_map_split!((), $($input)*) };
}


#[cfg(test)]
mod tests {
    #[test]
    fn map_macro_ident_1() {
        let a = super::always(1);

        let mut s = map_rc!(a => {
            let a: ::std::rc::Rc<u32> = a;
            *a + 1
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(2));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_ident_2() {
        let a = super::always(1);
        let b = super::always(2);

        let mut s = map_rc!(a, b => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            *a + *b
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(3));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_ident_3() {
        let a = super::always(1);
        let b = super::always(2);
        let c = super::always(3);

        let mut s = map_rc!(a, b, c => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            let c: ::std::rc::Rc<u32> = c;
            *a + *b + *c
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(6));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_ident_4() {
        let a = super::always(1);
        let b = super::always(2);
        let c = super::always(3);
        let d = super::always(4);

        let mut s = map_rc!(a, b, c, d => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            let c: ::std::rc::Rc<u32> = c;
            let d: ::std::rc::Rc<u32> = d;
            *a + *b + *c + *d
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(10));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_ident_5() {
        let a = super::always(1);
        let b = super::always(2);
        let c = super::always(3);
        let d = super::always(4);
        let e = super::always(5);

        let mut s = map_rc!(a, b, c, d, e => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            let c: ::std::rc::Rc<u32> = c;
            let d: ::std::rc::Rc<u32> = d;
            let e: ::std::rc::Rc<u32> = e;
            *a + *b + *c + *d + *e
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(15));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }


    #[test]
    fn map_macro_let_1() {
        let a2 = super::always(1);

        let mut s = map_rc!(let a = a2 => {
            let a: ::std::rc::Rc<u32> = a;
            *a + 1
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(2));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_2() {
        let a2 = super::always(1);
        let b2 = super::always(2);

        let mut s = map_rc!(let a = a2, let b = b2 => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            *a + *b
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(3));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_3() {
        let a2 = super::always(1);
        let b2 = super::always(2);
        let c2 = super::always(3);

        let mut s = map_rc!(let a = a2, let b = b2, let c = c2 => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            let c: ::std::rc::Rc<u32> = c;
            *a + *b + *c
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(6));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_4() {
        let a2 = super::always(1);
        let b2 = super::always(2);
        let c2 = super::always(3);
        let d2 = super::always(4);

        let mut s = map_rc!(let a = a2, let b = b2, let c = c2, let d = d2 => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            let c: ::std::rc::Rc<u32> = c;
            let d: ::std::rc::Rc<u32> = d;
            *a + *b + *c + *d
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(10));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_5() {
        let a2 = super::always(1);
        let b2 = super::always(2);
        let c2 = super::always(3);
        let d2 = super::always(4);
        let e2 = super::always(5);

        let mut s = map_rc!(let a = a2, let b = b2, let c = c2, let d = d2, let e = e2 => {
            let a: ::std::rc::Rc<u32> = a;
            let b: ::std::rc::Rc<u32> = b;
            let c: ::std::rc::Rc<u32> = c;
            let d: ::std::rc::Rc<u32> = d;
            let e: ::std::rc::Rc<u32> = e;
            *a + *b + *c + *d + *e
        });

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(15));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }


    #[test]
    fn map_macro_let_type_1() {
        let a2 = super::always(1);

        let mut s = map_rc! {
            let a: ::std::rc::Rc<u32> = a2 => {
                let a: ::std::rc::Rc<u32> = a;
                *a + 1
            }
        };

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(2));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_type_2() {
        let a2 = super::always(1);
        let b2 = super::always(2);

        let mut s = map_rc! {
            let a: ::std::rc::Rc<u32> = a2,
            let b: ::std::rc::Rc<u32> = b2 => {
                let a: ::std::rc::Rc<u32> = a;
                let b: ::std::rc::Rc<u32> = b;
                *a + *b
            }
        };

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(3));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_type_3() {
        let a2 = super::always(1);
        let b2 = super::always(2);
        let c2 = super::always(3);

        let mut s = map_rc! {
            let a: ::std::rc::Rc<u32> = a2,
            let b: ::std::rc::Rc<u32> = b2,
            let c: ::std::rc::Rc<u32> = c2 => {
                let a: ::std::rc::Rc<u32> = a;
                let b: ::std::rc::Rc<u32> = b;
                let c: ::std::rc::Rc<u32> = c;
                *a + *b + *c
            }
        };

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(6));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_type_4() {
        let a2 = super::always(1);
        let b2 = super::always(2);
        let c2 = super::always(3);
        let d2 = super::always(4);

        let mut s = map_rc! {
            let a: ::std::rc::Rc<u32> = a2,
            let b: ::std::rc::Rc<u32> = b2,
            let c: ::std::rc::Rc<u32> = c2,
            let d: ::std::rc::Rc<u32> = d2 => {
                let a: ::std::rc::Rc<u32> = a;
                let b: ::std::rc::Rc<u32> = b;
                let c: ::std::rc::Rc<u32> = c;
                let d: ::std::rc::Rc<u32> = d;
                *a + *b + *c + *d
            }
        };

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(10));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }

    #[test]
    fn map_macro_let_type_5() {
        let a2 = super::always(1);
        let b2 = super::always(2);
        let c2 = super::always(3);
        let d2 = super::always(4);
        let e2 = super::always(5);

        let mut s = map_rc! {
            let a: ::std::rc::Rc<u32> = a2,
            let b: ::std::rc::Rc<u32> = b2,
            let c: ::std::rc::Rc<u32> = c2,
            let d: ::std::rc::Rc<u32> = d2,
            let e: ::std::rc::Rc<u32> = e2 => {
                let a: ::std::rc::Rc<u32> = a;
                let b: ::std::rc::Rc<u32> = b;
                let c: ::std::rc::Rc<u32> = c;
                let d: ::std::rc::Rc<u32> = d;
                let e: ::std::rc::Rc<u32> = e;
                *a + *b + *c + *d + *e
            }
        };

        assert_eq!(super::Signal::poll(&mut s), super::State::Changed(15));
        assert_eq!(super::Signal::poll(&mut s), super::State::NotChanged);
    }
}
