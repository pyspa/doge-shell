use super::ast::{get_pos_word, get_string};
use super::expansion::{expand_alias, expand_alias_tilde};
use super::{Rule, ShellParser};
use crate::environment::Environment;
use anyhow::Result;
use pest::Parser;
use std::cell::RefCell;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use tracing::debug;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
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
    let pairs =
        ShellParser::parse(Rule::args, r#"echo "test""#).unwrap_or_else(|e| panic!("{}", e));
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
    let mut pairs = ShellParser::parse(Rule::simple_command, "cat < input.txt")
        .unwrap_or_else(|e| panic!("{}", e));
    let alias_simple = pairs.next().unwrap();
    let tokens = expand_alias_tilde(alias_simple, &env.read().alias, &PathBuf::from("."))
        .expect("tokenize redirect");
    assert_eq!(tokens, vec!["cat", "<", "input.txt"]);

    let result =
        expand_alias("cat < input.txt".to_string(), env).expect("alias expansion succeeds");
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
    let pairs = ShellParser::parse(Rule::command, "   ").unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(0, count);
    }
}

#[test]
fn parse_simple_command_bg1() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command_bg, "sleep 20 &")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::simple_command_bg, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);

        for inner_pair in pair.into_inner() {
            if inner_pair.as_rule() == Rule::simple_command {
                let cmd = inner_pair.as_str();
                assert_eq!("sleep 20", cmd);
            }
        }
    }
}

#[test]
fn parse_command_bg() {
    init();
    let pairs = ShellParser::parse(Rule::command, "sleep 20 & sleep 30 &")
        .unwrap_or_else(|e| panic!("{}", e));
    for pair in pairs {
        assert_eq!(Rule::command, pair.as_rule());
        let count = pair.clone().into_inner().count();
        assert_eq!(2, count);
        for (i, inner_pair) in pair.into_inner().enumerate() {
            if inner_pair.as_rule() == Rule::simple_command_bg {
                let inner_pair = inner_pair.into_inner();
                let cmd = inner_pair.as_str();
                if i == 0 {
                    assert_eq!("sleep 20 &", cmd);
                } else if i == 1 {
                    assert_eq!("sleep 30 &", cmd);
                }
            }
        }
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

#[test]
fn test_expand_alias() -> Result<()> {
    init();
    let env = crate::environment::Environment::new();

    env.write()
        .alias
        .insert("alias".to_string(), "echo 'test' | sk ".to_string());
    env.write()
        .variables
        .insert("$FOO".to_string(), "BAR".to_string());

    let input = r#"alias abc " test" '-vvv' --foo "#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(
        replaced,
        r#"echo 'test' | sk abc " test" '-vvv' --foo"#.to_string()
    );

    let input = r#"alias abc " test" '-vvv' --foo &"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(
        replaced,
        r#"echo 'test' | sk abc " test" '-vvv' --foo &"#.to_string()
    );

    let input = r#"alias | abc " test" '-vvv' --foo &"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(
        replaced,
        r#"echo 'test' | sk | abc " test" '-vvv' --foo"#.to_string()
    );

    let input = r#"sh -c | alias " test" '-vvv' --foo &"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(
        replaced,
        r#"sh -c | echo 'test' | sk " test" '-vvv' --foo"#.to_string()
    );

    let input = r#"echo (alias " test" '-vvv' --foo) "#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(
        replaced,
        r#"echo ( echo 'test' | sk " test" '-vvv' --foo )"#.to_string()
    );
    let input = r#"echo $FOO"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, r#"echo BAR"#.to_string());

    let input = r#"echo 'test' > test.log"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, r#"echo 'test' > test.log"#.to_string());

    Ok(())
}

