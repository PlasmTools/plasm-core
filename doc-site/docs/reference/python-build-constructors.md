# Program.build constructor rules

The production BuildStatement enum is the closed statement dispatch vocabulary.
PythonBuildStatement exposes its inventory for the conformance gate. Classification
is syntactic: compilation still validates every premise and the complete Program
root. A new variant requires an exhaustive lowering arm and a rule here.

Each rule links positive live matrix programs and negative programs with expected
semantic error variants. Each `invalid.error` is a closed serde enum label matched
against the public typed compilation/lowering cause, never diagnostic prose.
The gate checks the witness AST using production classification,
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
        "error": "BuildExpression"
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
        "error": "UnusedNonWriteExpression"
      },
      {
        "body": "E.query().take(1)\nreturn E.get(\"i1\")",
        "error": "UnusedNonWriteExpression"
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
        "error": "ReservedBindingName"
      },
      {
        "body": "a, b = E.get(\"i1\")\nreturn a",
        "error": "MutableLocalAssignment"
      },
      {
        "body": "a = b = E.get(\"i1\")\nreturn a",
        "error": "BuildAssignmentTargets"
      },
      {
        "body": "a = E.get(\"i1\")\na += E.get(\"i2\")\nreturn a",
        "error": "BuildAugmentedAssignment"
      }
    ]
  },
  {
    "operation": "finite_for",
    "premise": "Synchronous for without else; one non-reserved local target; literal list or tuple of string/integer IDs, directly supplied or remembered through a local binding; at most 256 total static expansions across the build",
    "transfer": "Elaborate each literal item in order into fresh immutable DAG bindings; retain body effects and causal order without executing user Python during build; reject return inside iteration and dynamic iterables",
    "law": "BC-04",
    "witnesses": [
      "static_literal_for_effects"
    ],
    "invalid": [
      {
        "body": "keys = E.query()\nfor key in keys:\n    E.get(key).PING()\nreturn keys",
        "error": "StaticIterationRequiresLiteralIds"
      },
      {
        "body": "for key in ['i1']:\n    return E.get(key)",
        "error": "StaticIterationReturn"
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
        "error": "CallbackDeclarationShape"
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
        "error": "BranchingReturnUnsupported"
      },
      {
        "body": "return ()",
        "error": "EmptyReturn"
      },
      {
        "body": "a = E.get(\"i1\")",
        "error": "BranchingReturnUnsupported"
      }
    ]
  }
]
```

`python_compute_dictionary_and_multiple_inputs_live` additionally executes local
reassignment and an unreachable statement after return. These change local
bindings, not the immutable DAG nodes already captured by earlier expressions.

`static_literal_for_effects`, executed by `python_static_literal_iteration_live`,
uses the abstract language matrix to expand two literal IDs. Its independent live
expectations require the returned `i1` row, two retained effect envelopes, exactly
two completed logical invocations and no failed invocations. The companion
`python_static_literal_iteration_rejects_dynamic_or_unbounded_forms` also checks
the shared 256-expansion limit; these witnesses exercise existing lowering rules.
