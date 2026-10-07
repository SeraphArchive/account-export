//! Shared detour install + panic fencing for native/IL2CPP hooks.
//!
//! Two invariants:
//!   1. The `OnceLock` is populated BEFORE `enable()`. `enable()` patches a jmp
//!      that other threads can enter immediately; a detour that `get().unwrap()`s
//!      an empty cell aborts the process (`panic = "abort"` used to make this
//!      instant death; even with unwind it is UB across FFI).
//!   2. Extra work in a detour (alloc, logging, IL2CPP) is wrapped in
//!      `no_panic` so a Rust panic never unwinds into game/Win32 frames.
#![cfg(windows)]

use retour::{Function, GenericDetour};
use std::sync::OnceLock;

/// Create, store, THEN enable. Idempotent if `slot` is already populated.
pub unsafe fn install<F: Function>(
    slot: &'static OnceLock<GenericDetour<F>>,
    target: F,
    hook: F,
) -> anyhow::Result<()> {
    if slot.get().is_some() {
        return Ok(());
    }
    let d = GenericDetour::new(target, hook)
        .map_err(|e| anyhow::anyhow!("detour create failed: {e:?}"))?;
    if slot.set(d).is_err() {
        return Ok(());
    }
    let detour = slot
        .get()
        .ok_or_else(|| anyhow::anyhow!("detour cell empty after set"))?;
    if let Err(e) = detour.enable() {
        // enable() may have patched the target before failing. Disabling restores
        // the original bytes; the detour stays in the slot so its trampoline is
        // never freed while a thread could still be inside it.
        unsafe {
            let _ = detour.disable();
        }
        anyhow::bail!("detour enable failed: {e:?}");
    }
    Ok(())
}
