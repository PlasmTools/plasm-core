# Python scalar semantic coverage

These families describe matrix obligations, not a closed Python grammar or a
production dispatch enumeration. Monty owns expression admission, inference and
execution. The matrix checks the declared witnesses through typed DAG validation
and live execution against independent expected results, plus genuine rejection
boundaries. Equivalent Python spellings need equivalent values and effects; they
do not need identical generated compute source or identical IL nodes.

```plasm-scalar-constructors
[
  {
    "operation": "text",
    "premise": "A string literal or recursively literal string addition; Python parsing supplies raw, adjacent and multiline string semantics.",
    "transfer": "Produce a pure singleton text node with exact string bytes.",
    "law": "BC-03",
    "witnesses": [
      "text_literal_binding",
      "text_literal_equals"
    ],
    "invalid": [
      {
        "body": "return \"x\" - 2",
        "error": "unsupported-operator"
      },
      {
        "body": "return \"x\" + 2",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "format",
    "premise": "An f-string captures typed singleton field dependencies; its generated compute body satisfies the Monty profile.",
    "transfer": "Assemble explicit captured inputs and produce a reviewed Python compute node returning string content.",
    "law": "BC-03",
    "witnesses": [
      "scoped_flat_map_format"
    ],
    "invalid": [
      {
        "body": "return f\"{missing}\"",
        "error": "missing"
      },
      {
        "body": "rows = E.query()\nreturn f\"{rows.title}\"",
        "error": "field input requires a proven singleton"
      }
    ]
  },
  {
    "operation": "field",
    "premise": "A non-relation field exists in the immediate declared shape and its source has a singleton proof.",
    "transfer": "Produce a dependent scalar extraction; retain the field contract and check exactly one observed value at runtime.",
    "law": "BC-03",
    "witnesses": [
      "cert_bind_singleton_field_scalar",
      "cert_get_singleton_field_scalar",
      "bound_get_scalar_keyword"
    ],
    "invalid": [
      {
        "body": "return E.query().title",
        "error": "field input requires a proven singleton"
      },
      {
        "body": "return E.query().take(2).title",
        "error": "field input requires a proven singleton"
      }
    ]
  }
]
```
