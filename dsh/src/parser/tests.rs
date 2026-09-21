use super::ast::{get_pos_word, get_string};
use super::expansion::rewrite_aliases;
use super::{Rule, ShellParser};
use crate::environment::Environment;
use anyhow::Result;
use pest::Parser;
use std::cell::RefCell;

use std::rc::Rc;
use std::sync::Arc;
use tracing::debug;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

/// Descend one `commands` child to its `command` nodes.
///
/// `commands` yields `and_or_list` nodes (plus `list_separator`s); a test
/// that wants the commands unwraps one level here.
fn child_commands(pair: pest::iterators::Pair<Rule>) -> Vec<pest::iterators::Pair<Rule>> {
    if pair.as_rule() == Rule::and_or_list {
        pair.into_inner()
            .filter(|inner| inner.as_rule() == Rule::command)
            .collect()
    } else {
        vec![pair]
    }
}

type JobLink = Rc<RefCell<Job>>;

#[derive(Debug)]
#[allow(dead_code)]
pub struct Job {
    name: String,
    next: Option<JobLink>,
}

impl Job {
    fn new(name: String) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self { name, next: None }))
    }
}

#[test]
fn parse_word() {
    init();
    let pairs = ShellParser::parse(Rule::word, "a1bc").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::word, pair.as_rule());
    }
}

#[test]
fn parse_quoted() {
    init();
    let pairs = ShellParser::parse(Rule::quoted, "\'a1bc\'").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::s_quoted, pair.as_rule());
        assert_eq!("a1bc", get_string(pair).unwrap());
    }
    let pairs = ShellParser::parse(Rule::quoted, "\"a1bc\"").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::d_quoted, pair.as_rule());
        assert_eq!("a1bc", get_string(pair).unwrap());
    }
}

#[test]
fn parse_argv0() {
    init();
    let pairs = ShellParser::parse(Rule::argv0, "a1bc").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::argv0, pair.as_rule());
    }
}

#[test]
fn parse_args1() {
    init();
    let pairs = ShellParser::parse(Rule::args, " a1bc b2").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::args, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);
        for inner_pair in pair.into_inner() {
            assert_eq!(Rule::span, inner_pair.as_rule());
        }
    }
}

#[test]
fn parse_args2() {
    init();
    // `args` is only ever entered after `argv0`, so it starts at the space
    // before the first argument -- same shape as `parse_args1`. Adjacent parts
    // now join into one span, so the separator is what marks a new argument.
    let pairs =
        ShellParser::parse(Rule::args, r#" echo "test""#).unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::args, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);
        for (i, inner_pair) in pair.into_inner().enumerate() {
            if i == 0 {
                assert_eq!(Rule::span, inner_pair.as_rule());
                assert_eq!("echo", get_string(inner_pair).unwrap());
            } else {
                assert_eq!(Rule::span, inner_pair.as_rule());
                assert_eq!("test", get_string(inner_pair).unwrap());
            }
        }
    }
}

#[test]
fn parse_simple_command1() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, "test --a1bc --b2=c3  ")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());

        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::argv0 => {
                    let cmd = inner_pair.as_str();
                    assert_eq!("test", cmd);
                }
                Rule::args => {
                    for inner_pair in inner_pair.into_inner() {
                        assert_eq!(Rule::span, inner_pair.as_rule());
                    }
                }

                _ => {}
            }
        }
    }
}

#[test]
fn parse_simple_command2() {
    init();
    let pairs =
        ShellParser::parse(Rule::simple_command, "  test   ").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(1, count);

        for inner_pair in pair.into_inner() {
            if inner_pair.as_rule() == Rule::argv0 {
                let cmd = inner_pair.as_str();
                assert_eq!("test", cmd);
            }
        }
    }
}

#[test]
fn parse_simple_command_with_input_redirect() {
    init();
    let mut pairs = ShellParser::parse(Rule::simple_command, "cat < input.txt")
        .unwrap_or_else(|e| panic!("{}", e));

    let command = pairs.next().expect("simple_command");
    assert_eq!(Rule::simple_command, command.as_rule());

    let mut inner_pairs = command.into_inner();
    let argv0 = inner_pairs.next().expect("argv0");
    assert_eq!(Rule::argv0, argv0.as_rule());
    assert_eq!("cat", argv0.as_str());

    let args = inner_pairs.next().expect("args");
    assert_eq!(Rule::args, args.as_rule());

    let mut args_inner = args.into_inner();
    let redirect = args_inner.next().expect("redirect");
    assert_eq!(Rule::redirect, redirect.as_rule());

    let mut redirect_inner = redirect.into_inner();
    let direction = redirect_inner.next().expect("stdin redirect direction");
    assert_eq!(Rule::stdin_redirect_direction, direction.as_rule());

    let target = redirect_inner.next().expect("redirect target span");
    assert_eq!(Rule::span, target.as_rule());
    assert_eq!("input.txt", target.as_str());

    assert!(redirect_inner.next().is_none());
    assert!(args_inner.next().is_none());
    assert!(inner_pairs.next().is_none());
    assert!(pairs.next().is_none());
}

