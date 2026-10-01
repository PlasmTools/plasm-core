# Python predicate semantic coverage

These families describe matrix obligations, not a closed Python grammar or a
production dispatch enumeration. Monty owns expression admission, inference and
execution. The matrix checks the declared witnesses through typed DAG validation
and live execution against independent expected results, plus genuine rejection
boundaries. Equivalent Python spellings need equivalent values and effects; they
do not need identical generated compute source or identical IL nodes.

```plasm-predicate-constructors
[
  {
    "operation": "and",
    "premise": "Typed row comparison leaves; all operands admitted",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "complete_boolean_literal",
      "predicate_truth_and"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: row.owner == \"alice\" and missing)",
        "error": "missing"
      }
    ]
  },
  {
    "operation": "or",
    "premise": "Typed row comparison leaves; all operands admitted",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "complete_boolean_rowset"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: row.owner == \"alice\" or missing)",
        "error": "missing"
      }
    ]
  },
  {
    "operation": "not",
    "premise": "Typed row comparison leaves; one admitted predicate",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "complete_boolean_literal"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: not missing)",
        "error": "missing"
      }
    ]
  },
  {
    "operation": "comparison",
    "premise": "Typed scalar comparison; chained expressions use recursive Boolean value lowering",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "where"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: 0 < row.title < 10)",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "scalar",
    "premise": "Typed row comparison leaves; supported comparison with a typed scalar operand",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "where",
      "predicate_truth_string",
      "predicate_truth_empty",
      "predicate_truth_null",
      "predicate_truth_iteration"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: row.score < \"invalid\")",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "null_test",
    "premise": "Explicit None operand with is/is not or equality/inequality; field presence remains required",
    "transfer": "Total Boolean via Exists, optionally negated; does not inherit three-valued row comparison semantics. A successful non-null test can refine immutable references only in its dominated branch.",
    "law": "BC-03",
    "witnesses": [
      "predicate_null_test",
      "predicate_null_filter"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: row.score < \"invalid\")",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "contains",
    "premise": "Typed row comparison leaves; literal string in the current row field",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "complete_predicate_contains"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: \"alice\" in other.owner)",
        "error": "other"
      }
    ]
  },
  {
    "operation": "literal_membership",
    "premise": "Finite typed list/tuple sequence; equality follows the shared value contract",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "complete_boolean_literal"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: row.owner in (unknown,))",
        "error": "unknown"
      }
    ]
  },
  {
    "operation": "rowset_membership",
    "premise": "Typed row comparison leaves; closed read-only one-column rowset with complete membership evidence",
    "transfer": "Preserve source order and row shape; filter does not promote coverage or receiver authority",
    "law": "BC-03",
    "witnesses": [
      "inline_membership",
      "where_in_rowset",
      "where_not_in_rowset"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: row.owner in E.query(owner=row.owner).select(\"owner\"))",
        "error": "membership RHS must be closed"
      },
      {
        "body": "return E.query().where(lambda row: row.owner in E.CREATE(title=\"hidden\").select(\"owner\"))",
        "error": "Python value expressions cannot acquire write authority"
      },
      {
        "body": "return E.query().where(lambda row: row.owner in E.get(\"i1\").score)",
        "error": "unsupported-operator"
      }
    ]
  },
  {
    "operation": "any",
    "premise": "Complete typed collection and pure Boolean predicate with a lexically scoped element",
    "transfer": "Short-circuit predicate evaluation; preserve captured type and authority, empty any is false and empty all is true; no effects in predicate scope",
    "law": "BC-03",
    "witnesses": [
      "predicate_any_relation",
      "predicate_truth_any"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: any(child.missing for child in row.REL_COMPLETE_LINES))",
        "error": "missing"
      }
    ]
  },
  {
    "operation": "all",
    "premise": "Complete typed collection and pure Boolean predicate with a lexically scoped element",
    "transfer": "Short-circuit predicate evaluation; preserve captured type and authority, empty any is false and empty all is true; no effects in predicate scope",
    "law": "BC-03",
    "witnesses": [
      "predicate_all_relation"
    ],
    "invalid": [
      {
        "body": "return E.query().where(lambda row: all(child.missing for child in row.REL_COMPLETE_LINES))",
        "error": "missing"
      }
    ]
  }
]
```

Filter, quantifier, and iteration stop consumers apply Python `bool` to the admitted value. Operand-selecting `and`/`or` remain Python expressions; only their consumer truth-tests the result. Witnesses: `predicate_truth_string`, `predicate_truth_and`, `predicate_truth_empty`, `predicate_truth_null`, `predicate_truth_iteration`, `predicate_truth_any`.
