//! Terraform workspace discovery from local `.terraform/` state.
use super::*;

pub(super) fn find_terraform_root(current_dir: &Path) -> Option<PathBuf> {
    let cwd = current_dir
        .canonicalize()
        .unwrap_or_else(|_| current_dir.to_path_buf());
    cwd.ancestors()
        .find(|ancestor| ancestor.join(".terraform").is_dir())
        .map(Path::to_path_buf)
}

pub(super) fn load_terraform_workspaces(root: &Path) -> Vec<String> {
    let terraform_dir = root.join(".terraform");
    let mut values = vec!["default".to_string()];
    if let Ok(current) = fs::read_to_string(terraform_dir.join("environment")) {
        let current = current.trim();
        if !current.is_empty() {
            values.push(current.to_string());
        }
    }
    let state_dir = terraform_dir.join("terraform.tfstate.d");
    if let Ok(entries) = fs::read_dir(state_dir) {
        values.extend(
            entries
                .flatten()
                .filter(|entry| entry.path().is_dir())
                .filter_map(|entry| entry.file_name().to_str().map(str::to_string)),
        );
    }
    dedup_sorted(values)
}
