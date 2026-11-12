#!/usr/bin/env python3
"""Dependency-free offline model; SQLite verifies durability, not runtime design."""
from dataclasses import asdict, dataclass
import hashlib
import json
import sqlite3
import tempfile
import threading
import unittest


class Conflict(Exception):
    pass


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


@dataclass(frozen=True)
class Evidence:
    tenant: str = "tenant"
    project: str = "project"
    session: str = "session"
    revision: int = 1
    intent: str = "transfer funds"
    source: str = "source"
    plan: str = "typed-plan"
    catalogs: tuple = ("catalog-contract",)
    receipts: tuple = ("run-1",)
    subtasks: tuple = ("transfer",)
    complete: bool = True
    unknown_effects: bool = False
    artifacts_present: bool = True
    postcondition_verified: bool = False

    def seal(self):
        if (not self.complete or self.unknown_effects or not self.artifacts_present
                or not self.receipts or not self.subtasks or not self.catalogs):
            raise ValueError("incomplete review evidence")
        body = canonical(asdict(self))
        return body, hashlib.sha256(body.encode()).hexdigest()


@dataclass(frozen=True)
class Example:
    identity: str
    behavior_fingerprint: str = "behavior"
    coverage: tuple = ("transfer",)
    assurance: int = 0  # 0 agent-attested; 1 registered deterministic postcondition
    robustness: int = 0
    unnecessary_calls: int = 0
    teaching_tokens: int = 50
    readability: int = 0
    verified: bool = True
    semantic_match: bool = True
    rewrite: bool = False
    preservation_witness: bool = False
    fresh_success: bool = False

    def quality(self):
        return (self.assurance, self.robustness, -self.unnecessary_calls,
                -self.teaching_tokens, self.readability)


