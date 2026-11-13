# Serve Web Checklist

- Preserve path traversal protection for every filesystem-backed request.
- Pin the public root at router creation. Resolve allowed in-root links, then open each component relative to that handle with no-follow semantics; stream/list the opened handles. Never reopen a canonicalized pathname after a containment check. Cover file, parent, index, and root replacement races, including directory listings.
- Preserve read-only listing and search-only index permissions separately. Reject search-only fds before passing ownership to `fdopendir`; for duplicated root handles, serialize listing and rewind before/after enumeration to avoid stale/shared offsets.
- Keep route parsing, static file lookup, and response formatting separated.
- Check CORS behavior when adding handlers or changing server config.
- Loopback binding alone does not stop DNS rebinding. Validate Host and URI authority before filesystem access, including when CORS is enabled; reject duplicate or conflicting external authorities.
- Update `dsh-types/src/mcp.rs` only for shared wire-shape changes.
- Prefer focused `dsh-builtin/src/serve/` tests before broader workspace checks.
