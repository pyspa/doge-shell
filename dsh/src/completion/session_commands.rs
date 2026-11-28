//! Names that the interactive shell resolves before searching PATH.

use super::Candidate;

/// Names resolved by this shell before searching PATH. They are captured
/// outside the filesystem cache because the session can redefine them.
pub(crate) fn session_command_names(
    shell: &crate::shell::Shell,
    allow_aliases: bool,
    matches: impl Fn(&str) -> bool,
) -> Vec<Candidate> {
    let mut names = Vec::new();
    if allow_aliases {
        let env = shell.environment.read();
        names.extend(
            env.variable_state
                .alias
                .keys()
                .filter(|name| matches(name))
                .map(|name| Candidate::Item(name.clone(), "(alias)".into())),
        );
    }
    names.extend(
        BUILTIN_NAMES
            .iter()
            .filter(|(name, _)| matches(name))
            .map(|(name, description)| Candidate::Item((*name).into(), (*description).into())),
    );
    let lisp = shell.lisp_engine.borrow();
    names.extend(
        lisp.env
            .borrow()
            .exported_command_names()
            .filter(|name| matches(name))
            .map(|name| Candidate::Item(name.into(), "(Lisp command)".into())),
    );
    names
}

static BUILTIN_NAMES: std::sync::LazyLock<Vec<(&'static str, &'static str)>> =
    std::sync::LazyLock::new(dsh_builtin::get_all_commands);
