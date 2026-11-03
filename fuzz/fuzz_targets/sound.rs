//! A sound file to add to hotkeys' sounds.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| weir_fuzz::sound(data));
