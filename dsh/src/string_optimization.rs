use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::path::Path;

use compact_str::CompactString;
use lru::LruCache;
use string_interner::{DefaultBackend, DefaultSymbol, StringInterner};
use tracing::debug;

/// String optimization context for efficient memory management
/// This provides caching, interning, and pooling for string operations
pub struct StringOptimizationContext {
    /// String interner for deduplicating common strings
    interner: StringInterner<DefaultBackend>,

    /// Cache for frequently used interned strings
    common_strings: HashMap<&'static str, DefaultSymbol>,

    /// LRU cache for tilde expansion results
    tilde_cache: LruCache<CompactString, CompactString>,

    /// LRU cache for path processing results (glob root finding)
    path_cache: LruCache<CompactString, (CompactString, CompactString)>,

    /// Pool of reusable string buffers
    string_pool: Vec<String>,

    /// Indices of available strings in the pool
    available_strings: Vec<usize>,

    /// Statistics for monitoring performance
    stats: OptimizationStats,
}

/// Statistics for monitoring string optimization performance
#[derive(Debug, Default)]
pub struct OptimizationStats {
    pub tilde_cache_hits: u64,
    pub tilde_cache_misses: u64,
    pub path_cache_hits: u64,
    pub path_cache_misses: u64,
    pub string_pool_reuses: u64,
    pub string_pool_allocations: u64,
    pub interned_strings: u64,
}

impl OptimizationStats {
    /// Calculate cache hit rate for tilde expansion
    pub fn tilde_hit_rate(&self) -> f64 {
        if self.tilde_cache_hits + self.tilde_cache_misses == 0 {
            0.0
        } else {
            self.tilde_cache_hits as f64 / (self.tilde_cache_hits + self.tilde_cache_misses) as f64
        }
    }

    /// Calculate cache hit rate for path processing
    pub fn path_hit_rate(&self) -> f64 {
        if self.path_cache_hits + self.path_cache_misses == 0 {
            0.0
        } else {
            self.path_cache_hits as f64 / (self.path_cache_hits + self.path_cache_misses) as f64
        }
    }

    /// Calculate string pool reuse rate
    pub fn pool_reuse_rate(&self) -> f64 {
        if self.string_pool_reuses + self.string_pool_allocations == 0 {
            0.0
        } else {
            self.string_pool_reuses as f64
                / (self.string_pool_reuses + self.string_pool_allocations) as f64
        }
    }
}

impl StringOptimizationContext {
    /// Create a new string optimization context
    pub fn new() -> Self {
        let mut ctx = Self {
            interner: StringInterner::default(),
            common_strings: HashMap::new(),
            tilde_cache: LruCache::new(NonZeroUsize::new(256).unwrap()),
            path_cache: LruCache::new(NonZeroUsize::new(128).unwrap()),
            string_pool: Vec::with_capacity(64),
            available_strings: Vec::new(),
            stats: OptimizationStats::default(),
        };

        // Pre-intern common strings
        ctx.intern_common_strings();
        ctx
    }

    /// Pre-intern frequently used strings to avoid repeated allocations
    fn intern_common_strings(&mut self) {
        let common_strings = [
            // Shell operators
            "&",
            "|",
            "(",
            ")",
            "<(",
            ")",
            "&&",
            "||",
            ";",
            // Common paths
            ".",
            "..",
            "~",
            "/",
            "/tmp",
            "/usr",
            "/home",
            // Common commands
            "ls",
            "cd",
            "git",
            "echo",
            "cat",
            "grep",
            "find",
            "ps",
            "kill",
            "mkdir",
            "rmdir",
            "cp",
            "mv",
            "rm",
            "chmod",
            "chown",
            // Common options
            "-l",
            "-a",
            "-r",
            "-f",
            "-v",
            "--help",
            "--version",
            // Common file extensions
            ".txt",
            ".log",
            ".json",
            ".yaml",
            ".toml",
            ".rs",
            ".py",
            ".js",
        ];

        for s in common_strings {
            let symbol = self.interner.get_or_intern(s);
            self.common_strings.insert(s, symbol);
        }

        self.stats.interned_strings = common_strings.len() as u64;
        debug!("Pre-interned {} common strings", common_strings.len());
    }

