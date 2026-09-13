//! Computing the completion grid's geometry: columns per row, column width (aligned to the longest name), and total row count, cached until the terminal width or candidate list changes (`force_layout`/
//! `ensure_layout`).
use super::*;

impl CompletionDisplay {
    #[cfg(test)]
    pub(crate) fn force_layout(&mut self, terminal_width: usize) -> LayoutCache {
        let cache = self.calculate_layout(terminal_width);
        self.layout_cache = Some(cache.clone());
        self.layout_dirty = false;
        cache
    }
    fn calculate_layout(&self, terminal_width: usize) -> LayoutCache {
        let mut max_name_width = 0;
        let mut max_total_width = 0;

        for c in &self.candidates {
            let name_width = unicode_display_width(c.get_display_name());
            let type_char_width = c.get_type_char().width().unwrap_or(2);
            let desc = c.get_description();
            let desc_width = if let Some(d) = desc {
                unicode_display_width(d) + 2 // +2 for spacing/separator
            } else {
                0
            };

            let full_name_width = name_width + type_char_width + 1; // type + space + name
            max_name_width = max_name_width.max(full_name_width);
            max_total_width = max_total_width.max(full_name_width + desc_width);
        }

        let max_display_width = max_total_width.max(10);

        debug!(
            "Layout calc: max_name_width={}, max_total_width={}",
            max_name_width, max_display_width
        );

        // Reserve space for selection indicator ("> " or "  ") and inter-column spacing
        let selection_indicator_width = 1; // ">" or " "
        let inter_column_spacing = 2; // Space between columns

        // Calculate effective column width including all necessary spacing
        let effective_column_width = max_display_width + selection_indicator_width;

        // Calculate how many items can fit per row, accounting for spacing between columns
        let items_per_row = if effective_column_width > 0 {
            let available_width = terminal_width.saturating_sub(4); // Reserve 4 chars margin for safety
            let width_per_item = effective_column_width + inter_column_spacing;

            // Calculate maximum items that can fit
            let max_items =
                std::cmp::max(1, available_width.checked_div(width_per_item).unwrap_or(0));

            std::cmp::min(max_items, self.candidates.len().max(1))
        } else {
            1
        };

        // Recalculate column width based on actual items per row to ensure proper fit
        let column_width = if items_per_row > 0 {
            let available_width = terminal_width.saturating_sub(4); // Reserve margin
            let total_spacing = (items_per_row.saturating_sub(1)) * inter_column_spacing;
            let width_for_content = available_width.saturating_sub(total_spacing);
            width_for_content.max(1) / items_per_row.max(1)
        } else {
            terminal_width.saturating_sub(4)
        };

        let total_rows = self.candidates.len().div_ceil(items_per_row.max(1));

        debug!(
            "Display layout: terminal_width={}, column_width={}, items_per_row={}, total_rows={}",
            terminal_width, column_width, items_per_row, total_rows
        );

        LayoutCache {
            terminal_width,
            column_width,
            max_name_width,
            items_per_row: items_per_row.max(1),
            total_rows,
        }
    }
    pub(super) fn ensure_layout(&mut self, terminal_width: usize) {
        let needs_recalc = self.layout_dirty
            || self
                .layout_cache
                .as_ref()
                .is_none_or(|cache| cache.terminal_width != terminal_width);
        if needs_recalc {
            let cache = self.calculate_layout(terminal_width);
            self.layout_cache = Some(cache);
            self.layout_dirty = false;
        }
    }
    pub(super) fn layout(&self) -> &LayoutCache {
        if let Some(layout) = self.layout_cache.as_ref() {
            layout
        } else {
            warn!("layout cache missing during render; using fallback layout");
            &FALLBACK_LAYOUT
        }
    }
}
