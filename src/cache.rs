//! Regenerable Chromium profile subdirectories.
//!
//! A bm profile directory is a full user-data-dir, so it accumulates caches,
//! crash dumps and pre-fetched model stores alongside the state that makes a
//! profile valuable (Cookies, History, Login Data, IndexedDB, Local Storage,
//! extension state). Clone, clean and backup only care about the valuable
//! state: they skip the entries listed here, which the browser rebuilds on
//! its own.
//!
//! Verified against a real Chrome user-data-dir (macOS, 2026-10): caches are
//! created under *every* profile subdir (`Default/`, `Profile 1/`,
//! `System Profile/`), so single-name entries match at any depth. Root-level
//! GPU/shader caches use different names (`GPUPersistentCache`,
//! `GraphiteDawnCache`) than the per-profile ones (`GPUCache`,
//! `DawnGraphiteCache`), so both families are listed.
//!
//! Note: excluding `Service Worker/CacheStorage` drops PWA offline data.
//! That is acceptable by default; callers that need a byte-exact copy can
//! opt out (e.g. `clone --full`).

use std::path::Path;

/// Directory names (relative paths within a profile dir) that hold only
/// regenerable browser state. Multi-component entries match as a path
/// suffix; single-component entries match any path component.
pub const REGENERABLE_DIRS: &[&str] = &[
    // HTTP disk cache (per-profile subdir).
    "Cache",
    // JavaScript/WASM bytecode cache.
    "Code Cache",
    // GPU caches — per-profile and user-data-dir-root variants.
    "GPUCache",
    "GPUPersistentCache",
    "GraphiteDawnCache",
    "DawnGraphiteCache",
    "DawnWebGPUCache",
    // GL/Skia shader caches (older Chrome versions, root level).
    "GrShaderCache",
    "ShaderCache",
    // Media (EME/playout) cache.
    "Media Cache",
    // Service-worker cache storage. Also drops PWA offline data — see
    // module docs; `--full` callers keep it.
    "Service Worker/CacheStorage",
    // Crash reports and minidumps.
    "Crashpad",
    // Downloaded component / extension CRX caches (redownloaded on demand).
    "component_crx_cache",
    "extensions_crx_cache",
];

/// Case-sensitive name prefixes for Chromium's optimization-guide model and
/// hint stores, which appear under several names across versions:
/// `OptimizationGuideModelsManifest`, `optimization_guide_model_store`,
/// `OptGuidePredictionModels`, `OptimizationHints`,
/// `optimization_guide_hint_cache_store`.
const REGENERABLE_PREFIXES: &[&str] = &[
    "OptimizationGuide",
    "optimization_guide",
    "OptGuide",
    "OptimizationHints",
];

/// Whether `relative_path` (relative to a profile dir root) holds only
/// regenerable state and may be skipped when copying, cleaning, or backing
/// up a profile. Paths *under* a regenerable dir match too, so callers can
/// test any walked path without tracking skipped subtrees.
pub fn is_regenerable(relative_path: &Path) -> bool {
    let components: Vec<String> = relative_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    for entry in REGENERABLE_DIRS {
        let entry: Vec<String> = entry.split('/').map(str::to_string).collect();
        if components.len() >= entry.len() && components.windows(entry.len()).any(|w| w == entry) {
            return true;
        }
    }
    components
        .iter()
        .any(|name| REGENERABLE_PREFIXES.iter().any(|p| name.starts_with(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_level_cache_dirs_match() {
        for name in [
            "Cache",
            "Code Cache",
            "Crashpad",
            "component_crx_cache",
            "extensions_crx_cache",
            "GPUPersistentCache",
            "GraphiteDawnCache",
        ] {
            assert!(is_regenerable(Path::new(name)), "{name} should match");
        }
    }

    #[test]
    fn nested_cache_dirs_match_under_any_profile_subdir() {
        assert!(is_regenerable(Path::new("Default/Cache")));
        assert!(is_regenerable(Path::new("Default/GPUCache")));
        assert!(is_regenerable(Path::new("Default/Code Cache/js")));
        assert!(is_regenerable(Path::new("Profile 1/DawnWebGPUCache")));
        assert!(is_regenerable(Path::new("System Profile/Media Cache")));
        // Multi-component entry matches as a path suffix at any depth.
        assert!(is_regenerable(Path::new(
            "Default/Service Worker/CacheStorage"
        )));
        assert!(is_regenerable(Path::new(
            "Default/Service Worker/CacheStorage/https_example.com"
        )));
    }

    #[test]
    fn optimization_guide_stores_match_across_naming_variants() {
        assert!(is_regenerable(Path::new("OptimizationGuideModelsManifest")));
        assert!(is_regenerable(Path::new("optimization_guide_model_store")));
        assert!(is_regenerable(Path::new("OptGuidePredictionModels")));
        assert!(is_regenerable(Path::new("OptimizationHints")));
        assert!(is_regenerable(Path::new(
            "Default/optimization_guide_hint_cache_store"
        )));
    }

    #[test]
    fn valuable_state_never_matches() {
        for name in [
            "Default",
            "Local State",
            "Cookies",
            "History",
            "Login Data",
            "IndexedDB",
            "Local Storage",
            "Default/Network",
            // Only CacheStorage under Service Worker is regenerable.
            "Service Worker",
            "Default/Service Worker/Database",
            // Exact-name entries don't bleed into longer names.
            "component_crx_cache_backup",
            "CacheBackup",
            "crashpad_reports",
        ] {
            assert!(!is_regenerable(Path::new(name)), "{name} must not match");
        }
    }
}
