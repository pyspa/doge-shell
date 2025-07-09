use crossterm::{
    cursor,
    event::{Event, KeyCode, KeyEvent, read},
    execute,
    terminal::{Clear, ClearType},
    style::{Color, Print, ResetColor, SetForegroundColor},
};
use std::io::{stdout, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut stdout = stdout();
    
    // Get terminal size
    let (width, height) = crossterm::terminal::size()?;
    println!("Terminal size: {}x{}", width, height);
    
    // Get current cursor position
    let (col, row) = cursor::position()?;
    println!("Current cursor position: col={}, row={}", col, row);
    
    // Calculate available space below
    let available_below = height.saturating_sub(row + 1);
    println!("Available rows below: {}", available_below);
    
    // Test creating space
    if available_below < 5 {
        println!("Creating space below...");
        
        // Clear current line
        execute!(stdout, cursor::MoveToColumn(0), Clear(ClearType::CurrentLine))?;
        print!("Test prompt> test input");
        
        // Add empty lines
        for i in 0..(5 - available_below) {
            print!("\n");
            println!("Added line {}", i + 1);
        }
        
        // Move cursor back
        let (new_col, new_row) = cursor::position()?;
        println!("New cursor position: col={}, row={}", new_col, new_row);
        
        // Move back to input position
        execute!(stdout, cursor::MoveTo(col + 18, row))?; // 18 = length of "Test prompt> test "
    }
    
    // Display some test completion candidates
    execute!(stdout, cursor::MoveToNextLine(1))?;
    
    let test_candidates = vec![
        "candidate1", "candidate2", "candidate3", "candidate4", "candidate5"
    ];
    
    for (i, candidate) in test_candidates.iter().enumerate() {
        if i == 0 {
            execute!(stdout, SetForegroundColor(Color::Yellow))?;
            print!("> ");
        } else {
            print!("  ");
        }
        
        execute!(stdout, SetForegroundColor(Color::Green))?;
        print!("C {}", candidate);
        execute!(stdout, ResetColor)?;
        
        if i < test_candidates.len() - 1 {
            execute!(stdout, cursor::MoveToNextLine(1))?;
        }
    }
    
    // Show scroll indicator
    execute!(stdout, cursor::MoveToNextLine(1))?;
    execute!(stdout, SetForegroundColor(Color::DarkGrey))?;
    print!("(5/5 rows)");
    execute!(stdout, ResetColor)?;
    
    stdout.flush()?;
    
    println!("\nPress any key to continue...");
    let _ = read()?;
    
    Ok(())
}
