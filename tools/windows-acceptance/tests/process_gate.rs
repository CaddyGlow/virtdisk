use virtdisk_windows_acceptance::process_gate::{Ack, Identity, Policy, find_flush_iat};
fn pe() -> Vec<u8> {
    let mut b = vec![0; 4096];
    b[..2].copy_from_slice(b"MZ");
    b[60..64].copy_from_slice(&128u32.to_le_bytes());
    b[128..132].copy_from_slice(b"PE\0\0");
    b[132..134].copy_from_slice(&0x8664u16.to_le_bytes());
    b[148..150].copy_from_slice(&240u16.to_le_bytes());
    b[152..154].copy_from_slice(&0x20bu16.to_le_bytes());
    b[208..212].copy_from_slice(&4096u32.to_le_bytes());
    b[260..264].copy_from_slice(&16u32.to_le_bytes());
    b[272..276].copy_from_slice(&512u32.to_le_bytes());
    b[276..280].copy_from_slice(&40u32.to_le_bytes());
    b[512..516].copy_from_slice(&640u32.to_le_bytes());
    b[524..528].copy_from_slice(&600u32.to_le_bytes());
    b[528..532].copy_from_slice(&672u32.to_le_bytes());
    b[600..613].copy_from_slice(b"KERNEL32.dll\0");
    b[640..648].copy_from_slice(&720u64.to_le_bytes());
    b[722..739].copy_from_slice(b"FlushFileBuffers\0");
    b
}
#[test]
fn identifies_only_bounded_x64_real_import_and_rejects_missing_or_corrupt_tables() {
    let b = pe();
    assert_eq!(find_flush_iat(&b).unwrap(), 672);
    for len in [0, 64, 128, 153, 512, 730] {
        assert!(find_flush_iat(&b[..len]).is_err());
    }
    for (at, bytes) in [
        (132, 0x14cu32.to_le_bytes()),
        (272, u32::MAX.to_le_bytes()),
        (512, u32::MAX.to_le_bytes()),
        (524, u32::MAX.to_le_bytes()),
    ] {
        let mut bad = b.clone();
        bad[at..at + 4].copy_from_slice(&bytes);
        assert!(find_flush_iat(&bad).is_err());
    }
    let mut missing = b.clone();
    missing[722] = b'X';
    assert!(find_flush_iat(&missing).is_err());
}
#[test]
fn fixed_protocol_rejects_wrong_identity_out_of_order_and_unconfirmed_flush() {
    let id = Identity {
        volume: 7,
        file: 123,
    };
    let ready = Ack::ready(id);
    let flush = Ack::flushed(1, id);
    assert_eq!(Ack::decode(&flush.encode()).unwrap(), flush);
    let mut policy = Policy::new(id);
    assert!(policy.observe(flush).is_err());
    policy.observe(ready).unwrap();
    assert!(policy.observe(Ack::flushed(2, id)).is_err());
    policy.observe(flush).unwrap();
    assert!(policy.observe(flush).is_err());
    assert!(
        policy
            .observe(Ack::flushed(
                2,
                Identity {
                    volume: 8,
                    file: 123
                }
            ))
            .is_err()
    );
    let mut bytes = flush.encode();
    bytes[28] = 1;
    assert!(Ack::decode(&bytes).is_err());
    bytes = flush.encode();
    bytes[8..12].copy_from_slice(&9u32.to_le_bytes());
    assert!(Ack::decode(&bytes).is_err());
}
#[test]
fn old_new_models_allow_only_one_whole_sector_mutation_with_retained_prefix() {
    use virtdisk_windows_acceptance::process_gate::Workload;
    for child in [false, true] {
        for sector in [512, 4096] {
            for retained in [false, true] {
                let w = Workload {
                    child,
                    sector,
                    retained,
                };
                let (old, new) = w.models().unwrap();
                let (offset, bytes) = w.write();
                assert_eq!(&new[offset as usize..offset as usize + 8], &bytes);
                assert_eq!(&old[..offset as usize], &new[..offset as usize]);
                assert_eq!(&old[offset as usize + 8..], &new[offset as usize + 8..]);
                if retained {
                    assert_eq!(&old[8195..8203], &[11; 8]);
                }
                assert_eq!(offset / u64::from(sector), (offset + 7) / u64::from(sector));
            }
        }
    }
    assert!(
        Workload {
            child: true,
            sector: 1,
            retained: false
        }
        .models()
        .is_err()
    );
}
