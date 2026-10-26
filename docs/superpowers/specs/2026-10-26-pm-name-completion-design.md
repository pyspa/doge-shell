# Literal pm add name completion

Public completion currently passes raw specified_arguments[0] to Path::file_name
and trims slash/backslash. Quotes remain in names; literal trailing backslashes
are lost. CommandLineParser preserves raw source, so decode exactly once at this
consumer with shell_path::decode_literal_word. Decode current_token once for name
matching too. Keep candidates raw; insertion formatting still belongs to the
existing shell_path formatter. Trim only / on supported Linux/macOS platforms.

No parser/global decoder changes, expansion execution, filesystem canonicalization,
project persistence, provider registration, other pm subcommands or ghost changes.
Use actual temporary directories and the public IntegratedCompletionEngine::complete
API. Pin empty name query and encoded partial queries, plain/single/double source
styles, spaces/Unicode/quotes/interior and trailing/repeated literal backslashes,
optional trailing /, repeated requests and insertion round-trip. Exact candidate
name assertions catch accidental double decoding. Negative query emits no path
name. Preserve existing plain/slash behavior and type/priority/description.

Self-review: trimming source before decoding confuses quoting and path data.
Decoding a previously normalized value twice would lose an interior backslash;
raw parser input plus interior/repeated-backslash regressions prevent that. Do not
expand variables or execute substitutions during suggestion. No unresolved scope
choice needs user confirmation. Independently review final diff and Linux/macOS CI.
