mod iterator;
mod search_marker;

pub(crate) use iterator::ListIterator;
pub use search_marker::MarkerList;

use super::*;
use crate::sync::RwLockWriteGuard;

pub(crate) struct ItemPosition {
    pub parent: YTypeRef,
    pub left: ItemRef,
    pub right: ItemRef,
    pub index: u64,
    pub offset: u64,
}

impl ItemPosition {
    pub fn forward(&mut self) {
        if let Some(right) = self.right.get() {
            if !right.deleted() {
                self.index += right.len();
            }

            self.left = self.right.clone();
            self.right = right.right.clone().into();
        } else {
            // FAIL
        }
    }

    /// we found a position cursor point in between a splitable item,
    /// we need to split the item by the offset.
    ///
    /// before:
    /// ---------------------------------
    ///    ^left                ^right
    ///            ^offset
    /// after:
    /// ---------------------------------
    ///    ^left   ^right
    pub fn normalize(&mut self, store: &mut DocStore) -> JwstCodecResult {
        if self.offset > 0 {
            debug_assert!(self.left.is_some());
            if let Some(left) = self.left.get() {
                let (left, right) = store.split_node(left.id, self.offset)?;
                self.left = left.as_item();
                self.right = right.as_item();
                self.index += self.offset;
                self.offset = 0;
            }
        }

        Ok(())
    }
}

pub(crate) trait ListType: AsInner<Inner = YTypeRef> {
    #[inline(always)]
    fn content_len(&self) -> u64 {
        self.as_inner().get().unwrap().read().unwrap().len
    }

    fn iter_item(&self) -> ListIterator {
        let inner = self.as_inner().get().unwrap().read().unwrap();
        ListIterator::new(inner.start.clone())
    }

    fn find_pos(&self, inner: &YType, index: u64) -> Option<ItemPosition> {
        let mut remaining = index;
        let start = inner.start.clone();

        let mut pos = ItemPosition {
            parent: self.as_inner().clone(),
            left: Somr::none(),
            right: start,
            index: 0,
            offset: 0,
        };

        if pos.right.is_none() {
            return Some(pos);
        }

        if let Some(markers) = &inner.markers {
            if let Some(marker) = markers.find_marker(inner, index) {
                if marker.index > remaining {
                    remaining = 0
                } else {
                    remaining -= marker.index;
                }
                pos.index = marker.index;
                pos.left = marker.ptr.get().and_then(|ptr| ptr.left.clone()).into();
                pos.right = marker.ptr;
            }
        };

        while remaining > 0 {
            if let Some(item) = pos.right.get() {
                if !item.deleted() {
                    let content_len = item.len();
                    if remaining < content_len {
                        pos.offset = remaining;
                        remaining = 0;
                    } else {
                        pos.index += content_len;
                        remaining -= content_len;
                    }
                }

                pos.left = pos.right.clone();
                pos.right = item.right.clone().into();
            } else {
                return None;
            }
        }

        Some(pos)
    }

    fn insert_at(&mut self, index: u64, content: Content) -> JwstCodecResult {
        if index > self.content_len() {
            return Err(JwstCodecError::IndexOutOfBound(index));
        }

        if let Some(ty) = self.as_inner().get() {
            let inner = ty.write().unwrap();
            if let Some(mut pos) = self.find_pos(&inner, index) {
                if let Some(mut store) = inner.store_mut() {
                    pos.normalize(&mut store)?;
                    Self::insert_after(inner, &mut store, pos, content)?;
                }
            }
        } else {
            return Err(JwstCodecError::DocReleased);
        }

        Ok(())
    }

    fn insert_after(
        mut lock: RwLockWriteGuard<YType>,
        store: &mut DocStore,
        pos: ItemPosition,
        content: Content,
    ) -> JwstCodecResult {
        if let Some(markers) = &lock.markers {
            markers.update_marker_changes(pos.index, content.clock_len() as i64);
        }

        let item = store.create_item(
            content,
            pos.left.clone(),
            pos.right.clone(),
            Some(Parent::Type(pos.parent)),
            None,
        );

        store.integrate(Node::Item(item), 0, Some(&mut lock))?;

        Ok(())
    }

    fn get_item_at(&self, index: u64) -> Option<(Somr<Item>, u64)> {
        if index >= self.content_len() {
            return None;
        }

        let inner = self.as_inner().get().unwrap().read().unwrap();

        if let Some(pos) = self.find_pos(&inner, index) {
            if pos.offset == 0 {
                return Some((pos.right, 0));
            } else {
                return Some((pos.left, pos.offset));
            }
        }

        None
    }

    fn remove_at(&mut self, idx: u64, len: u64) -> JwstCodecResult {
        if len == 0 {
            return Ok(());
        }

        if idx >= self.content_len() {
            return Err(JwstCodecError::IndexOutOfBound(idx));
        }

        if let Some(ty) = self.as_inner().get() {
            let inner = ty.write().unwrap();
            if let Some(pos) = self.find_pos(&inner, idx) {
                if let Some(mut store) = inner.store_mut() {
                    Self::remove_after(inner, &mut store, pos, len)?;
                }
            }
        } else {
            return Err(JwstCodecError::DocReleased);
        }

        Ok(())
    }

    fn remove_after(
        mut lock: RwLockWriteGuard<YType>,
        store: &mut DocStore,
        mut pos: ItemPosition,
        len: u64,
    ) -> JwstCodecResult {
        pos.normalize(store)?;

        let mut remaining = len;

        while remaining > 0 {
            if let Some(item) = pos.right.get() {
                if !item.deleted() {
                    let content_len = item.len();
                    if remaining < content_len {
                        store.split_node(item.id, remaining)?;
                        remaining = 0;
                    } else {
                        remaining -= content_len;
                    }
                    store.delete_set.add(item.id.client, item.id.clock, content_len);
                    DocStore::delete_item(item, Some(&mut lock));
                }

                pos.forward();
            } else {
                break;
            }
        }

        if let Some(markers) = &lock.markers {
            markers.update_marker_changes(pos.index, -((len - remaining) as i64));
        }

        Ok(())
    }
}
