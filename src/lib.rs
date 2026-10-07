#[cfg(windows)]
mod hook;
#[cfg(windows)]
mod il2cpp;
#[cfg(windows)]
mod runtime;
pub mod snapshot;
pub mod storage;
#[cfg(windows)]
mod version_proxy;
