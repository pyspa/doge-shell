use super::expansion::expand_alias;
use super::{Rule, ShellParser};
use crate::environment::Environment;
use anyhow::Result;
use pest::Parser;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[test]
fn test_literal_bracket() -> Result<()> {
    init();
    let input = "foo]bar";
    // This should parse as a simple word (or glob_word without specials)
    // Currently expected to FAIL if parser relies on ] exclusion
    let pairs = ShellParser::parse(Rule::simple_command, input);

    if let Err(e) = pairs {
        println!("Parse failed as expected: {}", e);
        // We actually want this to SUCCEED in the fixed version.
        // For verify step, we assert failure or success depending on what we test.
        // Here we assert it parses successfully because a shell MUST support this.
        panic!("Failed to parse literal bracket: {}", e);
    }

    let mut pairs = pairs.unwrap();
    let pair = pairs.next().unwrap();
    assert_eq!(Rule::simple_command, pair.as_rule());

    // Check inner structure
    // It should be argv0 -> span -> word (or glob_word)
    // With current pest, likely fails before here.
    Ok(())
}

#[test]
fn test_unclosed_bracket() -> Result<()> {
    init();
    let input = "val[1";
    // Should parse. globmatch might fail or return literal.
    // If it parses, we check expansion.
    let _pairs = ShellParser::parse(Rule::simple_command, input)?;
    let env = Environment::new();
    let expanded = expand_alias(input.to_string(), std::sync::Arc::clone(&env))?;
    // If no match, it should return original string
    assert_eq!(expanded, "val[1");
    Ok(())
}
