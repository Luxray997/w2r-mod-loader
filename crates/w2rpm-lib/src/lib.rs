//! Manager logic behind `w2rpm` (CLI) and the egui app: check the index and packages,
//! install atomically. Specs: `docs/index.md`, `docs/manifest.md`, `docs/layout.md`, `docs/manager.md`.
//! The in-game host never links this crate; only the manager touches the network.

pub mod index;

/// A stable reason code from the specs (`rollback`, `bad_hash`, ...).
pub type Reason = &'static str;
pub mod package;

/// A refusal with a stable `code` (documented in docs/manager.md) and a human `detail`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub detail: String,
}

impl Error {
    pub fn new(code: &'static str, detail: impl Into<String>) -> Error {
        Error {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::new("io", e.to_string())
    }
}
pub mod catalog;
pub mod coremod;
pub mod check;
#[cfg(feature = "dev-repo")]
pub mod devrepo;
pub mod fetch;
pub mod install;
pub mod session;
pub mod share;
pub mod store;
#[cfg(test)]
mod testutil;
