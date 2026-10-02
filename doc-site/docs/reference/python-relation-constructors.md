# Relation navigation constructor

`PythonRelationOperation` is the production inventory. `navigate` is attribute
navigation on a proven singleton, whether at root or inside a correlated body.
`flat_map` is a registered row constructor that supplies a local row; it does not
introduce a second relation syntax. One/many targets and materialization strategies
are catalog contracts, not additional Python constructors.

The executable gate recursively inspects typed relation payloads, including nested
`MapBody` plans. Witness expectations pin target catalog/entity, relation cardinality,
correlated context and required parent-binding proofs. Each linked program runs in
the ordinary live matrix. Negative examples assert specific diagnostic causes.
The gate rejects omitted/duplicate rules and false or dangling evidence.

Existing matrix tests additionally check wire-name/r# equivalence, foreign-symbol
rejection, serialized scoped reads and child rehydration after cache eviction.
This finite evidence does not certify every materialization × presence × scope ×
failure combination. [Scalar field access](python-scalar-constructors.md) has its own constructor gate.
[Recursive literal operands](python-literal-constructors.md) have a separate gate;
[class/decorator admission](python-declaration-constructors.md) is registered separately. Relation expressions are not admitted as iteration effects.

```plasm-relation-constructors
[
  {
    "operation": "navigate",
    "premise": "The source has a singleton proof, entity continuation authority and an anchor; the relation belongs to its qualified source entity.",
    "transfer": "Retain qualified target, declared cardinality, materialization and parent binding proofs. A correlated body applies this same constructor once per parent; it preserves parent order and empty input yields no children.",
    "law": "BC-03",
    "witnesses": [
      {
        "id": "prefix_union_single_relation",
        "relation": "tags",
        "target": "LangTag",
        "cardinality": "many",
        "correlated": false,
        "scoped": false
      },
      {
        "id": "relation_one_chain",
        "relation": "detail",
        "target": "LangDetail",
        "cardinality": "one",
        "correlated": false,
        "scoped": false
      },
      {
        "id": "relation_relation_lines",
        "relation": "lines",
        "target": "LangLine",
        "cardinality": "many",
        "correlated": false,
        "scoped": false
      },
      {
        "id": "relation_bind_projection_then_relation",
        "relation": "tags",
        "target": "LangTag",
        "cardinality": "many",
        "correlated": false,
        "scoped": false
      },
      {
        "id": "relation_relation_many_from_plural_query",
        "relation": "tags",
        "target": "LangTag",
        "cardinality": "many",
        "correlated": true,
        "scoped": false
      },
      {
        "id": "relation_empty_fanout",
        "relation": "tags",
        "target": "LangTag",
        "cardinality": "many",
        "correlated": true,
        "scoped": false
      },
      {
        "id": "cert_relation_opaque_r_symbol",
        "relation": "tags",
        "target": "LangTag",
        "cardinality": "many",
        "correlated": true,
        "scoped": false
      },
      {
        "id": "relation_relation_integer_scoped_bindings",
        "relation": "tags_by_score",
        "target": "LangTag",
        "cardinality": "many",
        "correlated": true,
        "scoped": true
      }
    ],
    "invalid": [
      {
        "body": "return E.query().tags",
        "error": "relation dot requires a singleton"
      },
      {
        "body": "return E.query().take(2).tags",
        "error": "relation dot requires a singleton"
      },
      {
        "body": "a = E.get(\"i1\")\nb = E.get(\"i2\")\nreturn a.union(b).tags",
        "error": "relation dot requires a singleton"
      },
      {
        "body": "seed = E.get(\"i1\")\nreturn seed.iterate(lambda row: row.tags, until=lambda row: row.score is not None and row.score > 0, max_steps=2)",
        "error": "invalid iteration step"
      }
    ]
  }
]
```
