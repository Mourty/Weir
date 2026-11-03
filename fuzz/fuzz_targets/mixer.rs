//! A mixer made up from the bytes, played by the mixing code.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| weir_fuzz::mixer(data));
