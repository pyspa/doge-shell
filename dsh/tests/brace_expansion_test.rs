use doge_shell::environment::Environment;
use doge_shell::parser::expansion::expand_alias;

#[test]
fn test_brace_expansion_basic() {
    let env = Environment::new();
    let input = "echo {a,b,c}".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo a b c");
}

#[test]
fn test_brace_expansion_pre_post() {
    let env = Environment::new();
    let input = "echo pre{X,Y}post".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo preXpost preYpost");
}

#[test]
fn test_brace_expansion_nested() {
    let env = Environment::new();
    let input = "echo a{b,c{d,e}}".to_string();
    let result = expand_alias(input, env).unwrap();
    // Order depends on implementation, but typically {b,c{d,e}} -> b, c{d,e} -> cd, ce
    // So: ab, acd, ace
    assert_eq!(result, "echo ab acd ace");
}

#[test]
fn test_brace_expansion_nested_deep() {
    let env = Environment::new();
    let input = "echo {a,b{c,d{e,f}}}".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo a bc bde bdf");
}

#[test]
fn test_brace_expansion_multiple_groups() {
    let env = Environment::new();
    // Shell usually expands as cartesian product but here we treat each word separately if they are separate words.
    // "a{1,2} b{x,y}" are two words.
    let input = "echo a{1,2} b{x,y}".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo a1 a2 bx by");
}

#[test]
fn test_brace_expansion_cartesian() {
    let env = Environment::new();
    // One word with two braces: "{a,b}{1,2}" -> a{1,2} b{1,2} -> a1 a2 b1 b2
    let input = "echo {a,b}{1,2}".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo a1 a2 b1 b2");
}

#[test]
fn test_brace_expansion_no_comma() {
    let env = Environment::new();
    // "{a}" is often not expanded in some shells if it has no comma, but technically it's a valid brace group with 1 element?
    // Bash: {a} -> {a} (literal) if no comma? Or {a} -> a?
    // Bash behaves: echo {a} -> {a}. echo {a,b} -> a b.
    // Our implementation splits by comma. If no comma, parts len is 1.
    // If parts len is 1, loop runs once. `expand_braces` returns literal?
    // Let's check logic:
    // while j < content_chars ... if c == ',' ...
    // If no comma, `parts` has 1 element.
    // `for part in parts` -> recurses.
    // It will effectively remove braces. {a} -> a.
    // This is arguably cleaner than bash. Let's accept this behavior or verify.
    // dsh philosophy: simple.
    let input = "echo {a}".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo a");
}

#[test]
fn test_brace_expansion_empty() {
    let env = Environment::new();
    // "{,}" -> "", ""
    let input = "echo start{,b}end".to_string();
    let result = expand_alias(input, env).unwrap();
    assert_eq!(result, "echo startend startbend");
}
