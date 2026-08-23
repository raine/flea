use std::{ops::AsyncFnMut, ops::ControlFlow};

pub const COLLECTION_PAGE_SIZE: usize = 50;
const MAX_COLLECTION_PAGES: usize = 10_000;

pub struct CollectionPage<T, M = ()> {
    pub items: Vec<T>,
    pub total: usize,
    pub metadata: M,
    pub status: Option<u16>,
}

#[allow(async_fn_in_trait)]
pub trait CollectionPageSource {
    type Item;
    type Metadata;
    type Error;

    async fn page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<CollectionPage<Self::Item, Self::Metadata>, Self::Error>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionScanError<E> {
    Source(E),
    TotalChanged { status: Option<u16> },
    PrematureEmptyPage { status: Option<u16> },
    PageBoundExceeded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollectionScan<R> {
    Match(R),
    Complete { total: usize },
}

pub async fn scan_collection<S, V, R>(
    source: &S,
    mut visit: V,
) -> Result<CollectionScan<R>, CollectionScanError<S::Error>>
where
    S: CollectionPageSource,
    V: AsyncFnMut(Vec<S::Item>, S::Metadata) -> ControlFlow<R>,
{
    let mut offset = 0;
    let mut expected_total = None;

    for _ in 0..MAX_COLLECTION_PAGES {
        let page = source
            .page(offset, COLLECTION_PAGE_SIZE)
            .await
            .map_err(CollectionScanError::Source)?;
        let total = *expected_total.get_or_insert(page.total);
        if page.total != total {
            return Err(CollectionScanError::TotalChanged {
                status: page.status,
            });
        }
        let page_len = page.items.len();
        if let ControlFlow::Break(found) = visit(page.items, page.metadata).await {
            return Ok(CollectionScan::Match(found));
        }
        offset += page_len;
        if offset >= total {
            return Ok(CollectionScan::Complete { total });
        }
        if page_len == 0 {
            return Err(CollectionScanError::PrematureEmptyPage {
                status: page.status,
            });
        }
    }

    Err(CollectionScanError::PageBoundExceeded)
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque, convert::Infallible, ops::ControlFlow};

    use super::*;

    struct Pages {
        pages: RefCell<VecDeque<CollectionPage<u8>>>,
        offsets: RefCell<Vec<(usize, usize)>>,
    }

    impl CollectionPageSource for Pages {
        type Item = u8;
        type Metadata = ();
        type Error = Infallible;

        async fn page(
            &self,
            offset: usize,
            limit: usize,
        ) -> Result<CollectionPage<Self::Item>, Self::Error> {
            self.offsets.borrow_mut().push((offset, limit));
            Ok(self.pages.borrow_mut().pop_front().expect("fixture page"))
        }
    }

    fn page(items: &[u8], total: usize) -> CollectionPage<u8> {
        CollectionPage {
            items: items.to_vec(),
            total,
            metadata: (),
            status: Some(200),
        }
    }

    #[tokio::test]
    async fn scans_by_returned_count_until_the_stable_total() {
        let source = Pages {
            pages: RefCell::new(VecDeque::from([page(&[1, 2], 3), page(&[3], 3)])),
            offsets: RefCell::default(),
        };
        let mut items = Vec::new();

        let result = scan_collection(&source, async |page, _| {
            items.extend(page);
            ControlFlow::<()>::Continue(())
        })
        .await
        .unwrap();

        assert_eq!(result, CollectionScan::Complete { total: 3 });
        assert_eq!(items, [1, 2, 3]);
        assert_eq!(
            *source.offsets.borrow(),
            [(0, COLLECTION_PAGE_SIZE), (2, COLLECTION_PAGE_SIZE)]
        );
    }

    #[tokio::test]
    async fn rejects_total_changes_and_premature_empty_pages() {
        let changed = Pages {
            pages: RefCell::new(VecDeque::from([page(&[1], 2), page(&[2], 3)])),
            offsets: RefCell::default(),
        };
        let empty = Pages {
            pages: RefCell::new(VecDeque::from([page(&[], 1)])),
            offsets: RefCell::default(),
        };

        assert_eq!(
            scan_collection(&changed, async |_, _| ControlFlow::<()>::Continue(())).await,
            Err(CollectionScanError::TotalChanged { status: Some(200) })
        );
        assert_eq!(
            scan_collection(&empty, async |_, _| ControlFlow::<()>::Continue(())).await,
            Err(CollectionScanError::PrematureEmptyPage { status: Some(200) })
        );
    }

    #[tokio::test]
    async fn returns_early_matches_without_fetching_another_page() {
        let source = Pages {
            pages: RefCell::new(VecDeque::from([page(&[1], 2), page(&[2], 2)])),
            offsets: RefCell::default(),
        };

        let result = scan_collection(&source, async |items, _| {
            items
                .contains(&1)
                .then_some(ControlFlow::Break("found"))
                .unwrap_or(ControlFlow::Continue(()))
        })
        .await
        .unwrap();

        assert_eq!(result, CollectionScan::Match("found"));
        assert_eq!(*source.offsets.borrow(), [(0, COLLECTION_PAGE_SIZE)]);
    }
}
