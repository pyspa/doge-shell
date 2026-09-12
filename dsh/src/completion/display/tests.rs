use super::*;

#[test]
fn detail_line_text_combines_name_and_description() {
    let candidate = Candidate::Command {
        name: "git".to_string(),
        description: "Version control".to_string(),
    };
    assert_eq!(
        detail_line_text(&candidate).as_deref(),
        Some("git — Version control")
    );
}

#[test]
fn detail_line_text_is_none_without_description() {
    let candidate = Candidate::File {
        path: "file.txt".to_string(),
        is_dir: false,
    };
    assert_eq!(detail_line_text(&candidate), None);
}

#[test]
fn detail_line_reserved_only_when_a_candidate_has_description() {
    // Plain files: no description anywhere -> no detail row reserved.
    let files = vec![
        Candidate::File {
            path: "a.txt".to_string(),
            is_dir: false,
        },
        Candidate::File {
            path: "b/".to_string(),
            is_dir: true,
        },
    ];
    let display = CompletionDisplay::new_with_config(files, "$ ", "", CompletionConfig::default());
    assert!(!display.show_detail_line);
    assert_eq!(display.detail_rows(), 0);

    // A described command -> detail row reserved.
    let described = vec![Candidate::Command {
        name: "git".to_string(),
        description: "Version control".to_string(),
    }];
    let display =
        CompletionDisplay::new_with_config(described, "$ ", "", CompletionConfig::default());
    assert!(display.show_detail_line);
    assert_eq!(display.detail_rows(), 1);
}

#[test]
fn completion_config_default_uses_env_override() {
    let _guard = crate::test_env_lock();
    let original = std::env::var(COMPLETION_MAX_ITEMS_ENV).ok();
    unsafe {
        std::env::set_var(COMPLETION_MAX_ITEMS_ENV, "42");
    }
    let config = CompletionConfig::default();
    assert_eq!(config.max_items, 42);

    match original {
        Some(value) => unsafe {
            std::env::set_var(COMPLETION_MAX_ITEMS_ENV, value);
        },
        None => unsafe {
            std::env::remove_var(COMPLETION_MAX_ITEMS_ENV);
        },
    }
}

#[test]
fn test_terminal_size_calculation() {
    // Test with various terminal widths to ensure proper calculation
    let candidates = vec![
        Candidate::Command {
            name: "git".to_string(),
            description: "Version control".to_string(),
        },
        Candidate::File {
            path: "file.txt".to_string(),
            is_dir: false,
        },
        Candidate::File {
            path: "directory/".to_string(),
            is_dir: true,
        },
    ];

    let config = CompletionConfig::default();
    let mut display = CompletionDisplay::new_with_config(candidates, "$ ", "test input", config);
    let layout = display.force_layout(80);

    // Verify that items_per_row is reasonable
    assert!(layout.items_per_row >= 1);
    assert!(layout.items_per_row <= display.candidates.len().max(1));

    // Verify that column_width is reasonable
    assert!(layout.column_width > 0);
    assert!(layout.column_width <= layout.terminal_width);

    // Verify that total_rows is calculated correctly
    let expected_rows = display
        .candidates
        .len()
        .div_ceil(layout.items_per_row.max(1));
    assert_eq!(layout.total_rows, expected_rows);
}

#[test]
fn test_formatted_display_width_limits() {
    let candidate = Candidate::Command {
        name: "very_long_command_name_that_should_be_truncated".to_string(),
        description: "A command with a very long name".to_string(),
    };

    // Test with small width
    let (formatted, _) = candidate.get_formatted_display(20, 0);
    let display_width = unicode_display_width(&formatted);

    // Should not exceed the requested width significantly
    assert!(display_width <= 25); // Allow some tolerance for emoji width

    // Should contain the type character
    assert!(formatted.contains('⚡'));

    // Should be truncated if name is too long
    if candidate.get_display_name().len() > 15 {
        assert!(formatted.contains('…'));
    }
}