#[test]
fn expand_alias_preserves_input_redirect() {
    init();
    let env = Environment::new();
    // Alias rewriting touches only static argv0 spans; redirects pass through.
    let result = rewrite_aliases("cat < input.txt", env).expect("alias rewrite succeeds");
    assert_eq!(result, "cat < input.txt");
}

#[test]
fn parse_simple_command3() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, r#"echo abc " test" '-vvv' --foo "#)
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        // let argv = get_argv(pair);
        // assert_eq!(5, argv.len());
        // assert_eq!("echo", argv[0].0);
        // assert_eq!("abc", argv[1].0);
        // assert_eq!(" test", argv[2].0);
        // assert_eq!("-vvv", argv[3].0);
        // assert_eq!("--foo", argv[4].0);
    }
}

#[test]
fn parse_simple_command4() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, r#"sk -q "" "#)
        .unwrap_or_else(|e| panic!("{}", e));

    let mut v = vec![];
    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        for pair in pair.into_inner() {
            if let Rule::args = pair.as_rule() {
                for pair in pair.into_inner() {
                    debug!("arg:'{}'", pair.as_str());
                    v.push(pair.as_str().to_string());
                }
            }
        }
        // assert_eq!(5, argv.len());
        // assert_eq!("echo", argv[0].0);
        // assert_eq!("abc", argv[1].0);
        // assert_eq!(" test", argv[2].0);
        // assert_eq!("-vvv", argv[3].0);
        // assert_eq!("--foo", argv[4].0);
    }

    debug!("{}", v.join(" "));
}

#[test]
fn parse_command1() {
    init();
    let pairs = ShellParser::parse(Rule::command, "history | sk --ansi --inline-info ")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());

        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let cmd = inner_pair.as_str();
                    assert_eq!("history", cmd);
                }
                Rule::pipe_command => {
                    let inner_pair = inner_pair.into_inner();
                    let cmd = inner_pair.as_str();
                    assert_eq!("| sk --ansi --inline-info", cmd);
                }
                _ => {}
            }
        }
    }
}

#[test]
fn parse_command2() {
    init();
    let pairs = ShellParser::parse(Rule::command, "history|test  --a1bc --b2=c3|dd  ")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());

        let count = pair.clone().into_inner().count();
        assert_eq!(3, count);

        for (i, inner_pair) in pair.into_inner().enumerate() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let cmd = inner_pair.as_str();
                    if i == 0 {
                        assert_eq!("history", cmd);
                    }
                }
                Rule::pipe_command => {
                    let inner_pair = inner_pair.into_inner();
                    let cmd = inner_pair.as_str();
                    if i == 1 {
                        assert_eq!("|test  --a1bc --b2=c3", cmd);
                    } else if i == 2 {
                        assert_eq!("|dd", cmd);
                    }
                }

                _ => {}
            }
        }
    }
}

#[test]
fn parse_command3() {
    init();
    let pairs = ShellParser::parse(Rule::command, "history").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());

        let count = pair.clone().into_inner().count();
        assert_eq!(1, count);

        for (i, inner_pair) in pair.into_inner().enumerate() {
            if inner_pair.as_rule() == Rule::simple_command {
                let cmd = inner_pair.as_str();
                if i == 0 {
                    assert_eq!("history", cmd);
                }
            }
        }
    }
}

#[test]
fn parse_command4() {
    init();
    let pairs = ShellParser::parse(Rule::command, "history | sk | bash -s")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());

        let count = pair.clone().into_inner().count();
        assert_eq!(3, count);

        for (i, inner_pair) in pair.into_inner().enumerate() {
            debug!("{:?}", inner_pair.as_rule());
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let cmd = inner_pair.as_str();
                    if i == 0 {
                        assert_eq!("history", cmd);
                    }
                }
                Rule::pipe_command => {
                    let inner_pair = inner_pair.into_inner();
                    let cmd = inner_pair.as_str();
                    if i == 1 {
                        assert_eq!("| sk", cmd);
                    } else if i == 2 {
                        assert_eq!("| bash -s", cmd);
                    }
                }

                _ => {}
            }
        }
    }
}

#[test]
fn parse_command_sp() {
    init();
    // A `command` always starts from a `simple_command`, so whitespace alone
    // is not a command. Execution maps it to an empty plan before parsing.
    assert!(ShellParser::parse(Rule::command, "   ").is_err());
    assert!(ShellParser::parse(Rule::commands, "   ").is_err());
}

