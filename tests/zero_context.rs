use std::{
    io,
    ops::ControlFlow,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use virtdisk::{
    OperationCancelled, OperationContext, OperationLimits, OperationPhase, OperationProgress,
    WriteAt, zero_image_with_context,
};
struct Writer {
    bytes: Mutex<Vec<u8>>,
    calls: AtomicUsize,
}
impl Writer {
    fn new(size: usize) -> Self {
        Self {
            bytes: Mutex::new(vec![37; size]),
            calls: AtomicUsize::new(0),
        }
    }
}
impl WriteAt for Writer {
    fn len(&self) -> u64 {
        self.bytes.lock().unwrap().len() as u64
    }
    fn write_all_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
        panic!("native zero dispatch required")
    }
    fn write_zeroes(&self, offset: u64, length: u64) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut bytes = self.bytes.lock().unwrap();
        bytes[offset as usize..(offset + length) as usize].fill(0);
        Ok(())
    }
    fn flush(&self) -> io::Result<()> {
        panic!("flush must remain explicit")
    }
}
#[test]
fn native_dispatch_uses_bounded_chunks_without_owned_scratch_or_flush() {
    let writer = Writer::new(131100);
    let mut events = Vec::new();
    let mut observer = |event: OperationProgress| {
        events.push(event);
        ControlFlow::Continue(())
    };
    let limits = OperationLimits::default()
        .logical_bytes(131073)
        .io_operations(3)
        .scratch_bytes(1)
        .unwrap();
    let mut context = OperationContext::new(limits).with_observer(&mut observer);
    zero_image_with_context(&writer, 7, 131073, &mut context).unwrap();
    assert_eq!(context.usage().logical_bytes, 131073);
    assert_eq!(context.usage().io_operations, 3);
    assert_eq!(context.usage().peak_scratch_bytes, 0);
    assert_eq!(
        events.iter().map(|e| e.completed_bytes).collect::<Vec<_>>(),
        [0, 65536, 131072, 131073]
    );
    assert!(
        events
            .iter()
            .all(|e| e.phase == OperationPhase::Zeroing && e.total_bytes == 131073)
    );
    let bytes = writer.bytes.lock().unwrap();
    assert!(
        bytes[..7]
            .iter()
            .chain(bytes[131080..].iter())
            .all(|&b| b == 37)
    );
    assert!(bytes[7..131080].iter().all(|&b| b == 0));
}
#[test]
fn whole_range_and_quota_refusal_precede_callbacks_or_mutation() {
    let writer = Writer::new(131100);
    let mut observer = |_: OperationProgress| panic!("refusal must precede notification");
    let mut context = OperationContext::default().with_observer(&mut observer);
    assert!(zero_image_with_context(&writer, 131100, 1, &mut context).is_err());
    let mut context = OperationContext::new(OperationLimits::default().io_operations(1));
    let error = zero_image_with_context(&writer, 0, 65537, &mut context).unwrap_err();
    assert!(
        error
            .get_ref()
            .unwrap()
            .is::<virtdisk::OperationLimitExceeded>()
    );
    assert_eq!(context.usage(), virtdisk::OperationUsage::default());
    assert_eq!(writer.calls.load(Ordering::Relaxed), 0);
}
#[test]
fn cancellation_retains_completed_prefix_and_releases_backend_locks() {
    let writer = Writer::new(131100);
    let mut observer = |event: OperationProgress| {
        if event.completed_bytes > 0 {
            let bytes = writer.bytes.lock().unwrap();
            assert!(bytes[..65536].iter().all(|&b| b == 0));
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut context = OperationContext::default().with_observer(&mut observer);
    let error = zero_image_with_context(&writer, 0, 131100, &mut context).unwrap_err();
    assert!(error.get_ref().unwrap().is::<OperationCancelled>());
    assert_eq!(context.usage().logical_bytes, 65536);
    assert_eq!(context.usage().io_operations, 1);
    assert!(
        writer.bytes.lock().unwrap()[65536..]
            .iter()
            .all(|&b| b == 37)
    );
}
#[test]
fn empty_ranges_notify_without_backend_calls() {
    let writer = Writer::new(512);
    let mut count = 0;
    let mut observer = |event: OperationProgress| {
        count += 1;
        assert_eq!(event.total_bytes, 0);
        ControlFlow::Continue(())
    };
    let mut context =
        OperationContext::new(OperationLimits::default().logical_bytes(0).io_operations(0))
            .with_observer(&mut observer);
    zero_image_with_context(&writer, 512, 0, &mut context).unwrap();
    assert_eq!(count, 1);
    assert_eq!(writer.calls.load(Ordering::Relaxed), 0);
}

#[test]
fn failed_native_call_is_charged_and_cumulative_quota_prevents_retry() {
    struct Failing;
    impl WriteAt for Failing {
        fn len(&self) -> u64 {
            65536
        }
        fn write_all_at(&self, _: u64, _: &[u8]) -> io::Result<()> {
            unreachable!()
        }
        fn write_zeroes(&self, _: u64, _: u64) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "injected native failure",
            ))
        }
        fn flush(&self) -> io::Result<()> {
            unreachable!()
        }
    }
    let mut context = OperationContext::new(OperationLimits::default().io_operations(1));
    assert_eq!(
        zero_image_with_context(&Failing, 0, 1, &mut context)
            .unwrap_err()
            .kind(),
        io::ErrorKind::BrokenPipe
    );
    assert_eq!(context.usage().io_operations, 1);
    assert_eq!(context.usage().logical_bytes, 0);
    assert!(
        zero_image_with_context(&Failing, 0, 1, &mut context)
            .unwrap_err()
            .get_ref()
            .unwrap()
            .is::<virtdisk::OperationLimitExceeded>()
    );
    assert_eq!(context.usage().io_operations, 1);
}
