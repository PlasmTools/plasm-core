use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(super) trait Structural {
    fn visit<'a>(&'a self, walk: &mut Walk<'a, '_>);
}

/// An explicit stack keeps recursive DAGs and operands off the Rust call stack.
pub(super) struct Walk<'a, 'h> {
    digest: &'h mut Sha256,
    pending: Vec<&'a dyn Structural>,
}

pub(super) fn hash(value: &dyn Structural, digest: &mut Sha256) {
    let mut walk = Walk {
        digest,
        pending: vec![value],
    };
    while let Some(value) = walk.pending.pop() {
        value.visit(&mut walk);
    }
}

impl<'a> Walk<'a, '_> {
    pub fn bytes(&mut self, bytes: &[u8]) {
        self.digest.update((bytes.len() as u64).to_le_bytes());
        self.digest.update(bytes);
    }
    pub fn fields(&mut self, tag: &str, fields: &[&'a dyn Structural]) {
        self.bytes(tag.as_bytes());
        self.digest.update((fields.len() as u64).to_le_bytes());
        self.pending.extend(fields.iter().rev().copied());
    }
    pub fn nested(&mut self, value: &dyn Structural) {
        hash(value, self.digest);
    }
}

macro_rules! record {
    ($ty:ident { $($field:ident),* $(,)? }) => {
        impl Structural for $ty {
            fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
                let Self { $($field),* } = self;
                w.fields(stringify!($ty), &[$($field),*]);
            }
        }
    };
}
macro_rules! variants {
    ($ty:ident { $($variant:ident $(($item:ident))? $({$($field:ident),*})?),* $(,)? }) => {
        impl Structural for $ty {
            fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
                match self {
                    $(Self::$variant $(($item))? $({$($field),*})? =>
                        w.fields(concat!(stringify!($ty), "::", stringify!($variant)), &[$($item,)? $($($field,)*)?]),)*
                }
            }
        }
    };
}
pub(super) use {record, variants};

impl Structural for String {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(self.as_bytes());
    }
}
macro_rules! integer {
    ($($ty:ty),*) => {$(impl Structural for $ty {
        fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) { w.bytes(&self.to_le_bytes()); }
    })*};
}
integer!(u8, u32, u64, i64);
impl Structural for usize {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(&(*self as u64).to_le_bytes());
    }
}
impl Structural for bool {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(&[u8::from(*self)]);
    }
}
impl Structural for f64 {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        // Equality treats both zero signs alike; every other bit remains significant.
        w.bytes(&if *self == 0.0 { 0_u64 } else { self.to_bits() }.to_le_bytes());
    }
}
impl Structural for std::num::NonZeroU32 {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.nested(&self.get());
    }
}
impl<T: Structural> Structural for Box<T> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.pending.push(self.as_ref());
    }
}
impl<T: Structural> Structural for Option<T> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        match self {
            None => w.fields("None", &[]),
            Some(value) => w.fields("Some", &[value]),
        }
    }
}
impl<T: Structural> Structural for Vec<T> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(b"Sequence");
        w.nested(&self.len());
        w.pending
            .extend(self.iter().rev().map(|v| v as &dyn Structural));
    }
}
impl<T: Structural + Ord> Structural for BTreeSet<T> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(b"Set");
        w.nested(&self.len());
        w.pending
            .extend(self.iter().rev().map(|v| v as &dyn Structural));
    }
}
impl<K: Structural + Ord, V: Structural> Structural for BTreeMap<K, V> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(b"Map");
        w.nested(&self.len());
        for (key, value) in self.iter().rev() {
            w.pending.push(value);
            w.pending.push(key);
        }
    }
}
impl<K: Structural + Ord, V: Structural> Structural for indexmap::IndexMap<K, V> {
    fn visit<'a>(&'a self, w: &mut Walk<'a, '_>) {
        w.bytes(b"Map");
        w.nested(&self.len());
        let mut entries: Vec<_> = self.iter().collect();
        entries.sort_unstable_by_key(|(key, _)| *key);
        for (key, value) in entries.into_iter().rev() {
            w.pending.push(value);
            w.pending.push(key);
        }
    }
}
