/// Part 0 cross-compilation probe, independent of the application crates.
#[unsafe(no_mangle)]
pub extern "C" fn er_toolchain_probe() -> u32 {
    1
}
