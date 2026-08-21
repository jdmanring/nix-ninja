pub mod canon;
// Public for nix-ninja, which drives the Loader directly and so never gets
// the `.n2_db` that `load::read()` opens for it. The used-input record lives
// here and nowhere else.
pub mod db;
pub mod densemap;
pub mod depfile;
mod eval;
pub mod graph;
mod hash;
pub mod load;
pub mod parse;
mod process;
#[cfg(unix)]
mod process_posix;
#[cfg(windows)]
mod process_win;
mod progress;
mod progress_dumb;
mod progress_fancy;
pub mod run;
pub mod scanner;
mod signal;
mod smallmap;
mod task;
mod terminal;
mod trace;
mod work;

#[cfg(feature = "jemalloc")]
#[cfg(not(any(miri, windows, target_arch = "wasm32")))]
use jemallocator::Jemalloc;

#[cfg(feature = "jemalloc")]
#[cfg(not(any(miri, windows, target_arch = "wasm32")))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;
