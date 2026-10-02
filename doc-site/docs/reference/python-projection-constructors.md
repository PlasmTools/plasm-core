# Python projection semantic coverage

These families describe matrix obligations, not a closed Python grammar or a
production dispatch enumeration. Monty owns expression admission, inference and
execution. The matrix checks the declared witnesses through typed DAG validation
and live execution against independent expected results, plus genuine rejection
boundaries. Equivalent Python spellings need equivalent values and effects; they
do not need identical generated compute source or identical IL nodes.

```plasm-projection-constructors
[
  {
    "operation": "field",
    "premise": "Current lambda row field; schema resolves its path",
    "transfer": "Preserve declared field value type",
    "law": "BC-03",
    "witnesses": [
      "render_parity_lang_with_mul"
    ],
    "invalid": [
      {
        "body": "return E.query().select(x=lambda self: self.score)",
        "error": "reserved projection parameter"
      }
    ]
  },
  {
    "operation": "literal",
    "premise": "Literal operand with a closed materialized value contract",
    "transfer": "Retain its scalar or collection kind",
    "law": "BC-03",
    "witnesses": [
      "render_parity_lang_with_mul"
    ],
    "invalid": [
      {
        "body": "return E.query().select(x=lambda row: {1: 2})",
        "error": "materialized dictionaries require string keys"
      }
    ]
  },
  {
    "operation": "arithmetic",
    "premise": "Typed operands with +, -, *, or /",
    "transfer": "Infer result type from admitted operand/operator combination",
    "law": "BC-03",
    "witnesses": [
      "render_parity_lang_with_mul",
      "render_parity_lang_with_div",
      "render_parity_lang_with_concat"
    ],
    "invalid": [
      {
        "body": "return E.query().select(x=lambda row: row.title - 2)",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "length",
    "premise": "len of a string, array or record",
    "transfer": "Produce typed length without arbitrary calls",
    "law": "BC-03",
    "witnesses": [
      "render_parity_lang_with_when_len"
    ],
    "invalid": [
      {
        "body": "return E.query().select(x=lambda row: len(7))",
        "error": "invalid-argument-type"
      }
    ]
  },
  {
    "operation": "conditional",
    "premise": "Boolean condition and typed branches",
    "transfer": "Join complete branch contracts; declare both dependencies; evaluate selected branch",
    "law": "BC-03",
    "witnesses": [
      "render_parity_lang_with_when_len"
    ],
    "invalid": [
      {
        "body": "return E.query().select(x=lambda row: row.title if missing else row.owner)",
        "error": "missing"
      }
    ]
  }
]
```
