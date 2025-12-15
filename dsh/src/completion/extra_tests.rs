#[cfg(test)]
mod tests {
    use crate::completion::parser::CommandLineParser;

    #[test]
    fn test_cursor_in_middle_of_token() {
        let parser = CommandLineParser::new();
        // "git sta|tus" -> cursor at index 7 (after 'a')
        // Expected: current_token should be "sta" (prefix), not "status"
        let input = "git status";
        let cursor_pos = 7;
        let result = parser.parse(input, cursor_pos);

        // Current behavior suspicion: it returns "status"
        // Desired behavior: "sta"
        assert_eq!(
            result.current_token, "sta",
            "Token should be truncated to cursor position"
        );
    }

    #[test]
    fn test_cursor_between_tokens() {
        let parser = CommandLineParser::new();
        // "git | add" -> cursor at index 4
        // Expected: We are inserting between 'git' and 'add'.
        // Logic should recognize we are completing a NEW token at position 1 (index 1).
        let input = "git  add";
        let cursor_pos = 4;
        let result = parser.parse(input, cursor_pos);

        // Current behavior suspicion: returns cursor_token_index pointing to end?
        // Let's verify what index it thinks we are at.
        // "git" is token 0. "add" is token 1.
        // We are between them, so we are essentially at token 1 (shifting "add" to 2).

        // If the parser ignores whitespace and just sees ["git", "add"],
        // and find_cursor_token_index returns spans.len() (2), it thinks we are at the end.

        // We can check completion context. 'git' is command. 'add' is subcommand.
        // If we are between them, we are looking for a subcommand (or argument).
        // But crucially, if it thinks we are at end, it thinks we are AFTER 'add'.

        // This test asserts the logic handles insertion correctly.
        // If we are inserting, we expect current_token to be empty string, BUT
        // the structural context (args/subcommands) should reflect we are *before* 'add'.

        // However, ParsedCommandLine structure flattens everything.
        // Let's look at `subcommand_path`.
        // If we are at end: ["add"]
        // If we are between: [] because "add" hasn't been parsed as part of the path *before* cursor?
        // Actually, if we are editing text, the parser usually parses the *whole* line.
        // But for completion, we care about the state *at the cursor*.
        // If the parser consumes "add" into subcommand_path, it implies "add" is already typed *before* the cursor context?
        // No, `analyze_tokens` iterates through `tokens_queue`.

        // If cursor_token_index is 2 (end), `analyze_tokens` processes `tokens[0]` ("git") then iter loop processes "add".
        // It pushes "add" to subcommand_path.
        // Then it determines context at index 2.
        // So `subcommand_path` will be `["add"]`.

        // BUT we are typing *before* add. The context should NOT include "add" in the path leading up to cursor.
        assert!(
            result.subcommand_path.is_empty(),
            "Subcommand path should not include tokens after cursor"
        );
    }
}
