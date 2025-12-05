use dsh::completion::json_loader::JsonCompletionLoader;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

fn main() {
    // Test what the actual count is
    let loader = JsonCompletionLoader::new();
    let completions = loader.list_available_completions().unwrap();
    println!("Total available completions: {}", completions.len());
    println!("First 10 completions: {:?}", &completions[0..std::cmp::min(10, completions.len())]);
    
    // Test with temp directory like the failing test
    let temp_dir = TempDir::new().unwrap();
    fs::write(temp_dir.path().join("git.json"), "{}").unwrap();
    fs::write(temp_dir.path().join("cargo.json"), "{}").unwrap();
    fs::write(temp_dir.path().join("not_json.txt"), "{}").unwrap();

    let loader2 = JsonCompletionLoader::with_dirs(vec![temp_dir.path().to_path_buf()]);
    let completions2 = loader2.list_available_completions().unwrap();
    println!("Available completions with temp dir: {}", completions2.len());
    println!("Completions from temp dir test: {:?}", &completions2[0..std::cmp::min(5, completions2.len())]);
}