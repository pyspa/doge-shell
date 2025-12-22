#[cfg(test)]
mod tests {
    use crate::{FrecencyStore, SortMethod};

    #[test]
    fn test_search_prefix_range() {
        let mut store = FrecencyStore::default();
        // Add items in unsorted order (add method auto-inserts in sorted order)
        store.add("apple", None);
        store.add("apricot", None);
        store.add("banana", None);
        store.add("cherry", None);
        store.add("date", None);
        store.add("fig", None);
        store.add("grape", None);

        // Verify sorted order invariant
        assert_eq!(store.items[0].item, "apple");
        assert_eq!(store.items[1].item, "apricot");
        assert_eq!(store.items[2].item, "banana");

        // 1. Exact prefix match returning multiple items
        let range = store.search_prefix_range("ap");
        assert_eq!(range, 0..2); // apple, apricot
        assert_eq!(store.items[range.clone()].len(), 2);
        assert_eq!(store.items[range.start].item, "apple");

        // 2. Exact match single item
        let range = store.search_prefix_range("banana");
        assert_eq!(range, 2..3);
        assert_eq!(store.items[range.start].item, "banana");

        // 3. No match (prefix comes after all items)
        let range = store.search_prefix_range("zebra");
        assert_eq!(range.start, range.end);
        assert_eq!(range.start, store.items.len());

        // 4. No match (prefix comes before all items)
        let range = store.search_prefix_range("aardvark");
        assert_eq!(range.start, range.end);
        assert_eq!(range.start, 0);

        // 5. No match (prefix would be in middle)
        let range = store.search_prefix_range("cantaloupe");
        assert_eq!(range.start, range.end);
        // "banana" < "cantaloupe" < "cherry"
        // apple(0), apricot(1), banana(2), cherry(3)
        assert_eq!(range.start, 3);

        // 6. Empty prefix (should match all)
        let range = store.search_prefix_range("");
        assert_eq!(range, 0..store.items.len());
    }

    #[test]
    fn test_truncate_restores_order() {
        let mut store = FrecencyStore::default();
        store.add("b", None);
        store.add("a", None);
        store.add("c", None);

        // Initial state: a, b, c
        assert_eq!(store.items[0].item, "a");
        assert_eq!(store.items[1].item, "b");
        assert_eq!(store.items[2].item, "c");

        // Manipulate scores to change sort order
        // "b" -> high score
        store.adjust("b", 100.0);

        // Truncate keeping 2, sorted by Frequent.
        // Expected freq sort: b, a, c (if a,c 0) or similar.
        // Actually adjust adds 1.0 weight.
        // b: >100, a: 1, c: 1.

        // This will temporarily sort by Frequent (b, a/c, a/c), take top 2 (b, a/c), then restore name sort.
        // Result should be (a, b) or (b, c) depending on tie breaking, but definitely name-sorted.
        store.truncate(2, &SortMethod::Frequent);

        assert_eq!(store.items.len(), 2);

        // Verify invariant: items are sorted by name
        assert!(store.items[0].item < store.items[1].item);

        // "b" should definitely be there
        assert!(store.items.iter().any(|i| i.item == "b"));
    }
}