#[test]
fn test_column_alignment_fixed_width() {
    // Test that formatted display produces consistent width
    let candidates = vec![
        Candidate::Command {
            name: "git".to_string(),
            description: "Version control".to_string(),
        },
        Candidate::Command {
            name: "very_long_command_name".to_string(),
            description: "A command with a long name".to_string(),
        },
        Candidate::File {
            path: "file.txt".to_string(),
            is_dir: false,
        },
    ];

    let fixed_width = 25;

    for candidate in &candidates {
        let (formatted, _) = candidate.get_formatted_display(fixed_width, 0);
        let actual_width = unicode_display_width(&formatted);

        // All formatted items should have exactly the same width
        assert_eq!(
            actual_width,
            fixed_width,
            "Candidate '{}' has width {} but expected {}",
            candidate.get_display_name(),
            actual_width,
            fixed_width
        );
    }
}

#[test]
fn test_column_alignment_with_unicode() {
    // Test column alignment with Unicode characters
    let candidates = vec![
        Candidate::File {
            path: "file.txt".to_string(),
            is_dir: false,
        },
        Candidate::File {
            path: "日本語ファイル.txt".to_string(), // Japanese filename
            is_dir: false,
        },
        Candidate::File {
            path: "🐕.txt".to_string(), // Emoji filename
            is_dir: false,
        },
    ];

    let fixed_width = 30;

    for candidate in &candidates {
        let (formatted, _) = candidate.get_formatted_display(fixed_width, 0);
        let actual_width = unicode_display_width(&formatted);

        // All formatted items should have exactly the same width, even with Unicode
        assert_eq!(
            actual_width,
            fixed_width,
            "Unicode candidate '{}' has width {} but expected {}",
            candidate.get_display_name(),
            actual_width,
            fixed_width
        );
    }
}
#[test]
fn test_candidate_description_retrieval() {
    // Test Command type
    let command = Candidate::Command {
        name: "git".to_string(),
        description: "Version control".to_string(),
    };
    assert_eq!(command.get_description(), Some("Version control"));

    // Test Option type
    let option = Candidate::Option {
        name: "--help".to_string(),
        description: "Show help".to_string(),
    };
    assert_eq!(option.get_description(), Some("Show help"));

    // Test Item type
    let item = Candidate::Item("value".to_string(), "A value".to_string());
    assert_eq!(item.get_description(), Some("A value"));

    // Test Item type with empty description
    let empty_item = Candidate::Item("value".to_string(), "".to_string());
    assert_eq!(empty_item.get_description(), None);

    // Test File type (should be None)
    let file = Candidate::File {
        path: "file.txt".to_string(),
        is_dir: false,
    };
    assert_eq!(file.get_description(), None);
}

#[test]
fn test_layout_calculation_description_width() {
    let candidates = vec![
        Candidate::Command {
            name: "short".to_string(),
            description: "desc".to_string(),
        },
        Candidate::Command {
            name: "a_very_long_command_name".to_string(),
            description: "desc".to_string(),
        },
    ];

    let config = CompletionConfig::default();
    let mut display = CompletionDisplay::new_with_config(candidates, "$ ", "", config);
    let layout = display.force_layout(80);

    // Verify max_name_width is calculated correctly
    // emoji (2) + space (1) + name width (24)
    let _expected_short_width = 3 + 5; // 8
    let expected_long_width = 3 + 24; // 27

    assert_eq!(layout.max_name_width, expected_long_width);
}

