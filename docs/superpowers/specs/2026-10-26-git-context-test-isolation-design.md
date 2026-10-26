# Hermetic git context tests

The current no-marker tests assume tempfile ancestors contain no .git marker.
That is not guaranteed (observed /tmp/.git failures). Product nearest-ancestor
search is correct and must remain unchanged. Include the related prompt fallback
test, which has the same assumption. Reproduce against current source with an
owned ancestor marker via TMPDIR, never alter shared /tmp or user repositories.

Keep the filesystem marker predicate at the public git_context entry point; move
the identical ancestor walk into a private predicate-accepting helper. Test the
no-marker case with an always-false predicate and assert traversal through every
ancestor. Keep real filesystem directory/file/nearest-marker tests. History uses
a private lookup-accepting helper so its cwd fallback test can supply None while
production supplies the same public marker walker. Add ancestor integration cases
for history and prompt, independent of outside ancestors.

Extract prompt command fallback into a private sibling-visible function; production
still calls it only after no marker. Test logical PATH/environment directly at
that fallback boundary, not by assuming host ancestors are empty. Keep async
public marker-priority integration with a real owned marker. No env overrides in
production, search boundaries, config, cache or shell semantics changes.

Self-review: replacing expectations with the production result would hide bugs;
assert deterministic absent/ancestor/nearest cases instead. Do not skip tests or
require privileged chroot. Private seams preserve exact production call order.
