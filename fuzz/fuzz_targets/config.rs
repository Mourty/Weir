//! Weir's own files, damaged: what it reads at startup and when asked.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| weir_fuzz::config(data));
