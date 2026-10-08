#![cfg(feature = "std")]
use std::sync::Arc;
use virtdisk::{
    DiskView, ParserLimits, RawDisk, ReadAt, ReadBudget, ReadContext, contextual_reader,
};

#[test]
fn retained_aliases_share_tokens_and_replacement_receives_new_identity() {
    let dir = tempfile::tempdir().unwrap();
    let original = dir.path().join("original");
    let alias = dir.path().join("alias");
    std::fs::write(&original, [1, 2, 3, 4]).unwrap();
    std::fs::hard_link(&original, &alias).unwrap();
    let source: Arc<dyn ReadAt> = Arc::new(RawDisk::open(&original).unwrap());
    let linked = RawDisk::open(&alias).unwrap();
    assert_eq!(source.source_identity(), linked.source_identity());
    let budget = ReadBudget::new(ParserLimits::default()).unwrap();
    let wrapped = contextual_reader(budget.reader(source.clone()), ReadContext::default());
    assert_eq!(wrapped.source_identity(), source.source_identity());
    assert!(wrapped.host_context().is_some());
    let view = DiskView::new(wrapped, 1, 2).unwrap();
    assert!(
        view.source_identity()
            .unwrap()
            .same_storage(source.source_identity().unwrap())
    );
    assert_ne!(view.source_identity(), source.source_identity());
    std::fs::remove_file(&original).unwrap();
    std::fs::write(&original, [1, 2, 3, 4]).unwrap();
    let replaced = RawDisk::open(&original).unwrap();
    assert_ne!(replaced.source_identity(), source.source_identity());
    assert_eq!(source.source_identity(), linked.source_identity());
}