    /// Get an interned string if it exists in the common strings cache
    pub fn get_interned_string(&self, key: &str) -> Option<&str> {
        self.common_strings
            .get(key)
            .and_then(|symbol| self.interner.resolve(*symbol))
    }

    /// Intern a string and return its symbol
    pub fn intern_string(&mut self, s: &str) -> DefaultSymbol {
        self.interner.get_or_intern(s)
    }

    /// Resolve an interned string symbol
    pub fn resolve_symbol(&self, symbol: DefaultSymbol) -> Option<&str> {
        self.interner.resolve(symbol)
    }

    /// Perform cached tilde expansion
    pub fn expand_tilde_cached<'a>(&mut self, input: &'a str) -> Cow<'a, str> {
        // Quick check: if it doesn't start with ~, return as-is
        if !input.starts_with('~') {
            return Cow::Borrowed(input);
        }

        let key = CompactString::new(input);

        // Check cache first
        if let Some(cached) = self.tilde_cache.get(&key).cloned() {
            self.stats.tilde_cache_hits += 1;
            return Cow::Owned(cached.into_string());
        }

        self.stats.tilde_cache_misses += 1;

        // Perform tilde expansion
        let expanded = shellexpand::tilde(input);

        // If no change, return original
        if expanded.len() == input.len() && expanded == input {
            return Cow::Borrowed(input);
        }

        // Cache the result
        let result = CompactString::new(&expanded);
        self.tilde_cache.put(key, result.clone());

        Cow::Owned(result.into_string())
    }

    /// Fast tilde expansion with optimizations for common cases
    pub fn fast_tilde_expand<'a>(&mut self, input: &'a str) -> Cow<'a, str> {
        // Handle simple ~ case efficiently
        if input == "~" {
            if let Some(home) = dirs::home_dir() {
                let home_str = home.to_string_lossy();
                let key = CompactString::new("~");
                let result = CompactString::new(&home_str);
                self.tilde_cache.put(key, result.clone());
                return Cow::Owned(result.into_string());
            }
        }

        // Use cached expansion for other cases
        self.expand_tilde_cached(input)
    }

    /// Zero-copy trim operation
    pub fn trim_cow(input: &str) -> Cow<str> {
        let trimmed = input.trim();

        // If trimming didn't change the string, return borrowed
        if trimmed.len() == input.len() && std::ptr::eq(trimmed.as_ptr(), input.as_ptr()) {
            Cow::Borrowed(input)
        } else {
            Cow::Borrowed(trimmed)
        }
    }

    /// Get a string from the pool or allocate a new one
    pub fn get_pooled_string(&mut self, capacity_hint: usize) -> String {
        if let Some(idx) = self.available_strings.pop() {
            let mut reused = std::mem::take(&mut self.string_pool[idx]);
            reused.clear();
            if reused.capacity() < capacity_hint {
                reused.reserve(capacity_hint - reused.capacity());
            }
            self.stats.string_pool_reuses += 1;
            reused
        } else {
            self.stats.string_pool_allocations += 1;
            String::with_capacity(capacity_hint)
        }
    }

    /// Return a string to the pool for reuse
    pub fn return_to_pool(&mut self, mut s: String) {
        // Don't pool excessively large strings
        if s.capacity() > 4096 {
            return;
        }

        s.clear();
        let idx = self.string_pool.len();
        self.string_pool.push(s);
        self.available_strings.push(idx);
    }

    /// Cached glob root finding
    pub fn find_glob_root_cached(&mut self, path: &str) -> (Cow<str>, Cow<str>) {
        let key = CompactString::new(path);

        // Check cache first
        if let Some((root, glob)) = self.path_cache.get(&key).cloned() {
            self.stats.path_cache_hits += 1;
            return (
                Cow::Owned(root.into_string()),
                Cow::Owned(glob.into_string()),
            );
        }

        self.stats.path_cache_misses += 1;

        // Compute glob root
        let (root, glob) = self.find_glob_root_optimized(path);

        // Cache the result
        let root_compact = CompactString::new(&root);
        let glob_compact = CompactString::new(&glob);
        self.path_cache
            .put(key, (root_compact.clone(), glob_compact.clone()));

        (
            Cow::Owned(root_compact.into_string()),
            Cow::Owned(glob_compact.into_string()),
        )
    }

    /// Optimized glob root finding implementation
    fn find_glob_root_optimized(&self, path: &str) -> (String, String) {
        let path_obj = Path::new(path);

        // Handle relative paths early
        if path_obj.is_relative() {
            return (".".to_string(), path.to_string());
        }

        // Process path components efficiently
        let mut root_parts = Vec::with_capacity(8);
        let mut glob_parts = Vec::with_capacity(8);
        let mut found_glob = false;

        for component in path_obj.iter() {
            let component_str = component.to_string_lossy();

            if !found_glob && component_str.contains('*') {
                found_glob = true;
            }

            if found_glob {
                glob_parts.push(component_str.into_owned());
            } else {
                root_parts.push(component_str.into_owned());
            }
        }

        // Build root path
        let root = if root_parts.is_empty() {
            ".".to_string()
        } else {
            let mut result = root_parts.join(std::path::MAIN_SEPARATOR_STR);
            // Handle double slash at the beginning
            if result.starts_with("//") {
                result.drain(..1);
            }
            result
        };

        // Build glob pattern
        let mut glob = glob_parts.join(std::path::MAIN_SEPARATOR_STR);
        if Path::new(&glob).is_absolute() {
            glob.drain(..1);
        }

        (root, glob)
    }

    /// Get optimization statistics
    pub fn stats(&self) -> &OptimizationStats {
        &self.stats
    }

    /// Reset statistics
    pub fn reset_stats(&mut self) {
        self.stats = OptimizationStats::default();
    }

    /// Clear all caches (useful for testing or memory pressure)
    pub fn clear_caches(&mut self) {
        self.tilde_cache.clear();
        self.path_cache.clear();
        debug!("Cleared string optimization caches");
    }

    /// Get cache sizes for monitoring
    pub fn cache_info(&self) -> (usize, usize, usize) {
        (
            self.tilde_cache.len(),
            self.path_cache.len(),
            self.string_pool.len(),
        )
    }
}

