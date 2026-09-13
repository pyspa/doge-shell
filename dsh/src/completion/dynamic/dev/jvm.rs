//! Maven `pom.xml` parsing: profile and module names via a tiny ad-hoc XML
//! tag scanner (no XML crate dependency for two tag shapes).
use super::*;

pub(super) fn find_maven_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| ancestor.join("pom.xml").is_file())
        .map(Path::to_path_buf)
}

pub(super) fn load_maven_profiles(pom_file: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(pom_file) else {
        return Vec::new();
    };
    dedup_sorted(
        xml_blocks(&contents, "profile")
            .into_iter()
            .flat_map(|block| xml_tag_values(block, "id"))
            .collect(),
    )
}

pub(super) fn load_maven_modules(pom_file: &Path) -> Vec<String> {
    let Ok(contents) = fs::read_to_string(pom_file) else {
        return Vec::new();
    };
    dedup_sorted(
        xml_blocks(&contents, "modules")
            .into_iter()
            .flat_map(|block| xml_tag_values(block, "module"))
            .collect(),
    )
}

pub(super) fn xml_blocks<'a>(contents: &'a str, tag: &str) -> Vec<&'a str> {
    let mut blocks = Vec::new();
    let mut rest = contents;
    let open_prefix = format!("<{tag}");
    let close = format!("</{tag}>");
    while let Some(start) = rest.find(&open_prefix) {
        let after_start = &rest[start..];
        let Some(open_end) = after_start.find('>') else {
            break;
        };
        let after_open = &after_start[open_end + 1..];
        let Some(close_start) = after_open.find(&close) else {
            break;
        };
        blocks.push(&after_open[..close_start]);
        rest = &after_open[close_start + close.len()..];
    }
    blocks
}

pub(super) fn xml_tag_values(contents: &str, tag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut rest = contents;
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    while let Some(start) = rest.find(&open) {
        let after_open = &rest[start + open.len()..];
        let Some(end) = after_open.find(&close) else {
            break;
        };
        let value = after_open[..end].trim();
        if !value.is_empty() && !value.contains('<') {
            values.push(value.to_string());
        }
        rest = &after_open[end + close.len()..];
    }
    values
}
