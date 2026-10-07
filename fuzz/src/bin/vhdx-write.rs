fn main() {
    loop {
        honggfuzz::fuzz!(|data: &[u8]| {
            virtdisk_fuzz::run("vhdx-write", data).unwrap();
        });
    }
}