impl Default for StringOptimizationContext {
    fn default() -> Self {
        Self::new()
    }
}

// Static instance for commonly used fixed strings
use once_cell::sync::Lazy;

/// Pre-defined fixed strings to avoid allocations
static FIXED_STRINGS: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut map = HashMap::new();

    // Shell operators
    map.insert("&", "&");
    map.insert("|", "|");
    map.insert("(", "(");
    map.insert(")", ")");
    map.insert("<(", "<(");
    map.insert("&&", "&&");
    map.insert("||", "||");
    map.insert(";", ";");

    // Common paths
    map.insert(".", ".");
    map.insert("..", "..");
    map.insert("~", "~");
    map.insert("/", "/");

    // Empty strings
    map.insert("\"\"", "\"\"");
    map.insert("''", "''");

    map
});

/// Get a fixed string reference if available
pub fn get_fixed_string(key: &str) -> Option<&'static str> {
    FIXED_STRINGS.get(key).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_string_optimization_context_creation() {
        let ctx = StringOptimizationContext::new();
        assert!(ctx.stats.interned_strings > 0);

        let (tilde_size, path_size, pool_size) = ctx.cache_info();
        assert_eq!(tilde_size, 0);
        assert_eq!(path_size, 0);
        assert_eq!(pool_size, 0);
    }

    #[test]
    fn test_tilde_expansion_caching() {
        let mut ctx = StringOptimizationContext::new();

        // First call should miss cache
        let _result1 = ctx.expand_tilde_cached("~/test");
        assert_eq!(ctx.stats.tilde_cache_misses, 1);
        assert_eq!(ctx.stats.tilde_cache_hits, 0);

        // Second call should hit cache
        let _result2 = ctx.expand_tilde_cached("~/test");
        assert_eq!(ctx.stats.tilde_cache_misses, 1);
        assert_eq!(ctx.stats.tilde_cache_hits, 1);
    }

    #[test]
    fn test_trim_cow_optimization() {
        // No trimming needed
        let input = "hello";
        let result = StringOptimizationContext::trim_cow(input);
        assert!(matches!(result, Cow::Borrowed(_)));

        // Trimming needed
        let input = "  hello  ";
        let result = StringOptimizationContext::trim_cow(input);
        assert!(matches!(result, Cow::Borrowed(_)));
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_string_pool() {
        let mut ctx = StringOptimizationContext::new();

        // Get a string from pool
        let s1 = ctx.get_pooled_string(10);
        assert_eq!(ctx.stats.string_pool_allocations, 1);
        assert_eq!(ctx.stats.string_pool_reuses, 0);

        // Return it to pool
        ctx.return_to_pool(s1);

        // Get another string (should reuse)
        let _s2 = ctx.get_pooled_string(10);
        assert_eq!(ctx.stats.string_pool_allocations, 1);
        assert_eq!(ctx.stats.string_pool_reuses, 1);
    }

    #[test]
    fn test_interned_strings() {
        let ctx = StringOptimizationContext::new();

        // Common strings should be interned
        assert!(ctx.get_interned_string("ls").is_some());
        assert!(ctx.get_interned_string("cd").is_some());
        assert!(ctx.get_interned_string("|").is_some());
        assert!(ctx.get_interned_string("&").is_some());

        // Uncommon strings should not be interned
        assert!(ctx.get_interned_string("very_uncommon_string").is_none());
    }

    #[test]
    fn test_fixed_strings() {
        assert_eq!(get_fixed_string("&"), Some("&"));
        assert_eq!(get_fixed_string("|"), Some("|"));
        assert_eq!(get_fixed_string("("), Some("("));
        assert_eq!(get_fixed_string(")"), Some(")"));
        assert_eq!(get_fixed_string("nonexistent"), None);
    }

    #[test]
    fn test_glob_root_caching() {
        let mut ctx = StringOptimizationContext::new();

        // Test basic functionality
        let (_root1, _glob1) = ctx.find_glob_root_cached("/home/user/*.txt");
        assert_eq!(ctx.stats.path_cache_misses, 1);

        // Second call should hit cache
        let (_root2, _glob2) = ctx.find_glob_root_cached("/home/user/*.txt");
        assert_eq!(ctx.stats.path_cache_hits, 1);
    }

    #[test]
    fn test_statistics() {
        let mut ctx = StringOptimizationContext::new();

        // Generate some cache activity
        ctx.expand_tilde_cached("~/test1");
        ctx.expand_tilde_cached("~/test1"); // cache hit
        ctx.expand_tilde_cached("~/test2");

        let stats = ctx.stats();
        assert_eq!(stats.tilde_cache_hits, 1);
        assert_eq!(stats.tilde_cache_misses, 2);
        assert_eq!(stats.tilde_hit_rate(), 1.0 / 3.0);

        // Reset stats
        ctx.reset_stats();
        let stats = ctx.stats();
        assert_eq!(stats.tilde_cache_hits, 0);
        assert_eq!(stats.tilde_cache_misses, 0);
    }
}
