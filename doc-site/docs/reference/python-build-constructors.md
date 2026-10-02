# Program.build constructor rules

The production BuildStatement enum is the closed statement dispatch vocabulary.
PythonBuildStatement exposes its inventory for the conformance gate. Classification
is syntactic: compilation still validates every premise and the complete Program
root. A new variant requires an exhaustive lowering arm and a rule here.

Each rule links positive live matrix programs and negative programs with expected
diagnostics. The gate checks the witness AST using production classification,
not a substring. Sequence constraints apply across constructors. These are outer
DAG-building rules, not restrictions on Monty's inner Python statements.

Class/decorator admission, catalog calls and scalar expressions are separate
families, not automatically registered by this gate. The listed negatives are
finite premise witnesses, not exhaustive invalid Python generation.

```plasm-build-constructors
[
  {
    "operation": "documentation",
    "premise": "Literal string statement before return",
    "transfer": "No DAG node, value, dependency or effect",
    "law": "BC-03",
    "witnesses": [
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "42\nreturn E.get(\"i1\")",
        "error": "unsupported build statement"
      }
    ]
  },
  {
    "operation": "effect_statement",
    "premise": "Discarded call lowers to a write or side effect, including effectful fanout/iteration",
    "transfer": "Retain effect node and causal order even when not explicitly returned; publish effect evidence",
    "law": "BC-04",
    "witnesses": [
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "E.get(\"i1\")\nreturn E.get(\"i2\")",
        "error": "unused expression statements must be writes"
      },
      {
        "body": "E.query().take(1)\nreturn E.get(\"i1\")",
        "error": "unused expression statements must be writes"
      }
    ]
  },
  {
    "operation": "binding",
    "premise": "One local name, no supplied symbol or method shadowing; admitted DAG-producing RHS",
    "transfer": "Create a fresh immutable DAG version with inferred shape, cardinality, coverage, ordering, authority and effects; publish after resolving RHS; earlier captures keep their version; no IO during build",
    "law": "BC-03",
    "witnesses": [
      "root_build_statements"
    ],
    "invalid": [
      {
        "body": "Program = E.get(\"i1\")\nreturn Program",
        "error": "reserved binding"
      },
      {
        "body": "a, b = E.get(\"i1\")\nreturn a",
        "error": "immutable local assignments"
      },
      {
        "body": "a = b = E.get(\"i1\")\nreturn a",
        "error": "unsupported build statement"
      },
      {
        "body": "a = E.get(\"i1\")\na += E.get(\"i2\")\nreturn a",
        "error": "unsupported build statement"
      }
    ]
  },
  {
    "operation": "callback",
    "premise": "Synchronous lexical callable accepting one row under upstream Python binding",
    "transfer": "Seal lexical callback version and default dependencies; instantiate its upstream flow at the consumer",
    "law": "BC-03",
    "witnesses": [
      "callback_lexical_record"
    ],
    "invalid": [
      {
        "body": "async def act(row):\n    return row\nreturn E.get('i1')",
        "error": "synchronous"
      }
    ]
  },
  {
    "operation": "return",
    "premise": "Explicit terminal return of an admitted result or nonempty tuple of results",
    "transfer": "Seal ordered return roots and separately retained effects; following statements are unreachable and emit no nodes",
    "law": "BC-03",
    "witnesses": [
      "root_build_statements",
      "parallel"
    ],
    "invalid": [
      {
        "body": "return",
        "error": "build requires materialized return roots"
      },
      {
        "body": "return ()",
        "error": "at least one rowset"
      },
      {
        "body": "a = E.get(\"i1\")",
        "error": "build requires materialized return roots"
      }
    ]
  }
]
```

`python_compute_dictionary_and_multiple_inputs_live` additionally executes local
reassignment and an unreachable statement after return. These change local
bindings, not the immutable DAG nodes already captured by earlier expressions.
