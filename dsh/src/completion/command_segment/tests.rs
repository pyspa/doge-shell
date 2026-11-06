use super::*;

#[test]
fn literal_operators_and_nested_bodies_are_not_boundaries() {
    for input in [
        r#"echo 'a && b'"#,
        r#"echo "a || b""#,
        r"echo a\&\&b",
        r"echo a\;b",
        r"echo a\|b",
        "echo ok\\\n tail",
        r"echo 'a\'&&b'",
        "echo ${X:-a&&b}",
        "echo $((1|2))",
        "echo $(a && b)",
        "echo <(a|b)",
        "echo (a;b)",
        "echo a\u{a0}b",
    ] {
        let site = command_segment_at_cursor(input, input.len());
        assert!(!site.multiple, "{input:?}");
        assert!(!site.suppressed, "{input:?}");
    }
}

#[test]
fn operators_and_comments_have_explicit_suppression() {
    for marked in [
        "a &§& b",
        "a |§| b",
        "a # b§",
        "a |: select p§",
        "a |>§",
        "a |& b§",
        "a &&& b§",
        "a ||| b§",
        "a & & b§",
    ] {
        let cursor = marked.find('§').unwrap();
        let input = marked.replace('§', "");
        assert!(
            command_segment_at_cursor(&input, cursor).suppressed,
            "{marked}"
        );
    }
}

#[test]
fn cursor_edges_select_left_right_and_empty_rhs() {
    for op in ["&&", "||", "|", ";", "\n", "\r\n", "&"] {
        let input = format!("left arg{op}right value");
        let left = command_segment_at_cursor(&input, 8);
        assert_eq!(&input[left.range], "left arg", "{op:?}");
        let right = command_segment_at_cursor(&input, 8 + op.len());
        assert_eq!(&input[right.range], "right value", "{op:?}");
        let empty = format!("left{op}  ");
        let site = command_segment_at_cursor(&empty, empty.len());
        assert!(!site.suppressed);
        assert_eq!(&empty[site.range], "  ");
    }
}

#[test]
fn redirects_and_continuation_do_not_create_false_commands() {
    for input in ["echo 2>&1", "echo <&0", "echo &>out", "echo &>>out"] {
        assert!(
            !command_segment_at_cursor(input, input.len()).multiple,
            "{input}"
        );
    }
    let input = "echo &&\n\tbg %";
    let site = command_segment_at_cursor(input, input.len());
    assert!(!site.suppressed);
    assert_eq!(input[site.range].trim(), "bg %");
}

#[test]
fn completed_simple_command_spans_match_execution_grammar() {
    for input in [
        "echo one&&git checkout ma",
        "echo one || git checkout ma",
        "echo one | git checkout ma",
        "echo one;git checkout ma",
        "echo one\ngit checkout ma",
        "echo one\r\ngit checkout ma",
        "echo one & git checkout ma",
        "echo 'a && b' && git checkout ma",
        "echo 2>&1 && git checkout ma",
        "echo ${X:-a&&b} && git checkout ma",
    ] {
        let pairs = ShellParser::parse(Rule::commands, input).unwrap();
        assert_eq!(pairs.clone().next().unwrap().as_span().end(), input.len());
        let expected: Vec<_> = pairs
            .flatten()
            .filter(|p| p.as_rule() == Rule::simple_command)
            .map(|p| p.as_str().trim().to_string())
            .collect();
        assert_eq!(expected.len(), 2);
        let left = command_segment_at_cursor(input, 4);
        let right = command_segment_at_cursor(input, input.len());
        assert!(!left.suppressed && !right.suppressed);
        assert_eq!(
            [input[left.range].trim(), input[right.range].trim()],
            [expected[0].as_str(), expected[1].as_str()],
            "{input:?}"
        );
    }
}