#[test]
fn parse_background_list_separator() {
    init();
    // `&` is a list separator between AND-OR lists, not a command suffix.
    let pairs = ShellParser::parse(Rule::commands, "sleep 20 & sleep 30 &")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::commands, pair.as_rule());
        let inners: Vec<_> = pair.into_inner().collect();
        // and_or_list, list_separator, and_or_list, list_separator (trailing).
        assert_eq!(4, inners.len());
        assert_eq!(Rule::and_or_list, inners[0].as_rule());
        assert_eq!("sleep 20", inners[0].as_str().trim());
        assert_eq!(Rule::list_separator, inners[1].as_rule());
        assert_eq!(Rule::and_or_list, inners[2].as_rule());
        assert_eq!("sleep 30", inners[2].as_str().trim());
        assert_eq!(Rule::list_separator, inners[3].as_rule());
    }
}

#[test]
fn parse_command_bg() {
    init();
    // One `command` never spans `&`: each side is its own AND-OR list.
    let pairs = ShellParser::parse(Rule::commands, "sleep 20 & sleep 30")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::commands, pair.as_rule());
        let inners: Vec<_> = pair.into_inner().collect();
        assert_eq!(3, inners.len());
        assert_eq!(Rule::and_or_list, inners[0].as_rule());
        assert_eq!(Rule::list_separator, inners[1].as_rule());
        assert_eq!("&", inners[1].as_str().trim());
        assert_eq!(Rule::and_or_list, inners[2].as_rule());
    }
}

#[test]
fn test_get_pos_word1() -> Result<()> {
    init();
    let input = "sudo git st aaa &";
    let res = get_pos_word(input, 1)?;
    assert_eq!("sudo", res.unwrap().1.as_str());

    let res = get_pos_word(input, 5)?;
    assert_eq!(None, res);

    let res = get_pos_word(input, 6)?;
    assert_eq!("git", res.unwrap().1.as_str());

    let input = "sudo ";
    let res = get_pos_word(input, 1)?;
    assert_eq!("sudo", res.unwrap().1.as_str());

    let input = "sudo git st ( docker ps -a -q) &";
    let res = get_pos_word(input, 15)?;
    assert_eq!("docker", res.unwrap().1.as_str());
    assert_eq!(Rule::argv0, res.unwrap().0);

    Ok(())
}

#[test]
fn test_get_pos_word2() -> Result<()> {
    init();
    let input = "mv *.toml ";
    let res = get_pos_word(input, 9)?;
    println!("{:?}", res.unwrap().0);
    assert_eq!("*.toml", res.unwrap().1.as_str());

    Ok(())
}

/// Every operator has to come back from the expansion round trip.
///
/// The expander re-serializes the line and the shell parses that, so an
/// operator its match did not name simply disappeared: `a | b &` ran in the
/// foreground and `(a; b)` collapsed into one command.
#[test]
fn expand_alias_preserves_operators() -> Result<()> {
    init();
    let env = crate::environment::Environment::new();
    env.write()
        .variable_state
        .variables
        .insert("$FOO".to_string(), "bar".to_string());

    // Alias rewriting leaves the rest of the line byte-for-byte intact, so
    // operators and `$FOO` spellings survive untouched.
    for (input, operator) in [
        ("echo $FOO | cat &", "&"),
        ("echo $FOO | cat", "|"),
        ("echo $FOO ; echo b", ";"),
        ("echo $FOO && echo b", "&&"),
        ("echo $FOO || echo b", "||"),
        ("echo $FOO |>", "|>"),
        ("(echo $FOO | cat)", "|"),
        ("(echo $FOO ; echo b)", ";"),
        ("(echo $FOO && echo b)", "&&"),
        ("(echo $FOO | cat &)", "&"),
    ] {
        let replaced = rewrite_aliases(input, Arc::clone(&env))?;
        assert!(
            replaced.contains(operator),
            "{input:?} lost {operator:?}: {replaced:?}"
        );
    }

    Ok(())
}

