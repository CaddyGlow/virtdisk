#![no_std]
//! Independent consumer proving feature-invariant portable reader signatures.
extern crate alloc;
use alloc::{sync::Arc, vec, vec::Vec};
use virtdisk::{
    DiskView, ParserLimits, ReadAt, ReadBudget, ReadContext, ReadError, SourceIdentity, Vdi, io,
};

struct Memory(Vec<u8>);
impl ReadAt for Memory {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn source_identity(&self) -> Option<SourceIdentity> {
        Some(SourceIdentity::new(42, 1, self.len()))
    }
    fn context(&self) -> ReadContext {
        ReadContext {
            partition: Some(1),
            ..ReadContext::default()
        }
    }
    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> io::Result<()> {
        let end = offset
            .checked_add(output.len() as u64)
            .filter(|end| *end <= self.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "memory bounds"))?;
        output.copy_from_slice(&self.0[offset as usize..end as usize]);
        Ok(())
    }
}
/// Exercise policy, views, extent traversal, typed errors and a format parser.
pub fn exercise() -> io::Result<()> {
    let limits = ParserLimits::default();
    limits.validate()?;
    let budget = ReadBudget::new(limits)?;
    budget.metadata(4)?;
    budget.work(1)?;
    let reservation = budget.cache(4)?;
    drop(reservation);
    let tight = ReadBudget::new(ParserLimits {
        metadata_bytes: 4,
        ..limits
    })?;
    tight.metadata(4)?;
    let refused = tight.metadata(1).unwrap_err();
    if refused.kind() != io::ErrorKind::ResourceLimit || tight.usage().metadata_bytes != 4 {
        return Err(io::Error::other("failed charge modified accounting"));
    }
    if budget.usage().cache_bytes != 0 {
        return Err(io::Error::other("reservation leaked"));
    }
    let memory: Arc<dyn ReadAt> = Arc::new(Memory(vec![0, 1, 2, 3]));
    let reader = budget.reader(memory);
    let view = DiskView::new(reader, 1, 2)?;
    let mut bytes = [0; 2];
    view.read_exact_at(0, &mut bytes)?;
    if bytes != [1, 2] {
        return Err(io::Error::other("view output"));
    }
    view.visit_extents(&mut |extent| {
        if extent.length != 2 {
            return Err(io::Error::other("extent length"));
        }
        Ok(())
    })?;
    let cancelled = view
        .visit_extents(&mut |_| Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled")))
        .unwrap_err();
    if cancelled.kind() != io::ErrorKind::Interrupted {
        return Err(io::Error::other("cancellation lost"));
    }
    view.read_exact_at(2, &mut [])?;
    if view.read_exact_at(3, &mut []).unwrap_err().kind() != io::ErrorKind::UnexpectedEof {
        return Err(io::Error::other("empty beyond-EOF read accepted"));
    }
    let contextual = ReadContext::default().error(
        "consumer",
        io::Error::new(io::ErrorKind::InvalidData, "fixture"),
    );
    if contextual
        .get_ref()
        .and_then(|source| source.downcast_ref::<ReadError>())
        .is_none()
    {
        return Err(io::Error::other("typed context unavailable"));
    }
    let mut fixture = vec![0u8; 1024];
    fixture[64..68].copy_from_slice(&0xbeda107fu32.to_le_bytes());
    fixture[68..72].copy_from_slice(&0x00010001u32.to_le_bytes());
    fixture[72..76].copy_from_slice(&400u32.to_le_bytes());
    fixture[76..80].copy_from_slice(&1u32.to_le_bytes());
    fixture[340..344].copy_from_slice(&512u32.to_le_bytes());
    fixture[344..348].copy_from_slice(&1024u32.to_le_bytes());
    fixture[360..364].copy_from_slice(&512u32.to_le_bytes());
    fixture[368..376].copy_from_slice(&512u64.to_le_bytes());
    fixture[376..380].copy_from_slice(&512u32.to_le_bytes());
    fixture[384..388].copy_from_slice(&1u32.to_le_bytes());
    fixture[392] = 1;
    fixture[408] = 2;
    fixture[512..516].copy_from_slice(&u32::MAX.to_le_bytes());
    let disk = Vdi::open(Arc::new(Memory(fixture)))?;
    disk.read_exact_at(0, &mut bytes)?;
    if bytes != [0, 0] {
        return Err(io::Error::other("VDI sparse output"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    #[test]
    fn consumer_exercises_same_api() {
        super::exercise().unwrap();
    }
}
