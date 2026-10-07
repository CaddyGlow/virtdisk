fn main() {
    loop {
        honggfuzz::fuzz!(|data: &[u8]| {
            virtdisk_fuzz::raw_write(data);
        });
    }
}