#[test]
fn test_expand_alias() -> Result<()> {
    init();
    let env = crate::environment::Environment::new();

    env.write()
        .variable_state
        .alias
        .insert("alias".to_string(), "echo 'test' | sk ".to_string());
    env.write()
        .variable_state
        .variables
        .insert("$FOO".to_string(), "BAR".to_string());

    // Span-based rewriting replaces only the argv0 span; the remaining
    // quoting and `$FOO` spellings stay exactly as typed for runtime.
    let input = r#"alias abc " test" '-vvv' --foo "#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(
        replaced.as_ref(),
        r#"echo 'test' | sk  abc " test" '-vvv' --foo "#.to_string()
    );

    let input = r#"alias abc " test" '-vvv' --foo &"#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(
        replaced.as_ref(),
        r#"echo 'test' | sk  abc " test" '-vvv' --foo &"#.to_string()
    );

    // The trailing `&` belongs to the last pipeline stage and has to survive
    // the rewrite untouched.
    let input = r#"alias | abc " test" '-vvv' --foo &"#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(
        replaced.as_ref(),
        r#"echo 'test' | sk  | abc " test" '-vvv' --foo &"#.to_string()
    );

    let input = r#"sh -c | alias " test" '-vvv' --foo &"#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(
        replaced.as_ref(),
        r#"sh -c | echo 'test' | sk  " test" '-vvv' --foo &"#.to_string()
    );

    let input = r#"echo (alias " test" '-vvv' --foo) "#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(
        replaced.as_ref(),
        r#"echo (echo 'test' | sk  " test" '-vvv' --foo) "#.to_string()
    );
    // `$FOO` is runtime data: alias rewriting must not resolve it.
    let input = r#"echo $FOO"#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(replaced.as_ref(), r#"echo $FOO"#.to_string());

    let input = r#"echo 'test' > test.log"#;
    let replaced = rewrite_aliases(input, Arc::clone(&env))?;
    assert_eq!(replaced.as_ref(), r#"echo 'test' > test.log"#.to_string());

    Ok(())
}

