//! Shared immutable payload storage with ordered occurrence references.
use serde::{ser::SerializeSeq, Serialize, Serializer};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
struct Occurrence {
    source: usize,
    row: usize,
}

/// A row sequence owns references to immutable batches. Selection and concatenation
/// copy occurrence coordinates and share batches; they never clone row payloads.
#[derive(Debug)]
pub struct SharedRows<R> {
    sources: Arc<[Arc<[R]>]>,
    order: Arc<[Occurrence]>,
}
impl<R> Default for SharedRows<R> {
    fn default() -> Self {
        Vec::new().into()
    }
}
impl<R> Clone for SharedRows<R> {
    fn clone(&self) -> Self {
        Self {
            sources: self.sources.clone(),
            order: self.order.clone(),
        }
    }
}
impl<R: PartialEq> PartialEq for SharedRows<R> {
    fn eq(&self, other: &Self) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<R: Eq> Eq for SharedRows<R> {}
impl<R: Serialize> Serialize for SharedRows<R> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.len()))?;
        for row in self {
            seq.serialize_element(row)?;
        }
        seq.end()
    }
}
impl<R> From<Vec<R>> for SharedRows<R> {
    fn from(rows: Vec<R>) -> Self {
        Self::from(Arc::<[R]>::from(rows))
    }
}
impl<R> From<Arc<[R]>> for SharedRows<R> {
    fn from(rows: Arc<[R]>) -> Self {
        let order = (0..rows.len())
            .map(|row| Occurrence { source: 0, row })
            .collect();
        Self {
            sources: Arc::from([rows]),
            order,
        }
    }
}
impl<R> SharedRows<R> {
    pub fn len(&self) -> usize {
        self.order.len()
    }
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
    pub fn iter(&self) -> RowIter<'_, R> {
        RowIter {
            sources: &self.sources,
            order: self.order.iter(),
        }
    }
    pub fn get(&self, index: usize) -> Option<&R> {
        self.order
            .get(index)
            .map(|position| &self.sources[position.source][position.row])
    }
    pub fn contains(&self, value: &R) -> bool
    where
        R: PartialEq,
    {
        self.iter().any(|row| row == value)
    }
    pub fn first(&self) -> Option<&R> {
        self.get(0)
    }
    pub fn select(
        &self,
        positions: impl IntoIterator<Item = usize>,
    ) -> Result<Self, super::CollectionFault> {
        let order = positions
            .into_iter()
            .map(|position| {
                self.order
                    .get(position)
                    .copied()
                    .ok_or(super::CollectionFault::Conservation)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if order.is_empty() {
            return Ok(Vec::new().into());
        }
        if self.sources.len() == 1 {
            return Ok(Self {
                sources: self.sources.clone(),
                order: order.into(),
            });
        }
        // A subset of a concatenation must not pin unrelated batches.
        let mut remap = std::collections::HashMap::new();
        let mut sources = Vec::new();
        let order: Arc<[Occurrence]> = order
            .into_iter()
            .map(|p| {
                let source = *remap.entry(p.source).or_insert_with(|| {
                    let index = sources.len();
                    sources.push(self.sources[p.source].clone());
                    index
                });
                Occurrence { source, row: p.row }
            })
            .collect();
        Ok(Self {
            sources: sources.into(),
            order,
        })
    }
    pub fn concat<'a>(inputs: impl IntoIterator<Item = &'a Self>) -> Self
    where
        R: 'a,
    {
        let mut sources = Vec::new();
        let mut order = Vec::new();
        // Pointer keys identify immutable backing allocations, not row equality.
        // Repeated occurrences still get separate coordinates in `order`.
        let mut batches = std::collections::HashMap::new();
        for input in inputs {
            if input.is_empty() {
                continue;
            }
            let remap: Vec<usize> = input
                .sources
                .iter()
                .map(|batch| {
                    *batches.entry(Arc::as_ptr(batch)).or_insert_with(|| {
                        let index = sources.len();
                        sources.push(batch.clone());
                        index
                    })
                })
                .collect();
            order.extend(input.order.iter().map(|p| Occurrence {
                source: remap[p.source],
                row: p.row,
            }));
        }
        Self {
            sources: sources.into(),
            order: order.into(),
        }
    }
}
impl<R> std::ops::Index<usize> for SharedRows<R> {
    type Output = R;
    fn index(&self, index: usize) -> &R {
        let position = self.order[index];
        &self.sources[position.source][position.row]
    }
}

