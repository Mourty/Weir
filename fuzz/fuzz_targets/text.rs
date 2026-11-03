//! Other text from outside: key names, layouts, file name patterns, colors, versions, GitHub answers, the session environment, crash reports.
#![no_main]
libfuzzer_sys::fuzz_target!(|data: &[u8]| weir_fuzz::text(data));