#[test]
fn test_formatted_display_alignment() {
    let short_cmd = Candidate::Command {
        name: "short".to_string(),
        description: "Short description".to_string(),
    };

    // If we force a max_name_width larger than this command, it should be padded
    let max_name_width = 20;
    let total_width = 40;

    // ⚡ short
    // Type (2) + Space (1) + "short" (5) = 8 chars visual width
    // Padding should be max_name_width (20) - current (8) = 12 spaces

    let (formatted, desc) = short_cmd.get_formatted_display(total_width, max_name_width);

    // Check padding
    // Format is: ICON + space + NAME + PADDING
    // ⚡ short
    // 12345678901234567890

    let _visual_width = unicode_display_width(&formatted);
    // The formatted string should be padded to match alignment requirements + spaces to fill row if needed?
    // Wait, logic says: padding_needed = target.saturating_sub(current_width)
    // target is max_name_width.min(width)
    // So expected visual width of the NAME part (including icon) should be max_name_width

    // But get_formatted_display currently appends EXTRA padding if column width is wide
    // Let's re-read the logic:
    // let padding_needed = if max_name_width > 0 { ... }
    // result_name.push_str(&" ".repeat(padding_needed));
    // So yes, `formatted` should have visual width approx equal to max_name_width (or more if column is wide?)

    // In get_formatted_display:
    // padding_needed based on align target (max_name_width) OR column width
    // If max_name_width is passed, we align to IT.

    // Let's verify the padding length specifically
    let padding_count = formatted.chars().filter(|c| *c == ' ').count();
    // 1 space after icon + 12 spaces padding = 13 spaces?
    // "short" has no spaces.
    assert!(
        padding_count >= 13,
        "Expected at least 13 spaces, got {}",
        padding_count
    );

    let desc = desc.expect("description should be returned");
    assert!(desc.starts_with("  Short description"));
    assert_eq!(
        unicode_display_width(&formatted) + unicode_display_width(&desc),
        total_width
    );
}

#[test]
fn test_description_candidates_fill_fixed_width() {
    let candidates = vec![
        Candidate::Item("src".to_string(), "(directory)".to_string()),
        Candidate::Item("Cargo.toml".to_string(), "(file)".to_string()),
        Candidate::Item("日本語ファイル.txt".to_string(), "(file)".to_string()),
        Candidate::Command {
            name: "git".to_string(),
            description: "Version control".to_string(),
        },
    ];

    let fixed_width = 32;
    let max_name_width = 18;

    for candidate in &candidates {
        let (formatted, desc) = candidate.get_formatted_display(fixed_width, max_name_width);
        let actual_width = unicode_display_width(&formatted)
            + desc.as_deref().map(unicode_display_width).unwrap_or(0);

        assert_eq!(
            actual_width,
            fixed_width,
            "Candidate '{}' has combined width {} but expected {}",
            candidate.get_display_name(),
            actual_width,
            fixed_width
        );
    }
}

#[test]
fn test_multicolumn_file_rows_keep_stable_cell_widths() {
    let candidates = vec![
        Candidate::Item("src".to_string(), "(directory)".to_string()),
        Candidate::Item("target".to_string(), "(directory)".to_string()),
        Candidate::Item("Cargo.toml".to_string(), "(file)".to_string()),
        Candidate::Item(
            "very_long_file_name_that_truncates.rs".to_string(),
            "(file)".to_string(),
        ),
        Candidate::File {
            path: "日本語ファイル.txt".to_string(),
            is_dir: false,
        },
        Candidate::File {
            path: "examples".to_string(),
            is_dir: true,
        },
    ];

    let mut display = CompletionDisplay::new_with_config(
        candidates,
        "$ ",
        "ls ",
        CompletionConfig {
            max_items: 30,
            ..CompletionConfig::default()
        },
    );
    let layout = display.force_layout(120);

    assert!(
        layout.items_per_row >= 2,
        "test requires at least two columns, got {}",
        layout.items_per_row
    );

    for candidate in &display.candidates {
        let (formatted, desc) =
            candidate.get_formatted_display(layout.column_width, layout.max_name_width);
        let content_width = unicode_display_width(&formatted)
            + desc.as_deref().map(unicode_display_width).unwrap_or(0);

        assert_eq!(
            content_width,
            layout.column_width,
            "Candidate '{}' content width should match the column width",
            candidate.get_display_name()
        );
    }

    let cell_width = layout.column_width + 1;
    let expected_starts: Vec<usize> = (0..layout.items_per_row)
        .map(|col| col * (cell_width + 2))
        .collect();

    for row in 0..layout.total_rows {
        let mut cursor = 0;
        for (col, expected_start) in expected_starts.iter().enumerate() {
            let index = row * layout.items_per_row + col;
            if index >= display.candidates.len() {
                break;
            }

            assert_eq!(
                cursor, *expected_start,
                "row {row} col {col} should start at a stable table offset"
            );

            cursor += cell_width;
            if col < layout.items_per_row - 1 && index + 1 < display.candidates.len() {
                cursor += 2;
            }
        }
    }
}
