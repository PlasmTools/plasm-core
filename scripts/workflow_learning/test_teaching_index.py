"""Offline whole-program teaching contract witnesses; no provider calls or dependencies."""

import ast
from dataclasses import dataclass
import hashlib
import io
import json
import tokenize
import unittest


@dataclass(frozen=True, order=True)
class SemanticRef:
    kind: str
    catalog: str
    owner: str
    member: str


@dataclass(frozen=True)
class Occurrence:
    start: int
    end: int
    expected: str
    ref: SemanticRef



def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode()).hexdigest()


def token_ranges(source):
    lines = source.splitlines(keepends=True)

    def offset(position):
        line, col = position
        return len("".join(lines[: line - 1]).encode()) + len(
            (lines[line - 1] if line <= len(lines) else "")[:col].encode()
        )

    return {
        (offset(token.start), offset(token.end)): (token.type, token.string)
        for token in tokenize.generate_tokens(io.StringIO(source).readline)
        if token.type not in (tokenize.ENDMARKER, tokenize.ENCODING)
    }


def rewrite(source, occurrences, replacements):
    """Validate trusted resolver spans, then substitute; never infer symbol ownership."""
    tokens = token_ranges(source)
    edits = []
    for occurrence in occurrences:
        if tokens.get((occurrence.start, occurrence.end)) != (
            tokenize.NAME, occurrence.expected
        ):
            raise ValueError("reference must match a whole identifier token")
        replacement = replacements[occurrence.ref]  # Missing exposure fails closed.
        if not replacement.isidentifier():
            raise ValueError("exposure must be one identifier")
        edits.append((occurrence.start, occurrence.end, replacement))
    edits.sort()
    for left, right in zip(edits, edits[1:]):
        if left[1] > right[0]:
            raise ValueError("overlapping occurrences")
    result = source.encode()
    for start, end, replacement in reversed(edits):
        result = result[:start] + replacement.encode() + result[end:]
    result = result.decode()
    ast.parse(result)
    return result


def canonical_shape(source, occurrences):
    replacements = {
        item.ref: "ref_" + digest(tuple(vars(item.ref).values())) for item in occurrences
    }
    normalized = rewrite(source, occurrences, replacements)
    return digest(ast.dump(ast.parse(normalized), include_attributes=False))


@dataclass(frozen=True)
class Candidate:
    name: str
    facets: frozenset
    catalogs: frozenset
    contract: str


def retrieve(facets, allowed_catalogs, candidates, requested_catalog=None, limit=3):
    """Facet retrieval witness, not a learned semantic matcher or serving gate."""
    if limit < 0:
        raise ValueError("negative limit")
    eligible = [
        candidate for candidate in candidates
        if candidate.catalogs <= allowed_catalogs
        and (requested_catalog is None or requested_catalog in candidate.catalogs)
        and facets & candidate.facets
    ]
    return sorted(
        eligible,
        key=lambda candidate: (
            -len(facets & candidate.facets) / len(facets | candidate.facets),
            candidate.name,
        ),
    )[:limit]


def occurrence(source, token, ref, number=0):
    """Fixture helper only: production supplies compiler-resolved occurrence spans."""
    matches = [span for span, value in token_ranges(source).items()
               if value == (tokenize.NAME, token)]
    start, end = matches[number]
    return Occurrence(start, end, token, ref)


class TeachingTests(unittest.TestCase):
    entity = SemanticRef("entity", "payments", "", "Account")
    method = SemanticRef("method", "payments", "Account", "pay")

    def test_symbol_permutation_is_neutral(self):
        a, b = "e1.m2()", "e8.m9()"
        self.assertEqual(
            canonical_shape(a, [occurrence(a, "e1", self.entity), occurrence(a, "m2", self.method)]),
            canonical_shape(b, [occurrence(b, "e8", self.entity), occurrence(b, "m9", self.method)]),
        )

    def test_every_reference_kind_rebinds(self):
        source = "x: v5 = e1.m2(p3=row.p4).r6\n"
        kinds = [("v5", "value", "Money"), ("e1", "entity", "Account"),
                 ("m2", "method", "pay"), ("p3", "parameter", "amount"),
                 ("p4", "field", "balance"), ("r6", "relation", "receipt")]
        ledger = [occurrence(source, token, SemanticRef(kind, "c", "Owner", member))
                  for token, kind, member in kinds]
        result = rewrite(source, ledger, {item.ref: "s" + str(i) for i, item in enumerate(ledger)})
        self.assertEqual(result, "x: s0 = s1.s2(s3=row.s4).s5\n")

    def test_strings_comments_and_utf8_untouched(self):
        source = '# é e1\nx = "e1"\ny = e1\n'
        result = rewrite(source, [occurrence(source, "e1", self.entity)], {self.entity: "e99"})
        self.assertEqual(result, '# é e1\nx = "e1"\ny = e99\n')

    def test_unresolved_and_invalid_ranges_rejected(self):
        source = "x = e1"
        item = occurrence(source, "e1", self.entity)
        with self.assertRaises(KeyError):
            rewrite(source, [item], {})
        with self.assertRaises(ValueError):
            rewrite(source, [Occurrence(item.start, item.end - 1, "e", self.entity)], {self.entity: "e2"})
        with self.assertRaises(ValueError):
            rewrite(source, [item, item], {self.entity: "e2"})

    def test_authored_literals_are_preserved_without_template_slots(self):
        a, b = 'pay("Karen", 25)', 'pay("Dave", 90)'
        self.assertEqual(rewrite(a, [], {}), a)
        self.assertNotEqual(canonical_shape(a, []), canonical_shape(b, []))
        facets = frozenset({"pay", "person", "outgoing"})
        examples = [Candidate("a", facets, frozenset({"payments"}), "v1"),
                    Candidate("b", facets, frozenset({"payments"}), "v1")]
        self.assertEqual(len(retrieve(facets, {"payments"}, examples)), 2)

    def test_guards_and_currency_literals_remain_significant(self):
        self.assertNotEqual(canonical_shape('pay(25) if balance > 30 else None', []),
                            canonical_shape('pay(25) if balance > 20 else None', []))
        self.assertNotEqual(canonical_shape('pay("USD")', []), canonical_shape('pay("GBP")', []))

    def test_equal_literals_do_not_create_parameter_relationships(self):
        source = 'f("a", "a")'
        self.assertEqual(rewrite(source, [], {}), source)
        self.assertNotEqual(canonical_shape(source, []), canonical_shape('f("a", "b")', []))

    def test_catalogue_variants_are_distinct(self):
        source = "e1"
        other = SemanticRef("entity", "other", "", "Account")
        self.assertNotEqual(canonical_shape(source, [occurrence(source, "e1", self.entity)]),
                            canonical_shape(source, [occurrence(source, "e1", other)]))

    def test_authorized_catalogue_retrieval_and_limit(self):
        facets = frozenset({"pay", "person", "outgoing"})
        candidates = [Candidate("a", facets, frozenset({"venmo"}), "v1"),
                      Candidate("b", facets, frozenset({"bank"}), "v2")]
        self.assertEqual(retrieve(facets, frozenset(), candidates), [])
        self.assertEqual([x.name for x in retrieve(facets, frozenset({"venmo", "bank"}), candidates,
                                                 requested_catalog="venmo")], ["a"])
        self.assertEqual(len(retrieve(facets, frozenset({"venmo", "bank"}), candidates, limit=1)), 1)


if __name__ == "__main__":
    unittest.main()
