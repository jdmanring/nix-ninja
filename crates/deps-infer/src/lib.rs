pub mod c_include_parser;
// Optional: libclang is a real preprocessor and the accurate answer, but
// clang-sys links it at runtime and the driver runs inside every task
// derivation. Gated so a gcc build does not carry libclang in its closure.
#[cfg(feature = "clang")]
pub mod clang_infer;
pub mod gcc_depfile;
mod gcc_depfile_parser;
mod gcc_include_parser;