pub struct RowIter<'a, R> {
    sources: &'a [Arc<[R]>],
    order: std::slice::Iter<'a, Occurrence>,
}
impl<'a, R> Iterator for RowIter<'a, R> {
    type Item = &'a R;
    fn next(&mut self) -> Option<Self::Item> {
        self.order.next().map(|p| &self.sources[p.source][p.row])
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.order.size_hint()
    }
}
impl<R> DoubleEndedIterator for RowIter<'_, R> {
    fn next_back(&mut self) -> Option<Self::Item> {
        self.order
            .next_back()
            .map(|p| &self.sources[p.source][p.row])
    }
}
impl<R> ExactSizeIterator for RowIter<'_, R> {}
impl<R> std::iter::FusedIterator for RowIter<'_, R> {}
impl<'a, R> IntoIterator for &'a SharedRows<R> {
    type Item = &'a R;
    type IntoIter = RowIter<'a, R>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<R: PartialEq> PartialEq<[R]> for SharedRows<R> {
    fn eq(&self, other: &[R]) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<R: PartialEq> PartialEq<Vec<R>> for SharedRows<R> {
    fn eq(&self, other: &Vec<R>) -> bool {
        self.iter().eq(other.iter())
    }
}
impl<R: PartialEq, const N: usize> PartialEq<[R; N]> for SharedRows<R> {
    fn eq(&self, other: &[R; N]) -> bool {
        self.iter().eq(other.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Tracked(Arc<AtomicUsize>);
    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn shared_lifetime_releases_payload_once() {
        let drops = Arc::new(AtomicUsize::new(0));
        let source = SharedRows::from(vec![Tracked(drops.clone()), Tracked(drops.clone())]);
        let selected = source.select([1]).unwrap();
        let joined = SharedRows::concat([&selected, &selected]);
        drop(source);
        drop(selected);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(std::ptr::eq(&joined[0], &joined[1]));
        drop(joined);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn empty_selection_does_not_pin_payload_storage() {
        let drops = Arc::new(AtomicUsize::new(0));
        let source = SharedRows::from(vec![Tracked(drops.clone())]);
        let empty = source.select([]).unwrap();
        drop(source);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(empty.is_empty());
    }

    #[test]
    fn clone_shares_both_coordinate_and_payload_storage() {
        let source = SharedRows::from(vec![String::from("payload")]);
        let copied = source.clone();
        assert!(Arc::ptr_eq(&source.sources, &copied.sources));
        assert!(Arc::ptr_eq(&source.order, &copied.order));
        std::thread::scope(|scope| {
            scope.spawn(|| assert!(std::ptr::eq(&source[0], &copied[0])));
        });
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    #[test]
    fn repeated_concatenation_deduplicates_storage_but_not_occurrences() {
        let source = SharedRows::from(vec![String::from("payload")]);
        let mut sequence = source.clone();
        for _ in 0..8 {
            sequence = SharedRows::concat([&sequence, &sequence]);
        }
        assert_eq!(sequence.len(), 256);
        assert_eq!(sequence.sources.len(), 1);
        assert!(sequence.iter().all(|row| std::ptr::eq(row, &source[0])));
    }

    #[test]
    fn selection_releases_unreferenced_batches() {
        let first = SharedRows::from(vec![String::from("first")]);
        let second = SharedRows::from(vec![String::from("second")]);
        let first_batch = Arc::downgrade(&first.sources[0]);
        let second_batch = Arc::downgrade(&second.sources[0]);
        let joined = SharedRows::concat([&first, &second]);
        let selected = joined.select([1]).unwrap();
        drop(first);
        drop(second);
        drop(joined);
        assert!(first_batch.upgrade().is_none());
        assert!(second_batch.upgrade().is_some());
        assert_eq!(selected[0], "second");
        drop(selected);
        assert!(second_batch.upgrade().is_none());
    }
}

impl<'de, R: serde::Deserialize<'de>> serde::Deserialize<'de> for SharedRows<R> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        <Vec<R> as serde::Deserialize>::deserialize(deserializer).map(Self::from)
    }
}