class Ledger:
    def __init__(self, path, cap=1000):
        self.db = sqlite3.connect(path)
        self.cap = cap
        self.db.executescript("""
          CREATE TABLE IF NOT EXISTS reviews (
            id INTEGER PRIMARY KEY, tenant TEXT, project TEXT, session TEXT,
            revision INTEGER, body TEXT, digest TEXT, state TEXT DEFAULT 'queued',
            fence INTEGER DEFAULT 0, expires INTEGER DEFAULT 0,
            UNIQUE(tenant, project, session, revision));
          CREATE TABLE IF NOT EXISTS portfolios (scope TEXT PRIMARY KEY, revision INTEGER);
          CREATE TABLE IF NOT EXISTS examples (
            id INTEGER PRIMARY KEY, tenant TEXT, scope TEXT, identity TEXT,
            body TEXT, state TEXT, reason TEXT DEFAULT '');
          CREATE TABLE IF NOT EXISTS support (
            example_id INTEGER, review_id INTEGER, UNIQUE(example_id, review_id));
        """)

    def close(self):
        self.db.close()

    def review(self, evidence):
        body, digest = evidence.seal()
        key = (evidence.tenant, evidence.project, evidence.session, evidence.revision)
        with self.db:
            self.db.execute("BEGIN IMMEDIATE")
            found = self.db.execute(
                "SELECT id,digest FROM reviews WHERE tenant=? AND project=? AND session=? AND revision=?", key
            ).fetchone()
            if found:
                if found[1] != digest:
                    raise Conflict("review revision already sealed")
                return found[0]
            return self.db.execute(
                "INSERT INTO reviews(tenant,project,session,revision,body,digest) VALUES(?,?,?,?,?,?)",
                key + (body, digest)).lastrowid

    def claim(self, review, now, duration=10):
        with self.db:
            changed = self.db.execute("""UPDATE reviews SET state='leased',fence=fence+1,expires=?
                WHERE id=? AND (state='queued' OR (state='leased' AND expires<=?))""",
                (now + duration, review, now)).rowcount
            if not changed:
                raise Conflict("job is not claimable")
            return self.db.execute("SELECT fence FROM reviews WHERE id=?", (review,)).fetchone()[0]

    @staticmethod
    def scope(tenant="tenant", project="project", family="transfer", variant="catalog"):
        return canonical([tenant, project, family, variant])

    def revision(self, scope):
        row = self.db.execute("SELECT revision FROM portfolios WHERE scope=?", (scope,)).fetchone()
        return row[0] if row else 0

    def rows(self, state=None):
        sql = "SELECT id,identity,state FROM examples"
        return self.db.execute(sql + (" WHERE state=?" if state else ""),
                               (state,) if state else ()).fetchall()

    def promote(self, review, fence, now, scope, expected, example, fail=False):
        """One transaction models job completion, CAS, cap check and replacement."""
        with self.db:
            # SQLite's database write lock models the Postgres tenant-scope lock.
            # Acquire before reading caps/revisions, including across families.
            self.db.execute("BEGIN IMMEDIATE")
            job = self.db.execute("SELECT tenant,project,state,fence,expires FROM reviews WHERE id=?",
                                  (review,)).fetchone()
            if not job or job[2] != "leased" or job[3] != fence or job[4] <= now:
                raise Conflict("expired or fenced worker")
            tenant, project, _, _ = json.loads(scope)
            if (tenant, project) != job[:2]:
                raise Conflict("scope mismatch")
            if self.revision(scope) != expected:
                raise Conflict("stale portfolio")
            active = self.db.execute("SELECT id,identity,body FROM examples WHERE scope=? AND state='active'",
                                     (scope,)).fetchall()
            duplicate = self.db.execute("SELECT id,identity,body FROM examples WHERE scope=? AND identity=? ORDER BY id LIMIT 1",
                                        (scope, example.identity)).fetchone()
            state = "candidate"
            if duplicate:
                example_id = duplicate[0]
            else:
                eligible = (example.verified and example.semantic_match
                            and (not example.rewrite or example.preservation_witness or example.fresh_success))
                equivalents = [(r[0], Example(**json.loads(r[2]))) for r in active
                               if tuple(json.loads(r[2])["coverage"]) == tuple(example.coverage)]
                victims = [(i, e) for i, e in equivalents
                           if example.behavior_fingerprint == e.behavior_fingerprint
                           and example.quality() > e.quality()]
                victim = min(victims, key=lambda pair: pair[1].quality())[0] if victims else None
                count = self.db.execute("SELECT count(*) FROM examples WHERE tenant=? AND state='active'",
                                        (tenant,)).fetchone()[0]
                if eligible and victim is not None:
                    self.db.execute("UPDATE examples SET state='superseded' WHERE id=?", (victim,))
                    state = "active"
                elif eligible and len(active) < 3 and count < self.cap and not equivalents:
                    state = "active"
                example_id = self.db.execute(
                    "INSERT INTO examples(tenant,scope,identity,body,state) VALUES(?,?,?,?,?)",
                    (tenant, scope, example.identity, canonical(asdict(example)), state)).lastrowid
            self.db.execute("INSERT OR IGNORE INTO support VALUES(?,?)", (example_id, review))
            self.db.execute("INSERT INTO portfolios VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET revision=excluded.revision",
                            (scope, expected + 1))
            self.db.execute("UPDATE reviews SET state='completed' WHERE id=?", (review,))
            if fail:
                raise RuntimeError("injected crash before commit")
            return example_id

    def quarantine(self, example_id, reason):
        with self.db:
            self.db.execute("BEGIN IMMEDIATE")
            scope = self.db.execute("SELECT scope FROM examples WHERE id=?", (example_id,)).fetchone()[0]
            self.db.execute("UPDATE examples SET state='bad',reason=? WHERE id=?", (reason, example_id))
            self.db.execute("UPDATE portfolios SET revision=revision+1 WHERE scope=?", (scope,))

    def fail(self, review, fence, now, retryable):
        with self.db:
            changed = self.db.execute("""UPDATE reviews SET state=? WHERE id=?
                AND state='leased' AND fence=? AND expires>?""",
                ("queued" if retryable else "failed", review, fence, now)).rowcount
            if changed != 1:
                raise Conflict("expired or fenced worker")


class ContractTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.path = self.temp.name + "/ledger.sqlite"
        self.ledger = Ledger(self.path)
        self.sequence = 0

    def tearDown(self):
        self.ledger.close()
        self.temp.cleanup()

    def admit(self, example, scope=None, tenant="tenant", **kwargs):
        self.sequence += 1
        review = self.ledger.review(Evidence(revision=self.sequence, tenant=tenant))
        fence = self.ledger.claim(review, 0)
        scope = scope or self.ledger.scope(tenant=tenant)
        return self.ledger.promote(review, fence, 1, scope, self.ledger.revision(scope), example, **kwargs)

    def test_immutable_seal_survives_restart_and_retry(self):
        evidence = Evidence()
        first = self.ledger.review(evidence)
        self.ledger.close()
        self.ledger = Ledger(self.path)
        self.assertEqual(first, self.ledger.review(evidence))
        with self.assertRaises(Conflict):
            self.ledger.review(Evidence(source="changed"))

    def test_successive_session_revisions_are_distinct(self):
        self.assertNotEqual(self.ledger.review(Evidence()), self.ledger.review(Evidence(revision=2)))

    def test_incomplete_or_unknown_evidence_never_queues(self):
        for changes in ({"complete": False}, {"unknown_effects": True},
                        {"artifacts_present": False}, {"receipts": ()}, {"subtasks": ()}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                self.ledger.review(Evidence(**changes))
        self.assertEqual(0, self.ledger.db.execute("SELECT count(*) FROM reviews").fetchone()[0])

    def test_assurance_is_recorded_without_promoting_attestation(self):
        for revision, verified in enumerate((False, True), 1):
            job = self.ledger.review(Evidence(revision=revision, postcondition_verified=verified))
            body = self.ledger.db.execute("SELECT body FROM reviews WHERE id=?", (job,)).fetchone()[0]
            self.assertEqual(verified, json.loads(body)["postcondition_verified"])

    def test_expired_worker_cannot_commit_after_reclaim(self):
        job = self.ledger.review(Evidence())
        old = self.ledger.claim(job, 0)
        new = self.ledger.claim(job, 11)
        self.assertGreater(new, old)
        with self.assertRaises(Conflict):
            self.ledger.promote(job, old, 12, self.ledger.scope(), 0, Example("one"))

    def test_completed_job_cannot_replay(self):
        self.admit(Example("one"))
        with self.assertRaises(Conflict):
            self.ledger.claim(1, 20)

    def test_retryable_failure_requeues_terminal_failure_stays_failed(self):
        job = self.ledger.review(Evidence())
        fence = self.ledger.claim(job, 0)
        self.ledger.fail(job, fence, 1, retryable=True)
        new_fence = self.ledger.claim(job, 2)
        self.assertGreater(new_fence, fence)
        self.ledger.fail(job, new_fence, 3, retryable=False)
        with self.assertRaises(Conflict):
            self.ledger.claim(job, 4)

    def test_expired_worker_cannot_complete_without_reclaim(self):
        job = self.ledger.review(Evidence())
        fence = self.ledger.claim(job, 0)
        with self.assertRaises(Conflict):
            self.ledger.promote(job, fence, 10, self.ledger.scope(), 0, Example("one"))

    def test_crash_rolls_back_replacement_and_completion(self):
        self.admit(Example("one"))
        with self.assertRaises(RuntimeError):
            self.admit(Example("better", robustness=1), fail=True)
        self.assertEqual(["one"], [r[1] for r in self.ledger.rows("active")])
        self.assertEqual("leased", self.ledger.db.execute("SELECT state FROM reviews WHERE id=2").fetchone()[0])

    def test_stale_family_decision_conflicts(self):
        self.admit(Example("one"))
        job = self.ledger.review(Evidence(revision=2))
        fence = self.ledger.claim(job, 0)
        with self.assertRaises(Conflict):
            self.ledger.promote(job, fence, 1, self.ledger.scope(), 0, Example("two"))

    def test_exact_duplicate_adds_support_only(self):
        self.admit(Example("transfer(recipient,amount)"))
        self.admit(Example("transfer(recipient,amount)"))
        self.assertEqual(1, len(self.ledger.rows()))
        self.assertEqual(2, self.ledger.db.execute("SELECT count(*) FROM support").fetchone()[0])

    def test_family_cap_and_uncertain_match(self):
        for name in ("a", "b", "c", "d"):
            self.admit(Example(name, coverage=(name,)))
        self.admit(Example("uncertain", semantic_match=False, assurance=1))
        self.assertEqual(3, len(self.ledger.rows("active")))
        self.assertEqual(2, len(self.ledger.rows("candidate")))

    def test_quality_cannot_replace_different_coverage(self):
        for name in ("a", "b", "c"):
            self.admit(Example(name, coverage=(name,)))
        self.admit(Example("narrow", assurance=1))
        self.assertEqual(0, len(self.ledger.rows("superseded")))

    def test_tenant_cap_spans_projects_and_families(self):
        self.ledger.cap = 1
        self.admit(Example("one"))
        self.admit(Example("other"), scope=self.ledger.scope(family="other"))
        self.admit(Example("better", robustness=1))
        self.admit(Example("separate"), tenant="separate")
        self.assertEqual(2, len(self.ledger.rows("active")))
        self.assertEqual(1, len(self.ledger.rows("superseded")))

    def test_concurrent_different_families_share_tenant_cap(self):
        jobs = [self.ledger.review(Evidence(revision=i)) for i in (1, 2)]
        fences = [self.ledger.claim(job, 0) for job in jobs]
        start = threading.Barrier(2)
        errors = []

        def contender(index):
            ledger = Ledger(self.path, cap=1)
            try:
                start.wait(timeout=5)
                ledger.promote(jobs[index], fences[index], 1,
                               ledger.scope(family=str(index)), 0, Example(str(index)))
            except Exception as error:
                errors.append(error)
            finally:
                ledger.close()

        workers = [threading.Thread(target=contender, args=(i,)) for i in range(2)]
        for worker in workers:
            worker.start()
        for worker in workers:
            worker.join(timeout=10)
        self.assertFalse(any(worker.is_alive() for worker in workers))
        self.assertEqual([], errors)
        self.assertEqual(1, len(self.ledger.rows("active")))
        self.assertEqual(1, len(self.ledger.rows("candidate")))

    def test_scope_cannot_cross_tenant(self):
        with self.assertRaises(Conflict):
            self.admit(Example("one"), scope=self.ledger.scope(tenant="other"))

    def test_bad_revision_is_retained_and_invalidates_cas(self):
        example = self.admit(Example("one"))
        self.ledger.quarantine(example, "catalog signature changed")
        self.assertEqual([(example, "one", "bad")], self.ledger.rows())
        self.assertEqual(2, self.ledger.revision(self.ledger.scope()))

    def test_rewrite_requires_preservation_or_fresh_success(self):
        self.admit(Example("unsupported", rewrite=True))
        self.admit(Example("proved", rewrite=True, preservation_witness=True))
        self.admit(Example("observed", coverage=("different-subtask",), rewrite=True, fresh_success=True))
        self.assertEqual(["unsupported"], [r[1] for r in self.ledger.rows("candidate")])
        self.assertEqual(2, len(self.ledger.rows("active")))

    def test_assurance_precedes_cost_and_readability(self):
        self.admit(Example("attested", teaching_tokens=1, readability=100))
        self.admit(Example("verified", assurance=1, teaching_tokens=100))
        self.assertEqual(["verified"], [r[1] for r in self.ledger.rows("active")])

    def test_inferior_same_coverage_stays_candidate_and_deduplicates(self):
        self.admit(Example("one"))
        self.admit(Example("worse", teaching_tokens=500))
        self.admit(Example("worse", teaching_tokens=500))
        self.assertEqual(1, len(self.ledger.rows("active")))
        self.assertEqual(1, len(self.ledger.rows("candidate")))

    def test_bad_identity_never_resurrects(self):
        identity = self.admit(Example("one"))
        self.ledger.quarantine(identity, "drift")
        self.admit(Example("one"))
        self.assertEqual([(identity, "one", "bad")], self.ledger.rows())

    def test_better_score_cannot_replace_different_behavior(self):
        self.admit(Example("guarded", behavior_fingerprint="guarded-transfer"))
        self.admit(Example("unguarded", behavior_fingerprint="unguarded-transfer", robustness=100))
        self.assertEqual(["guarded"], [r[1] for r in self.ledger.rows("active")])


if __name__ == "__main__":
    unittest.main(verbosity=2)
