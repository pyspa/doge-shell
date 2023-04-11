use crate::environment::Environment;
use anyhow::{anyhow, Result};
use pest::iterators::Pair;
use pest::Parser;
use pest::Span;
use pest_derive::Parser;
use std::collections::HashMap;
use std::{cell::RefCell, rc::Rc};
use tracing::debug;

#[derive(Parser)]
#[grammar = "shell.pest"]
pub struct ShellParser;

/// helpers
pub fn get_string(pair: Pair<Rule>) -> Option<String> {
    match pair.as_rule() {
        Rule::s_quoted => pair
            .into_inner()
            .next()
            .map(|next| next.as_str().to_string()),
        Rule::d_quoted => pair
            .into_inner()
            .next()
            .map(|next| next.as_str().to_string()),
        Rule::span => get_string(pair.into_inner().next().unwrap()), // TODO fix
        _ => Some(pair.as_str().to_string()),
    }
}

pub fn get_pos_word(input: &str, pos: usize) -> Result<Option<(Rule, Span)>> {
    let pairs = ShellParser::parse(Rule::command, input).map_err(|e| anyhow!(e))?;

    for pair in pairs {
        match pair.as_rule() {
            Rule::command => {
                for pair in pair.into_inner() {
                    let res = search_pos_word(pair, pos);
                    if res.is_some() {
                        return Ok(res);
                    }
                }
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

fn search_pos_word(pair: Pair<Rule>, pos: usize) -> Option<(Rule, Span)> {
    match pair.as_rule() {
        Rule::commands
        | Rule::command
        | Rule::simple_command
        | Rule::simple_command_bg
        | Rule::span
        | Rule::subshell => {
            for pair in pair.into_inner() {
                let res = search_pos_word(pair, pos);
                if res.is_some() {
                    return res;
                }
            }
        }
        Rule::argv0 => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    match pair.as_rule() {
                        Rule::subshell => {
                            let res = search_pos_word(pair, pos);
                            if res.is_some() {
                                return res;
                            }
                        }
                        _ => {
                            if let Some(res) = search_inner_word(pair, pos) {
                                return Some((Rule::argv0, res));
                            }
                        }
                    }
                }
            }
        }
        Rule::args => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    match pair.as_rule() {
                        Rule::subshell => {
                            let res = search_pos_word(pair, pos);
                            if res.is_some() {
                                return res;
                            }
                        }
                        _ => {
                            if let Some(res) = search_inner_word(pair, pos) {
                                return Some((Rule::argv0, res));
                            }
                        }
                    }
                }
            }
        }
        _ => {
            // TODO check search_pos_word
            println!("search_pos_word {:?} {:?}", pair.as_rule(), pair.as_str());
        }
    }
    None
}

fn search_inner_word(pair: Pair<Rule>, pos: usize) -> Option<Span> {
    match pair.as_rule() {
        Rule::s_quoted | Rule::d_quoted => {
            for pair in pair.into_inner() {
                let pair_span = pair.as_span();
                if pair_span.start() < pos && pos <= pair_span.end() {
                    return Some(pair_span);
                }
            }
        }
        Rule::word | Rule::variable => {
            let pair_span = pair.as_span();
            if pair_span.start() < pos && pos <= pair_span.end() {
                return Some(pair_span);
            }
        }
        _ => {}
    }
    None
}

