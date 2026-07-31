use super::dedup_sorted;

pub(super) fn parse_images(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .map(|line| line.trim())
            .filter(|image| !image.contains("<none>"))
            .map(str::to_string)
            .collect(),
    )
}
