# Python literal semantic coverage

These families describe matrix obligations, not a closed Python grammar or a
production dispatch enumeration. Monty owns expression admission, inference and
execution. The matrix checks the declared witnesses through typed DAG validation
and live execution against independent expected results, plus genuine rejection
boundaries. Equivalent Python spellings need equivalent values and effects; they
do not need identical generated compute source or identical IL nodes.

```plasm-literal-constructors
[
  {
    "operation": "text",
    "premise": "String literals or recursively literal string addition.",
    "transfer": "Preserve exact parsed text.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": \"x\" + 2}, max_parents=1)",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "number",
    "premise": "An i64 integer or finite real float; no complex numbers.",
    "transfer": "Retain integer versus float representation; reject overflow.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": 9223372036854775808}, max_parents=1)",
        "error": "integer out of range"
      }
    ]
  },
  {
    "operation": "signed",
    "premise": "Unary plus/minus applies directly to a numeric literal.",
    "transfer": "Parse signed magnitude together so i64::MIN is exact; preserve floating sign.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": -\"invalid\"}, max_parents=1)",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "boolean",
    "premise": "True or False are literal booleans, not arithmetic operands.",
    "transfer": "Retain Boolean; catalog type admission remains a separate check.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": not missing}, max_parents=1)",
        "error": "missing"
      }
    ]
  },
  {
    "operation": "null",
    "premise": "None is the explicit null literal, distinct from an unobserved field.",
    "transfer": "Retain Null; destination nullability is checked separately.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": missing or False}, max_parents=1)",
        "error": "missing"
      }
    ]
  },
  {
    "operation": "array",
    "premise": "A list recursively contains admitted values or explicit typed dependencies.",
    "transfer": "Preserve ordered elements and multiplicity; contextual schema unification still applies.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": [*1]}, max_parents=1)",
        "error": "Starred"
      }
    ]
  },
  {
    "operation": "record",
    "premise": "Keys are literal strings without unpacking or duplicates. Empty records and write payloads are admitted.",
    "transfer": "Recursively retain named values; never silently overwrite duplicates.",
    "law": "BC-01",
    "witnesses": [
      "operand_recursive_literals"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").map(lambda row: {\"value\": {\"x\": 1, \"x\": 2}}, max_parents=1)",
        "error": "duplicate output field"
      }
    ]
  }
]
```