#[test]
fn test_simple_alias_like_ll() -> Result<()> {
    init();
    let env = crate::environment::Environment::new();

    env.write()
        .alias
        .insert("ll".to_string(), "exa -al".to_string());
    env.write().alias.insert("g".to_string(), "git".to_string());

    // Test simple alias 'll'
    let input = r#"ll"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, r#"exa -al"#.to_string());

    // Test alias with arguments
    let input = r#"ll -h"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, r#"exa -al -h"#.to_string());

    // Test single letter alias
    let input = r#"g status"#.to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, r#"git status"#.to_string());

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
                Rule::command_list_sep => {}
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

    for pair in pairs {
        for pair in pair.into_inner() {
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
                                                            )
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

#[test]
fn parse_subshell2() {
    init();
    let sub = "(ls -al | wc -l)";
    let cmd = format!("echo {}", &sub);
    let pairs = ShellParser::parse(Rule::commands, &cmd).unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        for pair in pair.into_inner() {
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
                                                                    for pair in pair.into_inner() {
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

#[test]
fn parse_proc_subst() {
    init();
    let pairs =
        ShellParser::parse(Rule::commands, "echo <(ls)").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        for pair in pair.into_inner() {
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
                                                            assert_eq!(pair.as_str(), "<(ls)")
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
    let env = crate::environment::Environment::new();

    let input = "echo {a,b,c}".to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, "echo a b c".to_string());

    let input = "echo pre{X,Y}post".to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, "echo preXpost preYpost".to_string());

    let input = "echo a{b,c{d,e}}".to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, "echo ab acd ace".to_string());

    let input = "echo {a,b}{1,2}".to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    assert_eq!(replaced, "echo a1 a2 b1 b2".to_string());

    // Check globbing interaction
    // Since files don't exist, glob pattern remains literal
    let input = "echo {*.test_dummy_1,*.test_dummy_2}".to_string();
    let replaced = expand_alias(input, Arc::clone(&env))?;
    // result should be "*.test_dummy_1 *.test_dummy_2" because glob expansion fails and returns pattern
    // The order depends on implementation but implementation preserves order of brace expansion
    assert_eq!(replaced, "echo *.test_dummy_1 *.test_dummy_2".to_string());

    Ok(())
}

#[test]
fn test_glob_expansion() -> Result<()> {
    init();
    use std::fs::File;
    let dir = tempfile::tempdir()?;
    let path_a = dir.path().join("glob_test_a.txt");
    File::create(&path_a)?;
    let path_b = dir.path().join("glob_test_b.txt");
    File::create(&path_b)?;

    let env = crate::environment::Environment::new();
    let alias = &env.read().alias;

    // Test *.txt expansion
    let pairs = ShellParser::parse(Rule::glob_word, "*.txt").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let expanded = expand_alias_tilde(pair, alias, &dir.path().to_path_buf())?;
        // Should contain glob_test_a.txt and glob_test_b.txt (quoted or not?)
        // expand_alias_tilde returns fully qualified paths if using absolute root?
        // Or relative?
        // find_glob_root handles it.
        // Since we pass an absolute path as current_dir, and pattern is relative "*.txt".

        let s = expanded.join(" ");
        assert!(s.contains("glob_test_a.txt"));
        assert!(s.contains("glob_test_b.txt"));
        assert_eq!(expanded.len(), 2);
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

    let env = crate::environment::Environment::new();
    let alias = &env.read().alias;

    // Pattern matches nothing
    let pairs = ShellParser::parse(Rule::glob_word, "*.rs").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let expanded = expand_alias_tilde(pair, alias, &dir.path().to_path_buf())?;
        // Should return literal if no match
        assert_eq!(expanded.len(), 1);
        assert_eq!(expanded[0], "*.rs");
    }
    Ok(())
}

#[test]
fn test_glob_question_mark() -> Result<()> {
    init();
    use std::fs::File;
    let dir = tempfile::tempdir()?;
    File::create(dir.path().join("file1.txt"))?;
    File::create(dir.path().join("fileA.txt"))?;

    let env = crate::environment::Environment::new();
    let alias = &env.read().alias;

    let pairs =
        ShellParser::parse(Rule::glob_word, "file?.txt").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let expanded = expand_alias_tilde(pair, alias, &dir.path().to_path_buf())?;
        assert_eq!(expanded.len(), 2);
        let s = expanded.join(" ");
        assert!(s.contains("file1.txt"));
        assert!(s.contains("fileA.txt"));
    }
    Ok(())
}

#[test]
fn test_glob_character_class() -> Result<()> {
    init();
    use std::fs::File;
    let dir = tempfile::tempdir()?;
    File::create(dir.path().join("file1.txt"))?;
    File::create(dir.path().join("file2.txt"))?;
    File::create(dir.path().join("fileA.txt"))?;

    let env = crate::environment::Environment::new();
    let alias = &env.read().alias;

    let pairs =
        ShellParser::parse(Rule::glob_word, "file[0-9].txt").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let expanded = expand_alias_tilde(pair, alias, &dir.path().to_path_buf())?;
        assert_eq!(expanded.len(), 2);
        let s = expanded.join(" ");
        assert!(s.contains("file1.txt"));
        assert!(s.contains("file2.txt"));
        assert!(!s.contains("fileA.txt"));
    }
    Ok(())
}

#[test]
fn test_glob_subdirectory() -> Result<()> {
    init();
    use std::fs::{self, File};
    let dir = tempfile::tempdir()?;
    let subdir = dir.path().join("sub");
    fs::create_dir(&subdir)?;
    File::create(subdir.join("test.rs"))?;

    let env = crate::environment::Environment::new();
    let alias = &env.read().alias;

    let pairs = ShellParser::parse(Rule::glob_word, "sub/*.rs").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let expanded = expand_alias_tilde(pair, alias, &dir.path().to_path_buf())?;
        assert_eq!(expanded.len(), 1);
        let s = expanded[0].clone();
        assert!(s.contains("sub"));
        assert!(s.contains("test.rs"));
    }
    Ok(())
}

#[test]
fn test_recursive_glob() -> Result<()> {
    init();
    use std::fs::{self, File};
    let dir = tempfile::tempdir()?;
    let subdir = dir.path().join("sub");
    fs::create_dir(&subdir)?;
    let nested = subdir.join("nested");
    fs::create_dir(&nested)?;

    File::create(dir.path().join("root.rs"))?;
    File::create(subdir.join("sub.rs"))?;
    File::create(nested.join("deep.rs"))?;

    let env = crate::environment::Environment::new();
    let alias = &env.read().alias;

    // Test **/*.rs
    let pairs = ShellParser::parse(Rule::glob_word, "**/*.rs").unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let expanded = expand_alias_tilde(pair, alias, &dir.path().to_path_buf())?;
        // Should find all 3 .rs files
        assert_eq!(expanded.len(), 3);
        let s = expanded.join(" ");
        assert!(s.contains("root.rs"));
        assert!(s.contains("sub.rs"));
        assert!(s.contains("deep.rs"));
    }
    Ok(())
}
