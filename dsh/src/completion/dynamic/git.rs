use super::dedup_sorted;

pub(super) fn parse_remote_branches(lines: &[String], remote: Option<&str>) -> Vec<String> {
    let mut values = Vec::new();
    for line in lines {
        if line.ends_with("/HEAD") || line == "HEAD" {
            continue;
        }
        let Some((candidate_remote, branch)) = line.split_once('/') else {
            continue;
        };
        if branch.is_empty() {
            continue;
        }
        if let Some(remote) = remote
            && !remote.is_empty()
            && candidate_remote != remote
        {
            continue;
        }
        values.push(branch.to_string());
    }
    dedup_sorted(values)
}

pub(super) fn parse_stash_refs(lines: &[String]) -> Vec<String> {
    dedup_sorted(
        lines
            .iter()
            .filter_map(|line| line.split(':').next().map(str::to_string))
            .collect(),
    )
}

pub(super) fn parse_status_porcelain_paths(output: &str) -> Vec<String> {
    let records = output
        .split('\0')
        .filter(|record| !record.is_empty())
        .collect::<Vec<_>>();
    let mut values = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        if record.len() < 4 {
            index += 1;
            continue;
        }
        let status = &record[..2];
        let path = record[3..].trim();
        if !path.is_empty() {
            values.push(path.to_string());
        }
        if status.contains('R') || status.contains('C') {
            index += 2;
        } else {
            index += 1;
        }
    }
    dedup_sorted(values)
}
