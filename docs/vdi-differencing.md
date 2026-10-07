# VDI differencing images

The native VDI header records parent creation and modification UUIDs, not a
parent filename. `Vdi::open_chain` therefore takes an explicitly ordered list
of direct parent through base paths. Every path is caller-authorized; no host
path is inferred from disk bytes. At most 32 total images are permitted, with
caller-tightened recursion, metadata, cache and traversal budgets shared across
the chain. Reject aliases, cycles, missing parents and extra ancestors.

Each differencing image must match the immediate parent's creation UUID and
modification UUID exactly. Capacity and sector geometry must agree; parent allocation units may differ.
Every validated image must have non-nil creation and modification UUIDs,
including freshly created standalone images and children. Oracle's
`vdiValidateHeader` rejects either nil field; matching nil parent modification
values do not make a chain valid. Nil parent-linkage fields remain expected for
standalone images. Native validation does not require a particular UUID version
or variant, so the reader adds no such restriction. Retain immutable parents for
the entire child lifetime. Changing parent contents invalidates children.

This is strict native header validation for the supported VDI 1.1 profiles with
400-byte or 416-byte headers. Malformed or legacy producer images with nil own
identities fail as InvalidData before BAT or payload reads; opening never repairs
or replaces their identities. Signature-only format detection remains separate
from validated image opening.

Free mappings inherit the parent; explicit-zero mappings mask it. Allocated
blocks are private to the child. New overlays receive fresh creation and
modification UUIDs and capture both parent identifiers. They never overwrite
an existing destination. Standalone constructors continue refusing unresolved
parent references.

Writable overlays copy the complete inherited block before publishing its
private mapping using the existing bounded sidecar transaction. Partial zeroing
must preserve untouched inherited bytes; full-block zeroing must not expose
parent data. Recovery needs the same explicitly authorized parent chain and
validates old and proposed views before changing child bytes. Parent and sibling
files remain immutable. Linux journal persistence restrictions still apply.

Fault tests must cover publication ordering, torn mappings, authorization refusal,
identity epochs, poisoned handles and repeated recovery. Independently validate
native UUID fields and VirtualBox acceptance when native tooling is available;
QEMU flattening of our standalone result is supplementary evidence only.

Primary implementation references:

- [Oracle VDI backend](https://github.com/VirtualBox/virtualbox/blob/main/src/VBox/Storage/VDI.cpp)
- [Oracle disk chain engine](https://github.com/VirtualBox/virtualbox/blob/main/src/VBox/Storage/VD.cpp)
