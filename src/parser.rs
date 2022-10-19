use anyhow::{anyhow, Result};
use log::debug;
use pest::iterators::Pair;
use pest::Parser;
use pest::Span;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "shell.pest"]
pub struct ShellParser;

/// helpers
pub fn get_string(pair: Pair<Rule>) -> Option<String> {
    match pair.as_rule() {
        Rule::span => {
            if let Some(pair) = pair.into_inner().next() {
                get_string(pair)
            } else {
                None
            }
        }
        Rule::s_quoted => {
            if let Some(next) = pair.into_inner().next() {
                Some(next.as_str().to_string())
            } else {
                None
            }
        }
        Rule::d_quoted => {
            if let Some(next) = pair.into_inner().next() {
                Some(next.as_str().to_string())
            } else {
                None
            }
        }
        _ => Some(pair.as_str().to_string()),
    }
}

pub fn get_argv(pair: Pair<Rule>) -> Vec<String> {
    let mut argv: Vec<String> = vec![];
    for inner_pair in pair.into_inner() {
        match inner_pair.as_rule() {
            Rule::argv0 => {
                for inner_pair in inner_pair.into_inner() {
                    if let Some(arg) = get_string(inner_pair) {
                        argv.push(arg);
                    }
                }
            }
            Rule::args => {
                for inner_pair in inner_pair.into_inner() {
                    if let Some(arg) = get_string(inner_pair) {
                        argv.push(arg)
                    }
                }
            }
            _ => {}
        }
    }
    argv
}

pub fn get_pos_word(input: &str, pos: usize) -> Result<Option<(Rule, Span)>> {
    let pairs = ShellParser::parse(Rule::command, input).map_err(|e| anyhow!(e))?;

    for pair in pairs {
        match pair.as_rule() {
            Rule::command => {
                for pair in pair.into_inner() {
                    let res = search_pos_word(pair, input, pos);
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

fn search_pos_word<'a>(
    pair: Pair<'a, Rule>,
    input: &'a str,
    pos: usize,
) -> Option<(Rule, Span<'a>)> {
    match pair.as_rule() {
        Rule::simple_command | Rule::simple_command_bg => {
            for pair in pair.into_inner() {
                let res = search_pos_word(pair, input, pos);
                if res.is_some() {
                    return res;
                }
            }
        }
        Rule::argv0 => {
            for pair in pair.into_inner() {
                let res = search_inner_word(pair, pos);
                if res.is_some() {
                    return Some((Rule::argv0, res.unwrap()));
                }
            }
        }
        Rule::args => {
            for pair in pair.into_inner() {
                let res = search_inner_word(pair, pos);
                if res.is_some() {
                    return Some((Rule::args, res.unwrap()));
                }
            }
        }
        _ => {
            // TODO
            // println!("{:?} {:?}", pair.as_rule(), pair.as_str());
        }
    }
    None
}

fn search_inner_word(pair: Pair<Rule>, pos: usize) -> Option<Span> {
    match pair.as_rule() {
        Rule::span => {
            for pair in pair.into_inner() {
                let pair_span = pair.as_span();
                if pair_span.start() < pos && pos <= pair_span.end() {
                    return Some(pair_span);
                }
            }
        }
        _ => {}
    }
    None
}

#[cfg(test)]
mod test {
    use pest::Parser;

    use super::*;
    #[test]
    fn init() {
        let _ = env_logger::try_init();
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
                match inner_pair.as_rule() {
                    Rule::argv0 => {
                        let cmd = inner_pair.as_str();
                        assert_eq!("test", cmd);
                    }
                    _ => {}
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

            let argv = get_argv(pair);
            assert_eq!(5, argv.len());
            assert_eq!("echo", argv[0]);
            assert_eq!("abc", argv[1]);
            assert_eq!(" test", argv[2]);
            assert_eq!("-vvv", argv[3]);
            assert_eq!("--foo", argv[4]);
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
        let _ = env_logger::try_init();
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
    fn get_pos_word1() -> Result<()> {
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

        Ok(())
    }
}
