"""Offline authorization/disclosure contract; not a Rust compiler or model emulator."""
from dataclasses import dataclass
import unittest


@dataclass(frozen=True)
class Observation:
    scope: tuple
    source: str
    dependencies: frozenset
    contracts: tuple
    reviewed_success: bool
    known_sensitive: bool = False


def curate(observation, family_confirmed):
    if not observation.reviewed_success or not family_confirmed:
        return None, None
    guidance = (observation.scope, observation.dependencies, observation.contracts)
    teaching = None if observation.known_sensitive else observation.source
    return guidance, teaching


def recall(guidance, scope, allowed, contracts):
    if guidance is None:
        return None
    owner, dependencies, pinned = guidance
    if owner != scope or not dependencies <= allowed or pinned != contracts:
        return None
    return dependencies


class CatalogueGuidanceContract(unittest.TestCase):
    def observation(self, **changes):
        return Observation(**dict(scope=("tenant", "project"), source='e1.get("observed-id")',
            dependencies=frozenset({"catalog.get", "auth.login"}), contracts=("contract-a",),
            reviewed_success=True, **changes))

    def test_unannotated_literals_remain_authored_teaching(self):
        observation = self.observation()
        guidance, teaching = curate(observation, True)
        self.assertEqual(teaching, observation.source)
        self.assertIsNotNone(guidance)

    def test_sensitive_teaching_withheld_without_losing_reviewed_guidance(self):
        observation = self.observation(known_sensitive=True)
        guidance, teaching = curate(observation, True)
        self.assertIsNone(teaching)
        self.assertEqual(recall(guidance, observation.scope, observation.dependencies,
                                observation.contracts), observation.dependencies)
        self.assertNotIn(observation.source, repr(guidance))

    def test_guidance_cannot_bypass_scope_authorization_or_contracts(self):
        observation = self.observation()
        guidance, _ = curate(observation, True)
        for scope, allowed, contracts in [
            (("other", "project"), observation.dependencies, observation.contracts),
            (("tenant", "other"), observation.dependencies, observation.contracts),
            (observation.scope, {"catalog.get"}, observation.contracts),
            (observation.scope, observation.dependencies, ("changed-contract",)),
        ]:
            self.assertIsNone(recall(guidance, scope, allowed, contracts))

    def test_uncertain_interpretation_does_not_publish_guidance(self):
        self.assertEqual(curate(self.observation(), False), (None, None))


if __name__ == "__main__":
    unittest.main()
