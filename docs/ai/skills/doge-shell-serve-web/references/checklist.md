# Serve Web Checklist

- Preserve path traversal protection for every filesystem-backed request.
- Keep route parsing, static file lookup, and response formatting separated.
- Check CORS behavior when adding handlers or changing server config.
- Update `dsh-types/src/mcp.rs` only for shared wire-shape changes.
- Prefer focused `dsh-builtin/src/serve/` tests before broader workspace checks.
