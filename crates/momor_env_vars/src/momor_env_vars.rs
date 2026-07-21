pub use env_var::{EnvVar, bool_env_var, env_var};
use std::sync::LazyLock;

/// Whether Momor is running in stateless mode.
/// When true, Momor will use in-memory databases instead of persistent storage.
pub static MOMOR_STATELESS: LazyLock<bool> = bool_env_var!("MOMOR_STATELESS");
