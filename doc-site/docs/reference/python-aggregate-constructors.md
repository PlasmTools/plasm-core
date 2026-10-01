# Aggregate descriptor constructors

The production `PythonAggregateDescriptor` inventory owns the seven spellings,
conversion to typed aggregate functions and exhaustive argument dispatch. Count
accepts no field; sum, average, minimum, maximum, first and last accept one literal
field path. Descriptors are parsed DAG syntax, never executed Python functions.
This registration preserves current admission, including positional field arguments.

The shared gate requires exact inventory, recursive typed aggregate/group payload
witnesses with the corresponding field presence, and diagnostic-specific negative
programs. Existing live matrix cases cover all seven descriptors, grouped count
and first, aliased sum and selected empty-input behavior. Registry mutation checks
reject omissions, duplicates, dangling witnesses and missing constructor evidence.

The separate [reduction-v1 model](python-conformance.md#finite-reduction-algebra)
adds 48 complete-input products over nullable integers, empty inputs, group order
and direct/aliased fields. Complete operator/type, coverage and nested reduction
products remain explicit obligations in the [coverage audit](python-constructor-audit.md).
First/last retain the complete input value contract (including domain, enum,
reference target and recursive shape), with nullable output for global empty
input. They do not convert every result to Number. The primitive_reductions
family checks all 14 CGS FieldType variants through aliases and typed Monty
consumption (60 forward/reverse/empty programs).
The row-method gate separately owns aggregate/group/distinct invocation structure.

```plasm-aggregate-constructors
[
  {
    "operation": "count",
    "premise": "No arguments",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_lang_aggregate",
      "reduce_lang_group_by",
      "reduce_aggregate_empty"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.count(\"id\"))",
        "error": "agg.count takes no arguments"
      },
      {
        "body": "return E.query().aggregate(n=agg.count(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "sum",
    "premise": "Exactly one literal field path, resolved against current row shape",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_aggregate_functions",
      "reduce_aggregate_alias",
      "reduce_aggregate_empty"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.sum())",
        "error": "aggregate requires one literal field name"
      },
      {
        "body": "return E.query().aggregate(n=agg.sum(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "avg",
    "premise": "Exactly one literal field path, resolved against current row shape",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_aggregate_functions"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.avg())",
        "error": "aggregate requires one literal field name"
      },
      {
        "body": "return E.query().aggregate(n=agg.avg(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "min",
    "premise": "Exactly one literal field path, resolved against current row shape",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_aggregate_functions"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.min())",
        "error": "aggregate requires one literal field name"
      },
      {
        "body": "return E.query().aggregate(n=agg.min(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "max",
    "premise": "Exactly one literal field path, resolved against current row shape",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_aggregate_functions"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.max())",
        "error": "aggregate requires one literal field name"
      },
      {
        "body": "return E.query().aggregate(n=agg.max(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "first",
    "premise": "Exactly one literal field path, resolved against current row shape",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_aggregate_functions",
      "reduce_lang_group_by_first"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.first())",
        "error": "aggregate requires one literal field name"
      },
      {
        "body": "return E.query().aggregate(n=agg.first(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  },
  {
    "operation": "last",
    "premise": "Exactly one literal field path, resolved against current row shape",
    "transfer": "Typed synthetic result; no receiver authority. Existing reduction runtime owns empty/null/order semantics.",
    "law": "BC-03",
    "witnesses": [
      "reduce_aggregate_functions"
    ],
    "invalid": [
      {
        "body": "return E.query().aggregate(n=agg.last())",
        "error": "aggregate requires one literal field name"
      },
      {
        "body": "return E.query().aggregate(n=agg.last(field=\"score\"))",
        "error": "expected a literal agg descriptor"
      }
    ]
  }
]
```

### Declared ordering contract

`agg.min` and `agg.max` require an orderable declared operand, shared with
`order_by`. Supported ordering domains are numeric values, Boolean, string-like
primitives, exact money under its currency law, and date/datetime/time/timedelta.
Timezone objects and composite records/arrays are not ordered by these row
operators. Union variants must share an ordering domain. Naive and aware temporal
values cannot be ordered together. Temporal values compare by their declared
kind and encoding, not lexical wire representation.

The input contract survives empty materialization; min/max preserve it with
nullable output. Unsupported domains reject during admission, including when
there are no rows. Malformed singleton ordered values fail during execution.
`primitive_reductions::temporal_ordering_contract_end_to_end` covers the Python
alias → sort → grouped/global extrema → typed Monty consumer path. Core ordering
properties and runtime `row_compute::contract_tests` cover independent numeric
and temporal laws. These witnesses do not claim the complete operator/type
product is finished.

### Declared equality and arithmetic capabilities

Grouping and distinct derive semantic keys from the recursive field contract.
Temporal keys compare normalized instants/durations; equivalent offset spellings
produce the same key and hash. This applies inside typed records and arrays.
Missing fields remain distinct from present null fields. The first representative
keeps its original payload and encoding. Untyped JSON is not interpreted as a
temporal value merely because it contains a date-looking string or tag.

A union value must have an unambiguous semantic interpretation. If two matching
temporal encodings assign different instants to the same value, key construction
and ordering reject it; variant order cannot choose its meaning. Naive and aware
keys differ, while ordering across them remains invalid. Numeric key identity
retains native representation-sensitive equality; ordered numeric comparison is
exact across numeric representations.

`sum` and `avg` require a common arithmetic dimension across all union variants.
Integer/number unions are numeric; money unions are money; money mixed with a
number is not an additive domain. Empty sums use the declared domain's zero.
Averages are nullable numbers or nullable money. Binary arithmetic distributes
over every operand variant through the same `Arithmetic` capability interface.
Computed results do not inherit the input's catalog constraint proof.

Streaming `order_by(...).take(n)` resolves the same declared ordering as full row
sorting before page acquisition. It preserves input order among equal keys and
puts nulls last in either direction. Missing or malformed sort values are errors.
`top_k::laws` checks bounded selection against stable full sort/take; the live
`temporal_equivalence_contract_end_to_end` witness crosses direct/nested keys with
grouping/distinct. These are semantic refinements, with no new Python syntax.
