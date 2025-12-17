use super::{Rule, ShellParser};
use pest::Parser;

fn init() {
    let _ = tracing_subscriber::fmt::try_init();
}

use super::ast::get_string;

#[test]
fn test_escape_space() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, r#"echo hello\ world"#)
        .unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        assert_eq!(Rule::simple_command, pair.as_rule());
        let mut inner = pair.into_inner();
        let _cmd = inner.next().unwrap();
        let args_pair = inner.next().unwrap();
        assert_eq!(Rule::args, args_pair.as_rule());

        let mut args = args_pair.into_inner();
        let arg1 = args.next().unwrap(); // span or word

        // Check structure: Parsing as single argument
        assert_eq!(1, args.count() + 1); // +1 because we consumed arg1

        // Check AST value: Unescaping
        // arg1 is Rule::span -> inner is Rule::word
        let val = get_string(arg1).unwrap();
        assert_eq!("hello world", val);
    }
}

#[test]
fn test_escape_special() {
    init();
    let pairs = ShellParser::parse(Rule::simple_command, r#"echo \| \& \> \<"#)
        .unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let mut inner = pair.into_inner();
        let _cmd = inner.next().unwrap();
        let args_pair = inner.next().unwrap();
        let mut args = args_pair.into_inner();

        // |
        let arg = args.next().unwrap();
        assert_eq!("|", get_string(arg).unwrap());
        // &
        let arg = args.next().unwrap();
        assert_eq!("&", get_string(arg).unwrap());
        // >
        let arg = args.next().unwrap();
        assert_eq!(">", get_string(arg).unwrap());
        // <
        let arg = args.next().unwrap();
        assert_eq!("<", get_string(arg).unwrap());
    }
}

#[test]
fn test_escape_backslash() {
    init();
    let pairs =
        ShellParser::parse(Rule::simple_command, r#"echo \\"#).unwrap_or_else(|e| panic!("{}", e));

    for pair in pairs {
        let mut inner = pair.into_inner();
        let _cmd = inner.next().unwrap();
        let args_pair = inner.next().unwrap();
        let mut args = args_pair.into_inner();

        let arg = args.next().unwrap();
        assert_eq!("\\", get_string(arg).unwrap());
    }
}
