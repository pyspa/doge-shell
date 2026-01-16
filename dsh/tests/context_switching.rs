use doge_shell::completion::command::{
    Argument, ArgumentType, CommandCompletion, CommandCompletionDatabase, CommandOption, SubCommand,
};
use doge_shell::completion::generator::CompletionGenerator;
use doge_shell::completion::parser::CommandLineParser;

#[tokio::test]
async fn test_context_switching_logic() {
    // Create a mock DB to have controlled environment
    let mut db = CommandCompletionDatabase::new();
    let mock_completion = CommandCompletion {
        command: "mock".to_string(),
        description: None,
        global_options: vec![CommandOption {
            short: None,
            long: Some("--global".to_string()),
            description: None,
            argument: None,
        }],
        subcommands: vec![SubCommand {
            name: "sub".to_string(),
            description: None,
            options: vec![CommandOption {
                short: None,
                long: Some("--local".to_string()),
                description: None,
                argument: None,
            }],
            arguments: vec![Argument {
                name: "arg1".to_string(),
                description: None,
                arg_type: Some(ArgumentType::File { extensions: None }),
            }],
            subcommands: vec![SubCommand {
                name: "nested".to_string(),
                description: None,
                options: vec![],
                arguments: vec![],
                subcommands: vec![],
            }],
        }],
        arguments: vec![],
    };
    db.add_command(mock_completion);

    let parser = CommandLineParser::new();
    let generator = CompletionGenerator::new(&db);

    // 1. "mock " -> SubCommand context.
    // Should show "sub", NOT "--global"
    let input1 = "mock ";
    let parsed1 = parser.parse(input1, input1.len());
    let candidates1 = generator.generate_candidates(&parsed1).unwrap();
    assert!(
        candidates1.iter().any(|c| c.text == "sub"),
        "1: Should have 'sub'"
    );
    assert!(
        !candidates1.iter().any(|c| c.text == "--global"),
        "1: Should NOT have '--global' without dash"
    );

    // 2. "mock -" -> Option context (Global).
    // Should show "--global", NOT "sub"
    let input2 = "mock -";
    let parsed2 = parser.parse(input2, input2.len());
    let candidates2 = generator.generate_candidates(&parsed2).unwrap();
    assert!(
        candidates2.iter().any(|c| c.text == "--global"),
        "2: Should have '--global'"
    );
    assert!(
        !candidates2.iter().any(|c| c.text == "sub"),
        "2: Should NOT have 'sub'"
    );
    assert!(
        !candidates2.iter().any(|c| c.text == "--local"),
        "2: Should NOT have '--local'"
    );

    // 3. "mock sub " -> SubCommand context (Nested).
    // Should show "nested", NOT "--local" (unless dash)
    let input3 = "mock sub ";
    let parsed3 = parser.parse(input3, input3.len());
    let candidates3 = generator.generate_candidates(&parsed3).unwrap();
    assert!(
        candidates3.iter().any(|c| c.text == "nested"),
        "3: Should have 'nested'"
    );
    assert!(
        !candidates3.iter().any(|c| c.text == "--local"),
        "3: Should NOT have '--local' without dash"
    );

    // 4. "mock sub -" -> Option context (Local + Global).
    // Should show "--local" and "--global"
    let input4 = "mock sub -";
    let parsed4 = parser.parse(input4, input4.len());
    let candidates4 = generator.generate_candidates(&parsed4).unwrap();
    assert!(
        candidates4.iter().any(|c| c.text == "--local"),
        "4: Should have '--local'"
    );
    assert!(
        candidates4.iter().any(|c| c.text == "--global"),
        "4: Should have '--global'"
    );
    assert!(
        !candidates4.iter().any(|c| c.text == "nested"),
        "4: Should NOT have 'nested'"
    );

    // 5. "mock sub arg" -> Argument context.
    // Should show file candidates (mock behavior), NOT options
    let input5 = "mock sub arg";
    let parsed5 = parser.parse(input5, input5.len());
    let candidates5 = generator.generate_candidates(&parsed5).unwrap();
    // Assuming file generator works or returns distinct types.
    // Just verify NO options.
    assert!(
        !candidates5.iter().any(|c| c.text == "--local"),
        "5: Should NOT have '--local'"
    );

    // 6. "mock invalid -" -> Option context (Global).
    // "invalid" is treated as argument to "mock" (invalid subcommand).
    // But since current token is "-", it should be Option context.
    let input6 = "mock invalid -";
    let parsed6 = parser.parse(input6, input6.len());
    // Verify generator logic correction
    let candidates6 = generator.generate_candidates(&parsed6).unwrap();

    assert!(
        candidates6.iter().any(|c| c.text == "--global"),
        "6: Should have '--global' even after invalid subcommand"
    );
    assert!(
        !candidates6.iter().any(|c| c.text == "sub"),
        "6: Should NOT have 'sub'"
    );
}
