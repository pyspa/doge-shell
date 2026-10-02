//! Side-effect-free invocation expansion shared by live and dry materialization.
use super::field_split::{ExpandedSegment, IfsSpec};
use super::plan::{InvocationParameter, QuoteMode};
use super::word_expand::{ExpansionContext, dynamic_text_segment};
use crate::shell::expansion_host::ExpansionHost;

fn positional_join_separator(ifs: &IfsSpec) -> String {
    match ifs {
        IfsSpec::Default => " ".into(),
        IfsSpec::Disabled => String::new(),
        IfsSpec::Custom(value) => value.chars().next().map(String::from).unwrap_or_default(),
    }
}

pub(super) fn invocation_scalar(
    parameter: InvocationParameter,
    shell: &impl ExpansionHost,
) -> String {
    let env = shell.expansion_environment().read();
    let invocation = env.invocation();
    match parameter {
        InvocationParameter::Arg0 => invocation.argv0.clone(),
        InvocationParameter::Positional(n) => n
            .checked_sub(1)
            .and_then(|i| invocation.positional.get(i))
            .cloned()
            .unwrap_or_default(),
        InvocationParameter::Count => invocation.positional.len().to_string(),
        InvocationParameter::At | InvocationParameter::Star => {
            let ifs = env.lookup_variable("IFS");
            invocation
                .positional
                .join(&positional_join_separator(&IfsSpec::resolve(
                    ifs.as_deref(),
                )))
        }
    }
}

pub(super) fn invocation_segments(
    parameter: InvocationParameter,
    quote: QuoteMode,
    shell: &impl ExpansionHost,
    context: ExpansionContext,
) -> Vec<ExpandedSegment> {
    let quoted = quote != QuoteMode::Unquoted;
    if parameter == InvocationParameter::At
        && !matches!(
            context,
            ExpansionContext::Assignment | ExpansionContext::Redirect
        )
    {
        let values = shell
            .expansion_environment()
            .read()
            .invocation()
            .positional
            .clone();
        let len = values.len();
        values
            .into_iter()
            .enumerate()
            .map(|(i, value)| {
                let mut segment = dynamic_text_segment(value, quoted, context);
                segment.field_boundary_after = i + 1 < len;
                segment
            })
            .collect()
    } else {
        vec![dynamic_text_segment(
            invocation_scalar(parameter, shell),
            quoted,
            context,
        )]
    }
}
