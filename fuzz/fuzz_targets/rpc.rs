//! Lines to the control socket, run as requests on a mixer of its own.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| weir_fuzz::rpc(data));