fn expand_alias_tilde(pair: Pair<Rule>, alias: &HashMap<String, String>) -> Vec<String> {
    let mut argv: Vec<String> = vec![];

    match pair.as_rule() {
        Rule::word
        | Rule::variable
        | Rule::s_quoted
        | Rule::d_quoted
        | Rule::literal_s_quoted
        | Rule::literal_d_quoted
        | Rule::stdout_redirect_direction
        | Rule::stderr_redirect_direction => {
            argv.push(shellexpand::tilde(pair.as_str()).to_string());
        }
        Rule::argv0 => {
            for inner_pair in pair.into_inner() {
                let v = expand_alias_tilde(inner_pair, alias);
                for (i, arg) in v.iter().enumerate() {
                    if i == 0 {
                        if let Some(val) = alias.get(arg) {
                            debug!("replaced alias {arg} to {val}");
                            argv.push(val.trim().to_string());
                        } else {
                            argv.push(arg.trim().to_string());
                        }
                    } else {
                        argv.push(arg.trim().to_string());
                    }
                }
            }
        }
        _ => {
            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::simple_command_bg => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias);
                            argv.append(&mut v);
                        }
                        argv.push("&".to_string());
                    }
                    Rule::subshell => {
                        argv.push("(".to_string());
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias);
                            argv.append(&mut v);
                        }
                        argv.push(")".to_string());
                    }
                    Rule::argv0 => {
                        for inner_pair in inner_pair.into_inner() {
                            let v = expand_alias_tilde(inner_pair, alias);
                            for (i, arg) in v.iter().enumerate() {
                                if i == 0 {
                                    if let Some(val) = alias.get(arg) {
                                        debug!("replaced alias {arg} to {val}");
                                        argv.push(val.trim().to_string());
                                    } else {
                                        argv.push(arg.trim().to_string());
                                    }
                                } else {
                                    argv.push(arg.trim().to_string());
                                }
                            }
                        }
                    }
                    Rule::commands
                    | Rule::command
                    | Rule::simple_command
                    | Rule::args
                    | Rule::redirect
                    | Rule::span => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = expand_alias_tilde(inner_pair, alias);
                            argv.append(&mut v);
                        }
                    }
                    Rule::word
                    | Rule::variable
                    | Rule::s_quoted
                    | Rule::d_quoted
                    | Rule::literal_s_quoted
                    | Rule::literal_d_quoted
                    | Rule::stdout_redirect_direction
                    | Rule::stderr_redirect_direction => {
                        let mut v = expand_alias_tilde(inner_pair, alias);
                        argv.append(&mut v);
                    }
                    _ => {
                        debug!(
                            "expand_alias_tilde missing {:?} {:?}",
                            inner_pair.as_rule(),
                            inner_pair.as_str()
                        );
                    }
                }
            }
        }
    }
    argv
}

pub fn expand_alias(input: String, environment: Rc<RefCell<Environment>>) -> Result<String> {
    let mut buf: Vec<String> = Vec::new();
    let pairs = ShellParser::parse(Rule::commands, &input).map_err(|e| anyhow!(e))?;

    for pair in pairs {
        for pair in pair.into_inner() {
            let mut commands = expand_command_alias(pair, Rc::clone(&environment))?;
            buf.append(&mut commands);
        }
    }
    Ok(buf.join(" "))
}

fn expand_command_alias(
    pair: Pair<Rule>,
    environment: Rc<RefCell<Environment>>,
) -> Result<Vec<String>> {
    let mut buf: Vec<String> = Vec::new();

    if let Rule::command = pair.as_rule() {
        for inner_pair in pair.into_inner() {
            match inner_pair.as_rule() {
                Rule::simple_command => {
                    let args = expand_alias_tilde(inner_pair, &environment.borrow().alias);
                    for arg in args {
                        if let Some(val) = environment.borrow().variables.get(&arg) {
                            buf.push(val.trim().to_string());
                        } else {
                            buf.push(arg);
                        }
                    }
                }
                Rule::simple_command_bg => {
                    let args = expand_alias_tilde(inner_pair, &environment.borrow().alias);
                    for arg in args {
                        if let Some(val) = environment.borrow().variables.get(&arg) {
                            buf.push(val.trim().to_string());
                        } else {
                            buf.push(arg);
                        }
                    }
                    buf.push("&".to_string());
                }
                Rule::pipe_command => {
                    buf.push("|".to_string());
                    let args = expand_alias_tilde(inner_pair, &environment.borrow().alias);
                    for arg in args {
                        if let Some(val) = environment.borrow().variables.get(&arg) {
                            buf.push(val.trim().to_string());
                        } else {
                            buf.push(arg);
                        }
                    }
                }
                _ => {
                    debug!(
                        "expand_command_alias missing {:?} {:?}",
                        inner_pair.as_rule(),
                        inner_pair.as_str()
                    );
                }
            }
        }
    } else if let Rule::command_list_sep = pair.as_rule() {
        buf.push(pair.as_str().to_string());
    }

    Ok(buf)
}

pub fn get_words(input: &str, pos: usize) -> Result<Vec<(Rule, Span, bool)>> {
    let pairs = ShellParser::parse(Rule::command, input).map_err(|e| anyhow!(e))?;
    let mut result: Vec<(Rule, Span, bool)> = Vec::new();
    for pair in pairs {
        match pair.as_rule() {
            Rule::command => {
                for pair in pair.into_inner() {
                    let mut res = to_words(pair, pos);
                    result.append(&mut res);
                }
            }
            _ => return Ok(result),
        }
    }
    Ok(result)
}

