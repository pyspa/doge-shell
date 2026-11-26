//! Pacman operations whose positional targets depend on a modifier.
use super::*;

pub(crate) fn pacman_short_path(token: &str) -> Option<(&'static str, &str)> {
    let flags = token.strip_prefix('-')?;
    for (i, c) in flags.char_indices() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        if matches!(c, 'b' | 'r') {
            return Some((if c == 'b' { "-b" } else { "-r" }, &flags[i + 1..]));
        }
    }
    None
}

impl ContextCorrector<'_> {
    pub(super) fn pacman_bundled_operation<'a>(
        subcommands: &'a [SubCommand],
        token: &str,
    ) -> Option<&'a SubCommand> {
        let flags = token.strip_prefix('-')?;
        if flags.starts_with('-') {
            return None;
        }
        // -b and -r consume the remainder as a path, even if it contains an
        // uppercase operation letter.
        flags
            .char_indices()
            .take_while(|(_, c)| !matches!(c, 'b' | 'r'))
            .find_map(|(i, c)| {
                if !c.is_ascii_uppercase() {
                    return None;
                }
                subcommands
                    .iter()
                    .find(|sub| sub.name.strip_prefix('-') == flags.get(i..i + 1))
            })
    }

    pub(super) fn pacman_target_type(
        parsed: &ParsedCommandLine,
        options: &[&CommandOption],
        definition: &CommandCompletion,
    ) -> Option<ArgumentType> {
        let op = parsed.subcommand_path.first()?.as_str();
        let end = Self::current_raw_index(parsed)?.min(parsed.raw_args.len());
        let mut i = Self::next_argv_index(&parsed.raw_args, 0);
        while i < end {
            let token = super::super::shell_path::decode_literal_word(&parsed.raw_args[i]);
            i = Self::next_argv_index(&parsed.raw_args, i + 1);
            if token == "--" {
                break;
            }
            if OptionWord::parse(&token, options).is_some_and(|word| word.takes_separate_value()) {
                i = Self::next_argv_index(&parsed.raw_args, i + 1);
                continue;
            }
            let has = |short: char, long: &str| {
                token == long
                    || token.strip_prefix('-').is_some_and(|flags| {
                        !flags.starts_with('-')
                            && flags
                                .chars()
                                .take_while(|c| !matches!(c, 'b' | 'r'))
                                .any(|c| c == short)
                    })
            };
            let provider = match op {
                "-Q" if has('o', "--owns") => return Some(ArgumentType::File { extensions: None }),
                "-Q" if has('p', "--file") => {
                    return definition
                        .subcommands
                        .iter()
                        .find(|sub| sub.name == "-U")?
                        .arguments
                        .first()?
                        .arg_type
                        .clone();
                }
                "-S" if has('l', "--list") => "pacman.repository",
                "-F" if has('l', "--list") => "pacman.sync_package",
                "-Q" if has('g', "--groups") => "pacman.installed_group",
                "-S" if has('g', "--groups") => "pacman.sync_group",
                "-S" | "-Q" if has('s', "--search") => return Some(ArgumentType::String),
                _ => continue,
            };
            return Some(ArgumentType::Dynamic {
                provider: provider.into(),
                scope: None,
            });
        }
        None
    }
}
