# Deferred row constructor rules

The compiler's closed `PythonRowOperation` registry is the denominator for this
surface. Dispatch matches it exhaustively. The matrix requires exactly one rule
per operation, a positive executable matrix witness and a negative admission
program with a required diagnostic. Positive witnesses are compiled and inspected
for typed compute operations, scoped record/row outputs, iteration payloads or
explicit read page budgets; source spelling is not evidence. The shared gate
rejects removed constructor evidence as well as missing and duplicate rules. Adding an enum variant without an implementation fails compilation;
adding dispatch without these evidence links fails the matrix.

These rules specialize Q = (shape, cardinality, coverage, ordering, authority,
effects) from [the boundary laws](python-conformance.md). They are not an inner
Python expression whitelist. Build statements have their own [closed rule gate](python-build-constructors.md).
[Catalog operations](python-catalog-constructors.md), class/decorator admission, scalar expressions and Monty profile
classification have separate admission authorities;
this registry does not claim automatic coverage of those paths or all compositions.

```plasm-constructors
[
  {
    "operation": "map",
    "premise": "Scoped row, typed record body and hard bound",
    "transfer": "S becomes recursive output shape; retain parent O, bound K and body E",
    "law": "BC-03",
    "witnesses": [
      "scoped_nested_records"
    ],
    "invalid": [
      {
        "body": "return E.query().map(42, max_parents=1)",
        "error": "expected a lambda or a declared scoped callback"
      }
    ]
  },
  {
    "operation": "flat_map",
    "premise": "Typed rowset/effect body and bounded parent occurrences",
    "transfer": "Concatenate child O; preserve child authority; combine C and E",
    "law": "BC-03",
    "witnesses": [
      "scoped_flat_map_format"
    ],
    "invalid": [
      {
        "body": "return E.query().flat_map(42)",
        "error": "expected a lambda or a declared scoped callback"
      }
    ]
  },
  {
    "operation": "iterate",
    "premise": "Identity-bearing Get source, stop predicate and hard bound",
    "transfer": "Re-observe after E; retain identity; exhaustion is failure",
    "law": "BC-03",
    "witnesses": [
      "iterate_bound_identity"
    ],
    "invalid": [
      {
        "body": "return E.get(\"i1\").iterate(lambda r: r, until=lambda r: r.id == \"i1\", max_steps=0)",
        "error": "max_steps must be a positive u32"
      }
    ]
  },
  {
    "operation": "page_size",
    "premise": "Positive literal directly on query/search",
    "transfer": "Changes acquisition budget, not logical S or backend coordinate width",
    "law": "BC-03",
    "witnesses": [
      "complete_page_size"
    ],
    "invalid": [
      {
        "body": "return E.query().page_size(0)",
        "error": "page_size requires a positive u32"
      }
    ]
  },
  {
    "operation": "aggregate",
    "premise": "Typed aggregation expressions over complete source",
    "transfer": "Synthetic scalar shape with defined empty behavior; remove receiver authority",
    "law": "BC-03",
    "witnesses": [
      "reduce_lang_aggregate"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.no_such_operator())",
        "error": "unsupported aggregate function"
      }
    ]
  },
  {
    "operation": "group_by",
    "premise": "Declared keys and typed aggregations over complete source",
    "transfer": "Synthetic key/aggregate shape; preserve lawful group ordering",
    "law": "BC-03",
    "witnesses": [
      "reduce_lang_group_by"
    ],
    "invalid": [
      {
        "body": "return E.query().group_by(\"id\", n=other.count())",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "distinct",
    "premise": "Declared value keys or whole public shape",
    "transfer": "First-wins multiplicity reduction; preserve row shape and provenance",
    "law": "BC-03",
    "witnesses": [
      "reduce_lang_dedupe"
    ],
    "invalid": [
      {
        "body": "return E.query().distinct(key=\"id\")",
        "error": "distinct accepts only literal field names"
      }
    ]
  },
  {
    "operation": "select",
    "premise": "Declared source fields or typed scalar derivations",
    "transfer": "Narrow/derive S; demanded missing fields fail; no fabricated receiver authority",
    "law": "BC-03",
    "witnesses": [
      "projection"
    ],
    "invalid": [
      {
        "body": "return E.query().select()",
        "error": "select requires fields"
      }
    ]
  },
  {
    "operation": "order_by",
    "premise": "Declared sortable field and literal direction",
    "transfer": "Stable ordering of source values; S and provenance preserved",
    "law": "BC-03",
    "witnesses": [
      "sort_limit"
    ],
    "invalid": [
      {
        "body": "return E.query().order_by(\"id\", descending=1)",
        "error": "descending requires a literal Boolean"
      }
    ]
  },
  {
    "operation": "union",
    "premise": "Compatible public row shapes",
    "transfer": "First-wins set union in left/right order; combine presence and coverage",
    "law": "BC-03",
    "witnesses": [
      "union_collapses_duplicates"
    ],
    "invalid": [
      {
        "body": "return E.query().union()",
        "error": "row operation requires one positional argument"
      }
    ]
  },
  {
    "operation": "take",
    "premise": "Nonnegative literal bound; zero returns an empty selection",
    "transfer": "Bound K for requested prefix; no unique-identity inference",
    "law": "BC-03",
    "witnesses": [
      "sort_limit",
      "prefix_zero_rows"
    ],
    "invalid": [
      {
        "body": "return E.query().take(-1)",
        "error": "expected an integer literal"
      },
      {
        "body": "return E.query().take(4294967296)",
        "error": "take requires a nonnegative u32"
      }
    ]
  },
  {
    "operation": "where",
    "premise": "Typed predicate over admitted source shape",
    "transfer": "Preserve order/multiplicity among matches; cannot strengthen C or field presence",
    "law": "BC-03",
    "witnesses": [
      "union_rowset"
    ],
    "invalid": [
      {
        "body": "return E.query().where()",
        "error": "row operation requires one positional argument"
      }
    ]
  }
]
```
