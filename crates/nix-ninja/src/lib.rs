mod build;
pub mod cli;
mod dyndep;
pub mod local;
mod ninja_state;
mod relative_from;
mod resolve_cache;
mod subtool;
mod task;
/// Test fixtures embedded as `.rs`, because the driver's flake `src` globs
/// `.rs` alone: a binary fixture beside them is invisible to the nix build.
#[cfg(test)]
pub mod tests {
    pub mod zlib_debug_placeholder;
}
