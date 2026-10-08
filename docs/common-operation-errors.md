# Common writer operation errors

This API is a development increment after the published 0.2.0 checkpoint; it is
not included in that registry artifact.

`ImageWriter` retains `io::Result` signatures for compatibility. Errors from
operations on an existing handle carry an `OperationError` inside the returned
`io::Error`; `kind()` remains the concrete failure's kind, and the standard
`Error::source()` chain retains the original error.

The context records the selected format, typed `ImageOperation`, and logical
offset/length for range operations. Fields are accessed through methods;
callers cannot construct misleading public error context. No image inspection,
path reopening or extra I/O is performed to annotate a failure.

```rust,no_run
use virtdisk::{ImageWriter, OperationError, WriteAt};

fn write(writer: &ImageWriter) -> std::io::Result<()> {
    writer.write_all_at(0, &[1; 512]).inspect_err(|error| {
        if let Some(context) = error.get_ref()
            .and_then(|source| source.downcast_ref::<OperationError>())
        {
            eprintln!("{:?}: {:?}", context.operation(), context.range());
        }
    })
}
```

Covered operations: read, write, write-zeroes, flush, discard, preallocation,
resize and native snapshot create/delete/revert. Open/create failures retain
their existing contracts; consolidated opening and recovery policies remain
future common API work. Concrete per-format writer APIs remain compatible.

This context does not classify a mutation as rolled back, durable or recovered.
A failed operation may have partially changed an image according to its
profile's contract. Consumers still own uncertainty handling and publication
durability. No recovery action is inferred from error text or error kind.
