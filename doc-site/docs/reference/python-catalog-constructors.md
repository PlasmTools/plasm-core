# Catalog call constructors

The production `CatalogOperation` partition is the denominator: three read kinds
and four write kinds. Its conversion from `CapabilityKind` and both lowering
branches are exhaustive. Primary read names and served method symbols converge
on the same typed admission rules; symbols retain catalog/entity ownership.

The matrix gate resolves positive witnesses from compiled top-level invoke IL
against the abstract fixture catalog. Each witness also runs in the shared live
matrix. Negatives assert a specific admission diagnostic. This proves the seven
call categories, not every receiver × input × result × effect combination.
A spelling check compares the full plans for primary and symbolic Get, Query and
Search after resolving only absent primary capability pins against the fixture.
Explicit pins remain unchanged; raw commit identities need not match before that
resolution.

[Relation navigation](python-relation-constructors.md), scoped body constructors,
scalar expressions and class/decorator admission have separate obligations; they
are not catalog call kinds.

```plasm-catalog-constructors
[
  {
    "operation": "get",
    "premise": "Identity matches the declared simple, compound or pathless Get signature.",
    "transfer": "Produce catalog-owned singleton rows with the declared identity and result coverage.",
    "law": "BC-03",
    "witnesses": [
      "get"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\", identity=\"i2\")",
        "error": "multiple values for identity"
      }
    ]
  },
  {
    "operation": "query",
    "premise": "Named selection inputs satisfy the chosen query capability.",
    "transfer": "Produce catalog-owned plural rows; retain selection, capability and coverage.",
    "law": "BC-03",
    "witnesses": [
      "query"
    ],
    "invalid": [
      {
        "body": "return E.query(\"x\")",
        "error": "require named selection arguments"
      }
    ]
  },
  {
    "operation": "search",
    "premise": "Named search selection inputs satisfy the primary or explicitly selected search capability.",
    "transfer": "Pin the search capability in query IL; preserve catalog ownership and plural coverage.",
    "law": "BC-03",
    "witnesses": [
      "complete_search"
    ],
    "invalid": [
      {
        "body": "return E.search(\"x\")",
        "error": "require named selection arguments"
      }
    ]
  },
  {
    "operation": "create",
    "premise": "Named typed inputs satisfy the create capability and its receiver requirement.",
    "transfer": "Produce typed create IL with catalog stamp and declared effect/result contract.",
    "law": "BC-04",
    "witnesses": [
      "create"
    ],
    "invalid": [
      {
        "body": "return E.CREATE(\"x\")",
        "error": "writes require named arguments"
      }
    ]
  },
  {
    "operation": "update",
    "premise": "A proven singleton authoritative receiver and named typed inputs satisfy the capability.",
    "transfer": "Produce bound invoke IL; retain receiver identity and write ordering.",
    "law": "BC-04",
    "witnesses": [
      "update"
    ],
    "invalid": [
      {
        "body": "return E.UPDATE(title=\"x\")",
        "error": "requires an entity receiver"
      }
    ]
  },
  {
    "operation": "delete",
    "premise": "A proven singleton authoritative receiver satisfies the delete capability.",
    "transfer": "Produce bound delete IL and the declared result or acknowledgement.",
    "law": "BC-04",
    "witnesses": [
      "delete"
    ],
    "invalid": [
      {
        "body": "return E.DELETE()",
        "error": "requires an entity receiver"
      }
    ]
  },
  {
    "operation": "action",
    "premise": "Named typed inputs and root or singleton receiver match the declared action.",
    "transfer": "Produce invoke IL with declared side effects, dependencies and result shape.",
    "law": "BC-04",
    "witnesses": [
      "broadcast",
      "cert_effect_action_ping"
    ],
    "invalid": [
      {
        "body": "return E.PING()",
        "error": "requires an entity receiver"
      }
    ]
  }
]
```
