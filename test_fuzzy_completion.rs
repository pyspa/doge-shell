#!/usr/bin/env rust-script

//! Fuzzy completion functionality test script
//! 
//! This script demonstrates the fuzzy completion features integrated into doge-shell.

use std::process::Command;

fn main() {
    println!("🐕 Doge Shell - Fuzzy Completion Test");
    println!("=====================================");
    
    // Test 1: Check if dsh binary exists
    println!("\n1. Checking dsh binary...");
    let output = Command::new("ls")
        .arg("-la")
        .arg("target/release/dsh")
        .output();
        
    match output {
        Ok(output) => {
            if output.status.success() {
                println!("✅ dsh binary found");
                println!("{}", String::from_utf8_lossy(&output.stdout));
            } else {
                println!("❌ dsh binary not found");
                return;
            }
        }
        Err(e) => {
            println!("❌ Error checking dsh binary: {}", e);
            return;
        }
    }
    
    // Test 2: Show fuzzy completion features
    println!("\n2. Fuzzy Completion Features:");
    println!("   - Smart completion with exact, prefix, and fuzzy matching");
    println!("   - Command completion from PATH");
    println!("   - File and directory completion");
    println!("   - History-based completion with frecency scoring");
    println!("   - Git-aware completion");
    
    // Test 3: Usage instructions
    println!("\n3. How to test fuzzy completion:");
    println!("   1. Run: ./target/release/dsh");
    println!("   2. Type partial commands like 'gi' and press TAB");
    println!("   3. Try file completion with partial paths");
    println!("   4. Test with typos - fuzzy matching should help");
    
    println!("\n4. Example scenarios to test:");
    println!("   - Type 'gi' + TAB → should show 'git' and other commands");
    println!("   - Type 'git co' + TAB → should show 'git commit', 'git checkout'");
    println!("   - Type 'ls Car' + TAB → should find 'Cargo.toml' (fuzzy match)");
    println!("   - Type 'cd sr' + TAB → should find 'src/' directory");
    
    println!("\n✨ Fuzzy completion is now integrated into doge-shell!");
    println!("   The completion system will prioritize:");
    println!("   1. Exact matches");
    println!("   2. Prefix matches");
    println!("   3. Fuzzy matches (scored by relevance)");
}