fn to_words(pair: Pair<Rule>, pos: usize) -> Vec<(Rule, Span, bool)> {
    let mut result: Vec<(Rule, Span, bool)> = vec![];
    match pair.as_rule() {
        Rule::argv0 => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    // TODO check subshell
                    if let Some((span, current)) = get_span(pair, pos) {
                        result.push((Rule::argv0, span, current));
                    };
                }
            }
        }
        Rule::args => {
            for pair in pair.into_inner() {
                for pair in pair.into_inner() {
                    if let Some((span, current)) = get_span(pair, pos) {
                        result.push((Rule::args, span, current));
                    };
                }
            }
        }

        _ => {
            for inner_pair in pair.into_inner() {
                // debug!(
                //     "inner_pair {:?} {:?}",
                //     inner_pair.as_rule(),
                //     inner_pair.as_str()
                // );

                match inner_pair.as_rule() {
                    Rule::simple_command | Rule::simple_command_bg => {
                        for inner_pair in inner_pair.into_inner() {
                            let mut v = to_words(inner_pair, pos);
                            result.append(&mut v);
                        }
                    }
                    Rule::argv0 => {
                        for pair in inner_pair.into_inner() {
                            for pair in pair.into_inner() {
                                // TODO check subshell
                                if let Some((span, current)) = get_span(pair, pos) {
                                    result.push((Rule::argv0, span, current));
                                };
                            }
                        }
                    }
                    Rule::args => {
                        for pair in inner_pair.into_inner() {
                            for pair in pair.into_inner() {
                                if let Some((span, current)) = get_span(pair, pos) {
                                    result.push((Rule::args, span, current));
                                };
                            }
                        }
                    }

                    _ => {
                        debug!(
                            "to_words missing {:?} {:?}",
                            inner_pair.as_rule(),
                            inner_pair.as_str()
                        );
                    }
                }
            }
        }
    }
    result
}

fn get_span(pair: Pair<Rule>, pos: usize) -> Option<(Span, bool)> {
    match pair.as_rule() {
        Rule::span => {
            for pair in pair.into_inner() {
                let span = get_span(pair, pos);
                if span.is_some() {
                    return span;
                }
            }
        }
        Rule::s_quoted | Rule::d_quoted => {
            for pair in pair.into_inner() {
                let span = get_span(pair, pos);
                if span.is_some() {
                    return span;
                }
            }
        }
        Rule::word
        | Rule::variable
        | Rule::literal_s_quoted
        | Rule::literal_d_quoted
        | Rule::stdout_redirect_direction
        | Rule::stderr_redirect_direction => {
            let pair_span = pair.as_span();
            if pair_span.start() < pos && pos <= pair_span.end() {
                return Some((pair_span, true));
            } else {
                return Some((pair_span, false));
            }
        }
        _ => {
            debug!("get_span missing {:?} {:?}", pair.as_rule(), pair.as_str());
        }
    }
    None
}

#[cfg(test)]
mod test {
    use super::*;
    use pest::Parser;
    use std::cell::RefCell;
    use std::rc::Rc;
    use tracing::debug;

    fn init() {
        tracing_subscriber::fmt::init();
    }

    type JobLink = Rc<RefCell<Job>>;

