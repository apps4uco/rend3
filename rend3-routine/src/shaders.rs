//! Holds the sources and binaries for all shaders.

use include_dir::{include_dir, Dir};

/// All shaders in SPIRV form. This is what is used when GPU mode is enabled to
/// get spirv passthrough.
pub static SPIRV_SHADERS: Dir = include_dir!("$CARGO_MANIFEST_DIR/shaders/spirv");
/// Naga-compatible shaders in SPIRV form. This is what is used in CPU mode and
/// the naga-passing GPU mode shaders.
pub static WGSL_SHADERS: Dir = include_dir!("$CARGO_MANIFEST_DIR/shaders/wgsl");