//! Visible outgoing edges ordered by immutable identity after the fractional key.
//! Adapted from the existing Tree child B-tree; cached lengths provide rank/select.
use std::{cmp::Ordering, ops::Range, sync::Arc};

use generic_btree::{
    rle::{CanRemove, HasLength, Mergeable, Sliceable, TryInsert},
    BTree, BTreeTrait, Cursor, FindResult, LeafIndex, LengthFinder, Query, UseLengthFinder,
};
use loro_common::GraphEdgeId;
use rustc_hash::FxHashMap;

use crate::container::graph::GraphPosition;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EdgePosition {
    position: GraphPosition,
    id: GraphEdgeId,
}

struct OrderTreeTrait;
#[derive(Debug, Clone)]
pub(crate) struct OrderIndex {
    tree: BTree<OrderTreeTrait>,
    id_to_leaf_index: FxHashMap<GraphEdgeId, LeafIndex>,
}

impl OrderIndex {
    pub(crate) fn new() -> Self {
        Self {
            tree: BTree::new(),
            id_to_leaf_index: FxHashMap::default(),
        }
    }

    pub(crate) fn insert(&mut self, position: GraphPosition, id: GraphEdgeId) {
        self.remove(id);
        let pos = EdgePosition { position, id };
        let (c, _) = self.tree.insert::<KeyQuery>(
            &pos,
            Elem {
                pos: Arc::new(pos.clone()),
                id,
            },
        );

        self.id_to_leaf_index.insert(id, c.leaf);
    }

    pub(crate) fn remove(&mut self, id: GraphEdgeId) {
        if let Some(leaf) = self.id_to_leaf_index.remove(&id) {
            self.tree.remove_leaf(Cursor { leaf, offset: 0 });
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = GraphEdgeId> + '_ {
        self.tree.iter().map(|x| x.id)
    }

    pub(crate) fn len(&self) -> usize {
        self.tree.root_cache().len
    }

    fn get_elem_at(&self, pos: usize) -> Option<&Elem> {
        let result = self.tree.query::<LengthFinder>(&pos)?;
        if !result.found {
            return None;
        }
        self.tree.get_elem(result.leaf())
    }

    pub(crate) fn rank(&self, id: GraphEdgeId) -> Option<usize> {
        let leaf_index = self.id_to_leaf_index.get(&id)?;
        let mut ans = 0;
        self.tree.visit_previous_caches(
            Cursor {
                leaf: *leaf_index,
                offset: 0,
            },
            |prev| match prev {
                generic_btree::PreviousCache::NodeCache(c) => {
                    ans += c.len;
                }
                generic_btree::PreviousCache::PrevSiblingElem(_) => {
                    ans += 1;
                }
                generic_btree::PreviousCache::ThisElemAndOffset { .. } => {}
            },
        );

        Some(ans)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Elem {
    pub(crate) pos: Arc<EdgePosition>,
    pub(crate) id: GraphEdgeId,
}

impl Mergeable for Elem {
    fn can_merge(&self, _rhs: &Self) -> bool {
        false
    }

    fn merge_right(&mut self, _rhs: &Self) {
        unreachable!()
    }

    fn merge_left(&mut self, _left: &Self) {
        unreachable!()
    }
}

impl HasLength for Elem {
    fn rle_len(&self) -> usize {
        1
    }
}

impl Sliceable for Elem {
    fn _slice(&self, range: std::ops::Range<usize>) -> Self {
        assert!(range.len() == 1);
        self.clone()
    }
}

impl CanRemove for Elem {
    fn can_remove(&self) -> bool {
        false
    }
}

impl TryInsert for Elem {
    fn try_insert(&mut self, _pos: usize, elem: Self) -> Result<(), Self>
    where
        Self: Sized,
    {
        Err(elem)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Cache {
    range: Option<Range<Arc<EdgePosition>>>,
    len: usize,
}

impl BTreeTrait for OrderTreeTrait {
    type Elem = Elem;
    type Cache = Cache;
    type CacheDiff = ();
    const USE_DIFF: bool = false;

    fn calc_cache_internal(
        cache: &mut Self::Cache,
        caches: &[generic_btree::Child<Self>],
    ) -> Self::CacheDiff {
        if caches.is_empty() {
            *cache = Default::default();
            return;
        }

        *cache = Cache {
            range: Some(
                caches[0].cache.range.as_ref().unwrap().start.clone()
                    ..caches
                        .last()
                        .unwrap()
                        .cache
                        .range
                        .as_ref()
                        .unwrap()
                        .end
                        .clone(),
            ),
            len: caches.iter().map(|x| x.cache.len).sum(),
        };
    }

    fn apply_cache_diff(_cache: &mut Self::Cache, _diff: &Self::CacheDiff) {
        unreachable!()
    }

    fn merge_cache_diff(_diff1: &mut Self::CacheDiff, _diff2: &Self::CacheDiff) {}

    fn get_elem_cache(elem: &Self::Elem) -> Self::Cache {
        Cache {
            range: Some(elem.pos.clone()..elem.pos.clone()),
            len: 1,
        }
    }

    fn new_cache_to_diff(_cache: &Self::Cache) -> Self::CacheDiff {}

    fn sub_cache(_cache_lhs: &Self::Cache, _cache_rhs: &Self::Cache) -> Self::CacheDiff {}
}

struct KeyQuery;

impl Query<OrderTreeTrait> for KeyQuery {
    type QueryArg = EdgePosition;

    #[inline(always)]
    fn init(_target: &Self::QueryArg) -> Self {
        KeyQuery
    }

    #[inline]
    fn find_node(
        &mut self,
        target: &Self::QueryArg,
        caches: &[generic_btree::Child<OrderTreeTrait>],
    ) -> FindResult {
        let result = caches.binary_search_by(|x| {
            let range = x.cache.range.as_ref().unwrap();
            if target < &range.start {
                core::cmp::Ordering::Greater
            } else if target > &range.end {
                core::cmp::Ordering::Less
            } else {
                core::cmp::Ordering::Equal
            }
        });

        match result {
            Ok(i) => FindResult::new_found(i, 0),
            Err(i) => FindResult::new_missing(
                i.min(caches.len() - 1),
                if i == caches.len() { 1 } else { 0 },
            ),
        }
    }

    #[inline(always)]
    fn confirm_elem(
        &mut self,
        q: &Self::QueryArg,
        elem: &<OrderTreeTrait as BTreeTrait>::Elem,
    ) -> (usize, bool) {
        match q.cmp(&elem.pos) {
            Ordering::Less => (0, false),
            Ordering::Equal => (0, true),
            Ordering::Greater => (1, false),
        }
    }
}

impl UseLengthFinder<OrderTreeTrait> for OrderTreeTrait {
    fn get_len(cache: &<OrderTreeTrait as BTreeTrait>::Cache) -> usize {
        cache.len
    }
}
impl Default for OrderIndex {
    fn default() -> Self {
        Self::new()
    }
}
impl OrderIndex {
    pub(crate) fn at(&self, index: usize) -> Option<GraphEdgeId> {
        self.get_elem_at(index).map(|e| e.id)
    }
}
