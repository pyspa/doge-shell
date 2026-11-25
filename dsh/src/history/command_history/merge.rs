//! Reconcile the database's timestamp/id order with ordered local occurrences.
use super::*;

pub(super) enum CountMerge {
    // A new batch or startup delta contains only additional occurrences.
    Add,
    // A pending snapshot can already include the database baseline.
    Pending { advance_tie: bool, failed: i64 },
}

/// Keep the source order within each second. New occurrences in a tie follow
/// baseline rows. Older occurrences can increment a count without moving a row.
pub(super) fn merge_entries(
    stored: Vec<Entry>,
    updates: impl Iterator<Item = (usize, Entry, CountMerge)>,
) -> Vec<Entry> {
    let mut updates: HashMap<_, _> = updates
        .map(|(order, entry, mode)| (entry.entry.clone(), (order, entry, mode)))
        .collect();
    if updates.is_empty() {
        return stored;
    }

    let offset = stored.len();
    let mut merged = Vec::with_capacity(offset + updates.len());
    for (order, mut old) in stored.into_iter().enumerate() {
        let Some((local_order, mut local, mode)) = updates.remove(&old.entry) else {
            merged.push((order, old));
            continue;
        };

        let (count, advance_tie) = match mode {
            CountMerge::Add => (old.count + local.count, true),
            CountMerge::Pending {
                advance_tie,
                failed,
            } => {
                if local.when == old.when && !advance_tie {
                    local.context = local.context.or(old.context.take());
                    local.exit_code = local.exit_code.or(old.exit_code);
                    local.duration_ms = local.duration_ms.or(old.duration_ms);
                    local.cwd = local.cwd.or(old.cwd.take());
                    local.session_id = local.session_id.or(old.session_id.take());
                    local.hostname = local.hostname.or(old.hostname.take());
                }
                ((old.count + failed).max(local.count), advance_tie)
            }
        };
        if local.when < old.when {
            old.count = count;
            merged.push((order, old));
        } else {
            let promote = local.when > old.when || advance_tie;
            local.count = count;
            merged.push((if promote { offset + local_order } else { order }, local));
        }
    }
    merged.extend(
        updates
            .into_values()
            .map(|(i, entry, _)| (offset + i, entry)),
    );
    merged.sort_unstable_by_key(|(order, entry)| (entry.when, *order));
    merged.into_iter().map(|(_, entry)| entry).collect()
}