    #[derive(Debug)]
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
        let pairs = ShellParser::parse(Rule::word, "a1bc").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::word, pair.as_rule());
        }
    }

    #[test]
    fn parse_quoted() {
        let pairs =
            ShellParser::parse(Rule::quoted, "\'a1bc\'").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::s_quoted, pair.as_rule());
            assert_eq!("a1bc", get_string(pair).unwrap());
        }
        let pairs =
            ShellParser::parse(Rule::quoted, "\"a1bc\"").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::d_quoted, pair.as_rule());
            assert_eq!("a1bc", get_string(pair).unwrap());
        }
    }

    #[test]
    fn parse_argv0() {
        let pairs = ShellParser::parse(Rule::argv0, "a1bc").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::argv0, pair.as_rule());
        }
    }

    #[test]
    fn parse_args1() {
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
        let pairs = ShellParser::parse(Rule::simple_command, "  test   ")
            .unwrap_or_else(|e| panic!("{}", e));
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
    fn parse_simple_command3() {
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
    fn parse_command1() {
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
                        assert_eq!("sk --ansi --inline-info", cmd);
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command2() {
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
                            assert_eq!("test  --a1bc --b2=c3", cmd);
                        } else if i == 2 {
                            assert_eq!("dd", cmd);
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command3() {
        let pairs =
            ShellParser::parse(Rule::command, "history").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());

            let count = pair.clone().into_inner().count();
            assert_eq!(1, count);

            for (i, inner_pair) in pair.into_inner().enumerate() {
                match inner_pair.as_rule() {
                    Rule::simple_command => {
                        let cmd = inner_pair.as_str();
                        if i == 0 {
                            assert_eq!("history", cmd);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command4() {
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
                            assert_eq!("sk", cmd);
                        } else if i == 2 {
                            assert_eq!("bash -s", cmd);
                        }
                    }

                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command_sp() {
        let pairs = ShellParser::parse(Rule::command, "   ").unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(0, count);
        }
    }

    #[test]
    fn parse_simple_command_bg1() {
        let pairs = ShellParser::parse(Rule::simple_command_bg, "sleep 20 &")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::simple_command_bg, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(1, count);

            for inner_pair in pair.into_inner() {
                match inner_pair.as_rule() {
                    Rule::simple_command => {
                        let cmd = inner_pair.as_str();
                        assert_eq!("sleep 20", cmd);
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn parse_command_bg() {
        let pairs = ShellParser::parse(Rule::command, "sleep 20 & sleep 30 &")
            .unwrap_or_else(|e| panic!("{}", e));
        for pair in pairs {
            assert_eq!(Rule::command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for (i, inner_pair) in pair.into_inner().enumerate() {
                match inner_pair.as_rule() {
                    Rule::simple_command_bg => {
                        let inner_pair = inner_pair.into_inner();
                        let cmd = inner_pair.as_str();
                        if i == 0 {
                            assert_eq!("sleep 20", cmd);
                        } else if i == 1 {
                            assert_eq!("sleep 30", cmd);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn test_get_pos_word() -> Result<()> {
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
    fn test_expand_alias() -> Result<()> {
        let env = crate::environment::Environment::new();

        env.borrow_mut()
            .alias
            .insert("alias".to_string(), "echo 'test' | sk ".to_string());
        env.borrow_mut()
            .variables
            .insert("$FOO".to_string(), "BAR".to_string());

        let input = r#"alias abc " test" '-vvv' --foo "#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk abc " test" '-vvv' --foo"#.to_string()
        );

        let input = r#"alias abc " test" '-vvv' --foo &"#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk abc " test" '-vvv' --foo &"#.to_string()
        );

        let input = r#"alias | abc " test" '-vvv' --foo &"#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo 'test' | sk | abc " test" '-vvv' --foo &"#.to_string()
        );

        let input = r#"sh -c | alias " test" '-vvv' --foo &"#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"sh -c | echo 'test' | sk " test" '-vvv' --foo &"#.to_string()
        );

        let input = r#"echo (alias " test" '-vvv' --foo) "#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(
            replaced,
            r#"echo ( echo 'test' | sk " test" '-vvv' --foo )"#.to_string()
        );
        let input = r#"echo $FOO"#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(replaced, r#"echo BAR"#.to_string());

        let input = r#"echo 'test' > test.log"#.to_string();
        let replaced = expand_alias(input, Rc::clone(&env))?;
        assert_eq!(replaced, r#"echo 'test' > test.log"#.to_string());

        Ok(())
    }

    #[test]
    fn parse_commands() {
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
                                                    match pair.as_rule() {
                                                        Rule::span => {
                                                            for pair in pair.into_inner() {
                                                                match pair.as_rule() {
                                                                    Rule::subshell => {
                                                                        assert_eq!(pair.as_str(), "(sudo docker ps -a -q)")
                                                                    }
                                                                    _ => {}
                                                                }
                                                            }
                                                        }

                                                        _ => {}
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
        let mut find = false;
        let pairs = ShellParser::parse(Rule::simple_command, r#"sleep $foo "#)
            .unwrap_or_else(|e| panic!("{}", e));

        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::args => {
                        for pair in pair.into_inner() {
                            for pair in pair.into_inner() {
                                assert_eq!(Rule::variable, pair.as_rule());
                                assert_eq!("$foo", pair.as_str());

                                find = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        assert!(find);
    }

    #[test]
    fn test_redirect() {
        let pairs = ShellParser::parse(Rule::simple_command, r#"echo "f" > test.log "#)
            .unwrap_or_else(|e| panic!("{}", e));
        let mut found = false;
        for pair in pairs {
            assert_eq!(Rule::simple_command, pair.as_rule());
            // println!("* {:?} {:?}", pair.as_rule(), pair.as_str());
            let count = pair.clone().into_inner().count();
            assert_eq!(2, count);
            for pair in pair.into_inner() {
                match pair.as_rule() {
                    Rule::args => {
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
                    _ => {}
                }
            }
        }
        assert!(found);
    }

    #[test]
    fn test_redirect2() {
        let pairs = ShellParser::parse(Rule::command, r#"ls -al | wc -l > test.log "#)
            .unwrap_or_else(|e| panic!("{}", e));
        let mut found = false;
        for pair in pairs {
            // println!("* {:?} {:?}", pair.as_rule(), pair.as_str());
            for pair in pair.into_inner() {
                // println!("** {:?} {:?}", pair.as_rule(), pair.as_str());
                for pair in pair.into_inner() {
                    // println!("*** {:?} {:?}", pair.as_rule(), pair.as_str());
                    match pair.as_rule() {
                        Rule::simple_command => {
                            for pair in pair.into_inner() {
                                match pair.as_rule() {
                                    Rule::args => {
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
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        assert!(found);
    }
}
