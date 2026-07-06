//! Global registry of plugin-owned X11 window IDs.
//!
//! GUI adapters register every editor window on open so outbound XDND target
//! resolution can skip the whole plugin, not only the drag origin window.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

fn registry() -> &'static Mutex<HashSet<u32>> {
    static REGISTRY: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashSet::new()))
}

/// RAII handle for one registered plugin editor window.
///
/// Call [`register`](Self::register) when the window opens; dropping the guard
/// removes the XID from the global set.
#[derive(Debug)]
pub struct PluginWindowGuard {
    window: u32,
}

impl PluginWindowGuard {
    /// Register a plugin-owned X11 window ID.
    #[must_use]
    pub fn register(x11_window_id: u32) -> Self {
        if x11_window_id != 0 {
            if let Ok(mut windows) = registry().lock() {
                windows.insert(x11_window_id);
            }
        }
        Self {
            window: x11_window_id,
        }
    }
}

impl Drop for PluginWindowGuard {
    fn drop(&mut self) {
        if self.window == 0 {
            return;
        }
        if let Ok(mut windows) = registry().lock() {
            windows.remove(&self.window);
        }
    }
}

pub(super) fn is_registered_plugin_window(window: u32) -> bool {
    registry()
        .lock()
        .map(|windows| windows.contains(&window))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_registers_and_unregisters_on_drop() {
        let test_id = 0xdead_beef;
        {
            let _guard = PluginWindowGuard::register(test_id);
            assert!(is_registered_plugin_window(test_id));
        }
        assert!(!is_registered_plugin_window(test_id));
    }

    #[test]
    fn zero_window_id_is_ignored() {
        let _guard = PluginWindowGuard::register(0);
        assert!(!is_registered_plugin_window(0));
    }
}
