//! Synchronous source access inside native exporters.
use crate::{OperationContext, OperationPhase, ReadAt};
use std::io;

pub(crate) trait ExportSource {
    fn size(&self) -> u64;
    fn begin(&mut self, phase: OperationPhase, bytes: u64) -> io::Result<()>;
    fn read(&mut self, offset: u64, destination: &mut [u8]) -> io::Result<()>;
}
impl ExportSource for &dyn ReadAt {
    fn size(&self) -> u64 {
        self.len()
    }
    fn begin(&mut self, _: OperationPhase, _: u64) -> io::Result<()> {
        Ok(())
    }
    fn read(&mut self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.read_exact_at(offset, destination)
    }
}
pub(crate) struct ContextSource<'a, 'observer> {
    source: &'a dyn ReadAt,
    context: &'a mut OperationContext<'observer>,
    phase: OperationPhase,
    completed: u64,
    total: u64,
}
impl<'a, 'observer> ContextSource<'a, 'observer> {
    pub(crate) fn new(
        source: &'a dyn ReadAt,
        context: &'a mut OperationContext<'observer>,
    ) -> Self {
        Self {
            source,
            context,
            phase: OperationPhase::ImageExport,
            completed: 0,
            total: 0,
        }
    }
}
impl ExportSource for ContextSource<'_, '_> {
    fn size(&self) -> u64 {
        self.source.len()
    }
    fn begin(&mut self, phase: OperationPhase, bytes: u64) -> io::Result<()> {
        self.context.preflight(bytes, 1, 1)?;
        self.phase = phase;
        self.completed = 0;
        self.total = bytes;
        self.context.observe_phase(phase, 0, bytes)
    }
    fn read(&mut self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.context.preflight(destination.len() as u64, 1, 1)?;
        self.context.attempted_io();
        self.source.read_exact_at(offset, destination)?;
        self.context.completed(destination.len() as u64);
        self.completed += destination.len() as u64;
        self.context
            .observe_phase(self.phase, self.completed, self.total)
    }
}
