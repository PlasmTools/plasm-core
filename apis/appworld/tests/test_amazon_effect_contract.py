"""Offline catalog/provider contract checks against installed AppWorld source.

Run inside the pinned AppWorld worker with APPWORLD_SOURCE_ROOT=/opt/appworld/src.
These are intentional provider integration checks, not language matrix tests.
"""
import ast
import os
from pathlib import Path
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[1]


class AmazonEffects(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if not os.environ.get("APPWORLD_SOURCE_ROOT"):
            raise unittest.SkipTest("requires pinned AppWorld provider source")
        cls.catalog = yaml.safe_load((ROOT / "amazon/domain.yaml").read_text())
        cls.mapping = yaml.safe_load((ROOT / "amazon/mappings.yaml").read_text())
        source = Path(os.environ["APPWORLD_SOURCE_ROOT"]) / "appworld/apps/amazon/apis.py"
        text = source.read_text()
        cls.declarations = {node.name: node for node in ast.parse(text).body
                            if isinstance(node, ast.FunctionDef)}
        cls.functions = {
            node.name: ast.get_source_segment(text, node)
            for node in ast.parse(text).body if isinstance(node, ast.FunctionDef)
        }

    def test_cart_preservation_capabilities_match_provider_routes(self):
        pairs = {
            "cart_get": "show_cart",
            "cart_add_product": "add_product_to_cart",
            "cart_remove_product": "delete_product_from_cart",
            "cart_apply_promo": "apply_promo_code_to_cart",
            "cart_remove_promo": "remove_promo_code_from_cart",
            "cart_add_gift_wrap": "add_gift_wrapping_to_product",
            "order_place": "place_order",
            "order_get": "show_order",
        }
        for capability, function in pairs.items():
            with self.subTest(capability=capability):
                self.assertIn(capability, self.catalog["capabilities"])
                mapping = self.mapping[capability]
                declaration = self.declarations[function]
                route = next(d for d in declaration.decorator_list
                             if isinstance(d, ast.Call)
                             and isinstance(d.func, ast.Attribute)
                             and isinstance(d.func.value, ast.Name)
                             and d.func.value.id == "app")
                self.assertEqual(mapping["method"].lower(), route.func.attr)
                provider_path = ["amazon"] + route.args[0].value.strip("/").split("/")
                mapped_path = [p["value"] if p["type"] == "literal" else "{}"
                               for p in mapping["path"]]
                normalized = ["{}" if p.startswith("{") else p for p in provider_path]
                self.assertEqual(mapped_path, normalized)
                arguments = {a.arg for a in declaration.args.args}
                fields = mapping.get("body", {}).get("fields", [])
                self.assertTrue({name for name, _ in fields} <= arguments)

    def test_order_effects_match_provider_and_preserve_output(self):
        source = self.functions["place_order"]
        cap = self.catalog["capabilities"]["order_place"]
        description = cap["description"]
        # Provider evidence and corresponding public contract facts.
        for implementation, description_fact in [
            ("cart_entries = user.cart_entries", "all products and quantities"),
            ("request_payment_card_debit(", "Charges the card"),
            ("TAX_PERCENTAGE", "tax"),
            ("gift_wrap_fee", "gift wrapping"),
            ("delivery_fee", "delivery"),
            ("discount", "discounts"),
            ("product.inventory_quantity -= ordered_quantity", "reduces product"),
            ("Order.create_save(", "creates the order"),
            ('user.delete_all("cart_entries")', "clears every cart line"),
            ("user.cart_promo_code = None", "cart promo code"),
            ("amazon_notifier.notify_on_email(", "confirmation email"),
            ('"content": attachment_file_content', "receipt"),
        ]:
            with self.subTest(fact=description_fact):
                self.assertIn(implementation, source)
                self.assertIn(description_fact, description)
        self.assertIn("Does not select a subset", description)
        self.assertEqual(cap["kind"], "create")
        self.assertEqual(cap["provides"], ["order_id"])
        self.assertEqual(
            [field[0] for field in self.mapping["order_place"]["body"]["fields"]],
            ["payment_card_id", "address_id"],
        )

    def test_add_and_clear_effects_match_provider(self):
        add = self.functions["add_product_to_cart"]
        self.assertIn("cart_entry.quantity += quantity", add)
        self.assertIn('if clear_cart_first:\n        user.delete_all("cart_entries")', add)
        cap = self.catalog["capabilities"]["cart_add_product"]
        self.assertIn("increasing any existing quantity", cap["description"])
        self.assertIn("clear_cart_first", cap["description"])
        self.assertIn("clear_cart_first", cap["output"]["description"])
        clear = self.functions["clear_cart"]
        self.assertIn('user.delete_all("cart_entries")', clear)
        self.assertNotIn("Order.create", clear)
        self.assertNotIn("WishListEntry", clear)
        self.assertIn("Does not place an order", self.catalog["capabilities"]["cart_clear"]["output"]["description"])


if __name__ == "__main__":
    unittest.main()