#[test]
fn test_simple_alias_like_ll() -> Result<()> {
    init();
    let env = crate::environment::Environment::new();

    env.write()
        .variable_state
        .alias
        .insert("ll".to_string(), "exa -al".to_string());
    env.write()
        .variable_state
        .alias
        .insert("g".to_string(), "git".to_string());

    // Test simple alias 'll'
    let input = r#"ll"#.to_string();
    let replaced = rewrite_aliases(&input, Arc::clone(&env))?;
    assert_eq!(replaced.as_ref(), r#"exa -al"#.to_string());

    // Test alias with arguments
    let input = r#"ll -h"#.to_string();
    let replaced = rewrite_aliases(&input, Arc::clone(&env))?;
    assert_eq!(replaced.as_ref(), r#"exa -al -h"#.to_string());

    // Test single letter alias
    let input = r#"g status"#.to_string();
    let replaced = rewrite_aliases(&input, Arc::clone(&env))?;
    assert_eq!(replaced.as_ref(), r#"git status"#.to_string());

    Ok(())
}

#[test]
fn parse_commands() {
    init();
    let pairs = ShellParser::parse(Rule::commands, "sleep 10 ; echo 'test' ")
        .unwrap_or_else(|e| panic!("{}", e));

    let mut result: Option<JobLink> = None;
    let mut root: Option<JobLink> = None;
    // let mut result: Option<JobLink> = None;

    for pair in pairs {
        for pair in pair.into_inner() {
            match pair.as_rule() {
                Rule::and_or_list => {
                    for pair in pair.into_inner() {
                        match pair.as_rule() {
                            Rule::command => {
                                debug!("{:?} {:?}", pair.as_rule(), pair.as_str());
                                let job = Job::new(pair.as_str().to_string());
                                match result.take() {
                                    Some(prev) => {
                                        prev.borrow_mut().next = Some(Rc::clone(&job));
                                        result = Some(Rc::clone(&job));
                                    }
                                    None => {
                                        result = Some(Rc::clone(&job));
                                        root = Some(Rc::clone(&job));
                                    }
                                }
                            }
                            Rule::and_or_op => {}
                            _ => {}
                        }
                    }
                }
                Rule::list_separator => {}
                _ => {}
            }
        }
    }

    debug!("{:?}", root);
}

#[test]
fn parse_subshell() {
    init();
    let pairs = ShellParser::parse(Rule::commands, "sudo docker rm -v (sudo docker ps -a -q)")
        .unwrap_or_else(|e| panic!("{}", e));

    let mut found_subshell = false;
    for pair in pairs {
        for pair in pair.into_inner() {
            for pair in child_commands(pair) {
                match pair.as_rule() {
                    Rule::command => {
                        for pair in pair.into_inner() {
                            match pair.as_rule() {
                                Rule::simple_command => {
                                    for pair in pair.into_inner() {
                                        match pair.as_rule() {
                                            Rule::argv0 => {}
                                            Rule::args => {
                                                for pair in pair.into_inner() {
                                                    if pair.as_rule() == Rule::span {
                                                        for pair in pair.into_inner() {
                                                            if pair.as_rule() == Rule::subshell {
                                                                assert_eq!(
                                                                    pair.as_str(),
                                                                    "(sudo docker ps -a -q)"
                                                                );
                                                                found_subshell = true;
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                    _ => {
                        println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }
    }
    assert!(found_subshell);
}

#[test]
fn parse_subshell2() {
    init();
    let sub = "(ls -al | wc -l)";
    let cmd = format!("echo {sub}");
    let pairs = ShellParser::parse(Rule::commands, &cmd).unwrap_or_else(|e| panic!("{}", e));

    let mut found_subshell = false;
    for pair in pairs {
        for pair in pair.into_inner() {
            for pair in child_commands(pair) {
                match pair.as_rule() {
                    Rule::command => {
                        for pair in pair.into_inner() {
                            match pair.as_rule() {
                                Rule::simple_command => {
                                    for pair in pair.into_inner() {
                                        match pair.as_rule() {
                                            Rule::argv0 => {}
                                            Rule::args => {
                                                for pair in pair.into_inner() {
                                                    if pair.as_rule() == Rule::span {
                                                        for pair in pair.into_inner() {
                                                            if pair.as_rule() == Rule::subshell {
                                                                assert_eq!(pair.as_str(), sub);
                                                                found_subshell = true;
                                                                println!("{}", pair.as_str());
                                                                for pair in pair.into_inner() {
                                                                    println!(
                                                                        "{:?} {:?}",
                                                                        pair.as_rule(),
                                                                        pair.as_str()
                                                                    );
                                                                    for pair in pair.into_inner() {
                                                                        println!(
                                                                            "{:?} {:?}",
                                                                            pair.as_rule(),
                                                                            pair.as_str()
                                                                        );
                                                                        for pair in
                                                                            pair.into_inner()
                                                                        {
                                                                            println!(
                                                                                "{:?} {:?}",
                                                                                pair.as_rule(),
                                                                                pair.as_str()
                                                                            );
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                    _ => {
                        println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }
    }
    assert!(found_subshell);
}

#[test]
fn parse_proc_subst() {
    init();
    let pairs =
        ShellParser::parse(Rule::commands, "echo <(ls)").unwrap_or_else(|e| panic!("{}", e));

    let mut found_proc_subst = false;
    for pair in pairs {
        for pair in pair.into_inner() {
            for pair in child_commands(pair) {
                match pair.as_rule() {
                    Rule::command => {
                        for pair in pair.into_inner() {
                            match pair.as_rule() {
                                Rule::simple_command => {
                                    for pair in pair.into_inner() {
                                        match pair.as_rule() {
                                            Rule::argv0 => {}
                                            Rule::args => {
                                                for pair in pair.into_inner() {
                                                    if pair.as_rule() == Rule::span {
                                                        for pair in pair.into_inner() {
                                                            if pair.as_rule() == Rule::proc_subst {
                                                                assert_eq!(pair.as_str(), "<(ls)");
                                                                found_proc_subst = true;
                                                            }
                                                        }
                                                    }
                                                }
                                            }

                                            _ => {}
                                        }
                                    }
                                }
                                _ => {
                                    println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                                }
                            }
                        }
                    }
                    _ => {
                        println!("unknown {:?} {:?}", pair.as_rule(), pair.as_str());
                    }
                }
            }
        }
    }
    assert!(found_proc_subst);
}

#[test]
fn test_exec_subshell() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, r#"sleep (echo 1) "#)
        .unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        // let argv = get_argv(pair);
        // assert_eq!(2, argv.len());
        // assert_eq!("sleep", argv[0].0);
        // assert_eq!("(echo 1)", argv[1].0);
    }
}

#[test]
fn test_variable() {
    init();
    let mut find = false;
    let pairs = ShellParser::parse(Rule::simple_command, r#"sleep $foo "#)
        .unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);
        for pair in pair.into_inner() {
            if pair.as_rule() == Rule::args {
                for pair in pair.into_inner() {
                    for pair in pair.into_inner() {
                        assert_eq!(Rule::variable, pair.as_rule());
                        assert_eq!("$foo", pair.as_str());

                        find = true;
                    }
                }
            }
        }
    }

    assert!(find);
}

#[test]
fn test_redirect() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, r#"echo "f" > test.log "#)
        .unwrap_or_else(|e| panic!("{}", e));
    let mut found = false;
    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        // println!("* {:?} {:?}", pair.as_rule(), pair.as_str());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);
        for pair in pair.into_inner() {
            if pair.as_rule() == Rule::args {
                for pair in pair.into_inner() {
                    // println!("** {:?} {:?}", pair.as_rule(), pair.as_str());
                    let parent = pair.as_rule();
                    if parent == Rule::redirect {
                        for pair in pair.into_inner() {
                            println!("*** {:?} {:?}", pair.as_rule(), pair.as_str());
                            found = true;
                        }
                    }
                }
            }
        }
    }
    assert!(found);
}

#[test]
fn test_redirect2() {
    init();
    let pairs = ShellParser::parse(Rule::command, r#"ls -al | wc -l > test.log "#)
        .unwrap_or_else(|e| panic!("{}", e));
    let mut found = false;
    for pair in pairs {
        // println!("* {:?} {:?}", pair.as_rule(), pair.as_str());
        for pair in pair.into_inner() {
            // println!("** {:?} {:?}", pair.as_rule(), pair.as_str());
            for pair in pair.into_inner() {
                // println!("*** {:?} {:?}", pair.as_rule(), pair.as_str());
                if pair.as_rule() == Rule::simple_command {
                    for pair in pair.into_inner() {
                        if pair.as_rule() == Rule::args {
                            for pair in pair.into_inner() {
                                // println!(
                                //     "**** {:?} {:?}",
                                //     pair.as_rule(),
                                //     pair.as_str()
                                // );
                                let parent = pair.as_rule();
                                if parent == Rule::redirect {
                                    for _pair in pair.into_inner() {
                                        // println!(
                                        //     "**** {:?} {:?}",
                                        //     pair.as_rule(),
                                        //     pair.as_str()
                                        // );
                                        found = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(found);
}

#[test]
fn parse_glob() {
    init();
    let pairs = ShellParser::parse(Rule::glob_word, "~/Downloads/*.pdf")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::glob_word, pair.as_rule());
        assert_eq!("~/Downloads/*.pdf", get_string(pair).unwrap());
    }

    let pairs = ShellParser::parse(Rule::simple_command, "ls ~/Downloads/*.pdf")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        debug!("{:?} {}", pair.as_rule(), pair.as_str());
        assert_eq!(Rule::simple_command, pair.as_rule());
        if Rule::simple_command == pair.as_rule() {
            for pair in pair.into_inner() {
                debug!("{:?} {}", pair.as_rule(), pair.as_str());
                if Rule::args == pair.as_rule() {
                    for pair in pair.into_inner() {
                        debug!("{:?} {}", pair.as_rule(), pair.as_str());
                        if Rule::span == pair.as_rule() {
                            for pair in pair.into_inner() {
                                debug!("{:?} {}", pair.as_rule(), pair.as_str());
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn test_get_string_safety() {
    // Construct a mock Pair that mimics the structure causing the panic
    // Rule::span with empty inner
    // Since we can't easily construct a Pair manually without parsing,
    // we'll rely on our fix being correct by code inspection or trying to parse input that triggers it.
    // However, triggering it via parse might be hard if the grammar enforces it.
    // But the previous code had an unwrap() on pair.into_inner().next().
    // If pest guarantees next() exists for Rule::span, then the unwrap was safe (but bad practice).
    // If not, our fix handles it.

    // Let's at least test that get_string works for normal inputs.
    let input = "\"test\"";
    let mut pairs = ShellParser::parse(Rule::d_quoted, input).unwrap();
    let pair = pairs.next().unwrap();
    assert_eq!(get_string(pair), Some("test".to_string()));

    let input = "'test'";
    let mut pairs = ShellParser::parse(Rule::s_quoted, input).unwrap();
    let pair = pairs.next().unwrap();
    assert_eq!(get_string(pair), Some("test".to_string()));
}

#[test]
fn test_brace_expansion_unit() -> Result<()> {
    init();
    // Brace expansion is runtime-only now: alias rewriting leaves the source
    // spelling intact for the materializer.
    let env = crate::environment::Environment::new();
    for input in [
        "echo {a,b,c}",
        "echo pre{X,Y}post",
        "echo a{b,c{d,e}}",
        "echo {a,b}{1,2}",
        "echo a{1,2} b{x,y}",
        "echo {a}",
        "echo {*.test_dummy_1,*.test_dummy_2}",
    ] {
        let replaced = rewrite_aliases(input, Arc::clone(&env))?;
        assert_eq!(replaced.as_ref(), input);
    }

    // Pure brace helper behavior stays pinned here; runtime composition is
    // covered by shell word expansion tests.
    use super::expansion::expand_braces;
    assert_eq!(expand_braces("{a,b,c}"), vec!["a", "b", "c"]);
    assert_eq!(expand_braces("{a}"), vec!["a"]);

    Ok(())
}

/// One expansion of `pattern` against a freshly built directory tree.
///
/// `absent` is what must *not* come back; it is the only reason the
/// character-class case needs its own row rather than sharing one.
struct GlobCase {
    what: &'static str,
    dirs: &'static [&'static str],
    files: &'static [&'static str],
    pattern: &'static str,
    expected_len: usize,
    contains: &'static [&'static str],
    absent: &'static [&'static str],
}

#[test]
fn glob_patterns_expand_against_the_current_directory() -> Result<()> {
    init();
    use std::fs::{self, File};

    let cases = [
        GlobCase {
            what: "* matches every file with the suffix",
            dirs: &[],
            files: &["glob_test_a.txt", "glob_test_b.txt"],
            pattern: "*.txt",
            expected_len: 2,
            contains: &["glob_test_a.txt", "glob_test_b.txt"],
            absent: &[],
        },
        GlobCase {
            what: "? matches exactly one character",
            dirs: &[],
            files: &["file1.txt", "fileA.txt"],
            pattern: "file?.txt",
            expected_len: 2,
            contains: &["file1.txt", "fileA.txt"],
            absent: &[],
        },
        GlobCase {
            what: "a character class matches only its members",
            dirs: &[],
            files: &["file1.txt", "file2.txt", "fileA.txt"],
            pattern: "file[0-9].txt",
            expected_len: 2,
            contains: &["file1.txt", "file2.txt"],
            absent: &["fileA.txt"],
        },
        GlobCase {
            what: "a pattern may name a subdirectory",
            dirs: &["sub"],
            files: &["sub/test.rs"],
            pattern: "sub/*.rs",
            expected_len: 1,
            contains: &["sub", "test.rs"],
            absent: &[],
        },
        GlobCase {
            what: "** descends through every subdirectory",
            dirs: &["sub", "sub/nested"],
            files: &["root.rs", "sub/sub.rs", "sub/nested/deep.rs"],
            pattern: "**/*.rs",
            expected_len: 3,
            contains: &["root.rs", "sub.rs", "deep.rs"],
            absent: &[],
        },
    ];

    for case in cases {
        let dir = tempfile::tempdir()?;
        for sub in case.dirs {
            fs::create_dir(dir.path().join(sub))?;
        }
        for file in case.files {
            File::create(dir.path().join(file))?;
        }

        use super::expansion::expand_glob_pattern;
        for _pair in ShellParser::parse(Rule::glob_word, case.pattern)
            .unwrap_or_else(|e| panic!("{}: {}", case.what, e))
        {
            let expanded = expand_glob_pattern(case.pattern, dir.path());
            assert_eq!(
                expanded.len(),
                case.expected_len,
                "{}: expanding '{}' gave {:?}",
                case.what,
                case.pattern,
                expanded
            );

            let joined = expanded.join(" ");
            for name in case.contains {
                assert!(
                    joined.contains(name),
                    "{}: expanding '{}' should offer '{}', got {:?}",
                    case.what,
                    case.pattern,
                    name,
                    expanded
                );
            }
            for name in case.absent {
                assert!(
                    !joined.contains(name),
                    "{}: expanding '{}' must not offer '{}', got {:?}",
                    case.what,
                    case.pattern,
                    name,
                    expanded
                );
            }
        }
    }

    Ok(())
}

#[test]
fn test_glob_no_match() -> Result<()> {
    init();
    use std::fs::File;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("file.txt");
    File::create(&path)?;

    use super::expansion::expand_glob_pattern;
    // Pattern matches nothing: it comes back as itself for runtime argv.
    let expanded = expand_glob_pattern("*.rs", dir.path());
    assert_eq!(expanded.len(), 1);
    assert_eq!(expanded[0], "*.rs");
    Ok(())
}

#[test]
fn parse_struct_pipe_simple() {
    init();
    let pairs = ShellParser::parse(Rule::command, "cat data.json |: (json-parse $_)")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let cmd = inner_pair.as_str();
                    assert_eq!("cat data.json", cmd);
                }
                Rule::struct_pipe_command => {
                    let inner_pair = inner_pair.into_inner();
                    let cmd = inner_pair.as_str();
                    assert!(cmd.contains("|:"));
                    assert!(cmd.contains("(json-parse $_)"));
                }
                _ => {}
            }
        }
    }
}

#[test]
fn parse_struct_pipe_chain() {
    init();
    let pairs = ShellParser::parse(
        Rule::command,
        "kubectl get pods -o json |: (get $_ \"items\") |: (head $_ 5)",
    )
    .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(3, count); // simple_command + 2 struct_pipe_commands
    }
}

#[test]
fn parse_struct_pipe_with_nested_parens() {
    init();
    let pairs = ShellParser::parse(
        Rule::command,
        "echo test |: (where $_ (lambda (r) (get r \"active\")))",
    )
    .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);
    }
}

#[test]
fn parse_struct_pipe_mixed_with_regular_pipe() {
    init();
    let pairs = ShellParser::parse(Rule::command, "cat data.json |: (json-parse $_) | head")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(3, count); // simple_command + struct_pipe + pipe_command

        let mut found_struct_pipe = false;
        let mut found_pipe = false;
        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::struct_pipe_command => found_struct_pipe = true,
                Rule::pipe_command => found_pipe = true,
                _ => {}
            }
        }
        assert!(found_struct_pipe, "Expected struct_pipe_command");
        assert!(found_pipe, "Expected pipe_command");
    }
}

#[test]
fn parse_struct_pipe_dsl_simple() {
    init();
    let pairs = ShellParser::parse(Rule::command, "ps aux |: where cpu > 5")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let mut found_dsl = false;
        for inner_pair in pair.into_inner() {
            if let Rule::struct_pipe_command = inner_pair.as_rule() {
                for dsl_pair in inner_pair.into_inner() {
                    if let Rule::struct_pipe_dsl = dsl_pair.as_rule() {
                        found_dsl = true;
                        assert_eq!("where cpu > 5", dsl_pair.as_str());
                    }
                }
            }
        }
        assert!(found_dsl, "Expected struct_pipe_dsl");
    }
}

#[test]
fn parse_struct_pipe_dsl_uses_bare_pipe_as_stage_separator() {
    init();
    // The `|` inside the DSL is not a shell `pipe_command`: it's swallowed
    // whole by `struct_pipe_dsl`, which only stops at `;`/`&&`/`||`/`|>`/`|:`.
    let pairs = ShellParser::parse(
        Rule::command,
        "ps aux |: where cpu > 5 | select pid command | head 3",
    )
    .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count, "expected simple_command + struct_pipe_command");
        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::pipe_command => panic!("bare '|' inside the DSL must not become a pipe"),
                Rule::struct_pipe_command => {
                    let text = inner_pair.as_str();
                    assert!(text.contains("select pid command"));
                    assert!(text.contains("head 3"));
                }
                _ => {}
            }
        }
    }
}

#[test]
fn parse_struct_pipe_dsl_stops_at_statement_terminators() {
    init();
    for input in [
        "ps aux |: where cpu > 5 ; echo done",
        "ps aux |: where cpu > 5 && echo done",
        "ps aux |: where cpu > 5 || echo done",
        // A single `&` ends the DSL too: `& next` is the next AND-OR list,
        // not DSL text.
        "ps aux |: where cpu > 5 & echo done",
    ] {
        let pairs =
            ShellParser::parse(Rule::commands, input).unwrap_or_else(|e| panic!("{input}: {e}"));
        let mut command_count = 0;
        for pair in pairs {
            assert_eq!(Rule::commands, pair.as_rule());
            for inner_pair in pair.into_inner() {
                for command_pair in child_commands(inner_pair) {
                    if let Rule::command = command_pair.as_rule() {
                        command_count += 1;
                        if command_count == 1 {
                            assert!(!command_pair.as_str().contains("echo"));
                        }
                    }
                }
            }
        }
        assert_eq!(2, command_count, "{input}: expected two commands");
    }
}

#[test]
fn parse_struct_pipe_dsl_chain() {
    init();
    let pairs = ShellParser::parse(Rule::command, "ps aux |: where cpu > 5 |: count")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        let count = pair.clone().into_inner().count();
        assert_eq!(3, count); // simple_command + 2 struct_pipe_commands
    }
}

#[test]
fn parse_struct_pipe_malformed_lisp_falls_through_to_dsl() {
    init();
    // Unbalanced parens: `lisp_expr` fails, so this becomes `struct_pipe_dsl`
    // text (and `struct_pipe::desugar` rejects it at eval time with a clear
    // "starts with '(' but never balances" error instead of the previous
    // silent "ignored unparsed input" warning).
    let pairs = ShellParser::parse(Rule::command, "echo hi |: (table-head $_ 3")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        for inner_pair in pair.into_inner() {
            if let Rule::struct_pipe_command = inner_pair.as_rule() {
                for dsl_pair in inner_pair.into_inner() {
                    if let Rule::struct_pipe_dsl = dsl_pair.as_rule() {
                        assert!(dsl_pair.as_str().starts_with('('));
                    }
                }
            }
        }
    }
}

#[test]
fn test_expand_braces() {
    use super::expansion::expand_braces;

    // Basic
    assert_eq!(expand_braces("a{b,c}d"), vec!["abd", "acd"]);

    // Nested braces
    assert_eq!(expand_braces("a{b,c{d,e}}f"), vec!["abf", "acdf", "acef"]);

    // No braces
    assert_eq!(expand_braces("abcd"), vec!["abcd"]);

    // Escaped braces
    assert_eq!(expand_braces("a\\{b,c\\}d"), vec!["a\\{b,c\\}d"]);

    // Unmatched opening brace
    assert_eq!(expand_braces("a{b,cd"), vec!["a{b,cd"]);

    // Empty comma (implicit empty string)
    assert_eq!(expand_braces("a{,c}d"), vec!["ad", "acd"]);
    assert_eq!(expand_braces("a{b,}d"), vec!["abd", "ad"]);

    // Multiple top level braces
    assert_eq!(
        expand_braces("a{b,c}d{e,f}g"),
        vec!["abdeg", "abdfg", "acdeg", "acdfg"]
    );

    // Complex nested and consecutive braces
    assert_eq!(
        expand_braces("{a,b{c,d}}{e,f}"),
        vec!["ae", "af", "bce", "bcf", "bde", "bdf"]
    );
}
