# Signed arithmetic variable radix

Variable reads reuse parse_integer_literal, which detects 0x/leading0 before
considering sign. Thus -010 becomes decimal -10, +010 decimal10, -0x10 fails and
-08 is accepted. Latest develop has no overlapping PR. Existing integer-only
variable policy remains authoritative: no recursive arithmetic expressions.

Strip one optional sign for radix detection and digit validation, then parse the
signed digit string with i64::from_str_radix. This supports negative magnitude
2^63 without parsing an overflowing positive i64 first. Reject empty/double signs,
invalid base digits and out-of-range values. Unsigned tokens retain existing rules.
Grammar/tokenization, direct literal overflow policy, checked operators, RHS order,
short-circuiting and environment/export write authority remain unchanged.

Test old and new paths before implementation. Cover +/-octal, +/-hex, decimal,
zero, invalid08/hex/multiple signs, positive overflow and negative underflow,
i64::MIN in all bases, variable assignments/compound operations and unchanged
error state. Add public contract and end-to-end exact status/output regressions.

Self-review: negating parsed positive i64 cannot represent MIN; parsing signed
digits directly avoids that bug. Signs must not bypass digit validation. Do not
make arbitrary text evaluate recursively or alter the parser's unary AST grammar.
