# Calculator MCP

ArithmeticResult is identified by the supplied expression. Its result is a
string, matching upstream's declared return type; the catalogue does not coerce
that string to a number. The mapping selects the actual `structuredContent`
object emitted by Python MCP 1.13.1 for mcp_server_calculator 0.1.1.

Supported operators follow upstream's AST evaluator: addition, subtraction,
multiplication, division, floor division, remainder, exponentiation and unary
negation. Division by zero arrives as an MCP `isError` result and fails Plasm
execution. It is not decoded as an empty or successful result.

The live slice checks `(7 + 5) / 3` and `1 / 0` through both the upstream protocol
and Plasm's public MCP interface. No expression answers are embedded in teaching.
