# Serve Web Checklist

- Preserve path traversal protection for every filesystem-backed request.
- Keep route parsing, static file lookup, and response formatting separated.
- Check CORS behavior when adding handlers or changing server config.
- Loopback binding alone does not stop DNS rebinding. Validate Host and URI authority before filesystem access, including when CORS is enabled; reject duplicate or conflicting external authorities.
- Update `dsh-types/src/mcp.rs` only for shared wire-shape changes.
- Prefer focused `dsh-builtin/src/serve/` tests before broader workspace checks.
