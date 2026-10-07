fn main() {
    loop {
        honggfuzz::fuzz!(|data: &[u8]| {
            virtdisk_fuzz::qcow2(data);
        });
    }
}
