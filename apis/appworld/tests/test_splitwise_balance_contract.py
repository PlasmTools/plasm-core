"""Catalog integration guard; language/concurrency behavior lives in abstract matrices."""
import json
from pathlib import Path
import unittest
import yaml

ROOT = Path(__file__).resolve().parents[1] / "splitwise"


class GroupBalanceContract(unittest.TestCase):
    def test_parent_identity_observes_the_complete_group(self):
        schema = yaml.safe_load((ROOT / "domain.yaml").read_text())
        mapping = yaml.safe_load((ROOT / "mappings.yaml").read_text())
        operation = schema["capabilities"]["group_balance_get"]
        self.assertEqual(operation["scope"][0]["name"], "group_id")
        self.assertNotIn("email", [f["name"] for f in operation["arguments"]["input_type"]["fields"]])
        self.assertEqual([k for k, _ in mapping["group_balance_get"]["query"]["fields"]], ["group_id"])
        relation = schema["entities"]["GroupBalance"]["relations"]["participants"]
        self.assertEqual(relation["materialize"]["collection_coverage"], "complete")
        spec = json.loads((ROOT / "openapi.json").read_text())
        parameters = spec["paths"]["/splitwise/balance/group"]["get"]["parameters"]
        email = next(p for p in parameters if p["name"] == "email")
        self.assertFalse(email["required"])
        self.assertIn("Otherwise, balance of all members", email["description"])


if __name__ == "__main__":
    unittest.main()
