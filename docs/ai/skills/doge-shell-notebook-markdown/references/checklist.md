# Notebook Markdown Checklist

- Preserve markdown rendering for lists, quotes, links, code blocks, and ANSI output.
- Keep notebook parsing and notebook execution concerns separate.
- Check empty output, large output, and mixed stdout/stderr display paths.
- Update shared `dsh-types` tests when notebook or output history shapes change.
- Prefer focused `dsh-builtin` tests before broad workspace validation.
