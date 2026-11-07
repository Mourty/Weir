//! A file to import settings from: a .zip or a .json Weir exported.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| weir_fuzz::import(data));
