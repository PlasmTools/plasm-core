//! Python conformance programs with explicit planning, value and failure assertions.
use super::*;
use plasm_agent::plasm_compile::compile_python_program;
use plasm_agent::plasm_plan_run::{run_plasm_comp, PlasmPlanRunResult};
use plasm_core::symbol_tuning::SymbolRender;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) enum PythonOutcome {
    Equivalent,
    CompileError(&'static str),
    LiveError(&'static str),
    ExplicitQualification,
}
pub(super) struct Case {
    pub(super) id: &'static str,
    pub(super) python: &'static str,
    pub(super) existing: Option<&'static str>,
    pub(super) expect_live_error: Option<&'static str>,
}
pub(super) const CASES: &[Case] = &[
    Case { id: "callback_iteration", python: "def step(row):\n    return row.PING()\ndef done(row):\n    return row.title\nreturn E.get('i1').iterate(step, until=done, max_steps=1).select('id')", existing: None, expect_live_error: None },
    Case { id: "callback_projection", python: "def key(row):\n    value = row.id\n    return value\nreturn E.get('i1').select(id=key)", existing: None, expect_live_error: None },
    Case { id: "callback_lexical_capture", python: "value = 'i1'\ndef key(row):\n    return {'id': value}\ndef outer(row):\n    value = 'wrong'\n    return E.get('i1').map(key, max_parents=1)\nreturn E.get('i1').flat_map(outer)", existing: None, expect_live_error: None },
    Case { id: "callback_branch_record", python: "def project(row):\n    if row.id == 'i1':\n        result = {'id': row.id}\n    else:\n        result = {'id': row.title}\n    return result\nreturn E.get('i1').map(project, max_parents=1)", existing: None, expect_live_error: None },
    Case { id: "callback_branch_predicate", python: "def choose(row):\n    if row.id == 'i1':\n        return row.title\n    return None\nreturn E.get('i1').where(choose).select('id')", existing: None, expect_live_error: None },
    Case { id: "callback_lexical_record", python: "def project(row):\n    identifier = row.id\n    return {'id': identifier}\nreturn E.get('i1').map(project, max_parents=1)", existing: None, expect_live_error: None },
    Case { id: "predicate_truth_refinement", python: "return E.get(\"i1\").where(lambda r: r.score is not None).select(value=lambda r: r.score + 1)", existing: None, expect_live_error: None },
    Case { id: "repair_runtime_projection", python: "return E.get(\"i1\").select(value=lambda r: 1 // (len(r.title) - len(r.title)))", existing: None, expect_live_error: Some("division by zero") },
    Case { id: "repair_runtime_compute", python: "class Repair(Program):\n    @compute\n    def fail(self, row: Row) -> str:\n        return str(1 // (len(row.id) - len(row.id)))\n    def build(self):\n        return self.fail(E.get(\"i1\"))\n", existing: None, expect_live_error: Some("division by zero") },
    Case { id: "predicate_truth_string", python: "return E.get(\"i1\").where(lambda r: r.title).select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_truth_and", python: "return E.get(\"i1\").where(lambda r: r.title and 3).select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_truth_empty", python: "return E.get(\"i1\").where(lambda r: \"\").select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_truth_null", python: "return E.get(\"i1\").where(lambda r: None).select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_truth_iteration", python: "return E.get(\"i1\").iterate(lambda r: r.PING(), until=lambda r: r.title, max_steps=1).select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_truth_any", python: "item = E.get(\"i1\")\nreturn {\"value\": any(child.note for child in item.REL_COMPLETE_LINES)}", existing: None, expect_live_error: None },
    Case { id: "predicate_null_test", python: "item = E.get(\"i1\")\nreturn {\"missing\": item.score is None, \"present\": item.score is not None, \"null\": None is None}", existing: None, expect_live_error: None },
    Case { id: "predicate_null_filter", python: "return E.get(\"i1\").where(lambda r: r.score is not None).select(\"id\")", existing: None, expect_live_error: None },

    Case { id: "predicate_scalar_capture", python: "item = E.get(\"i1\")\nthreshold = item.score\nreturn item.map(lambda row: {\"value\": any(threshold is not None and value <= threshold for value in [1, 2])}, max_parents=1)", existing: None, expect_live_error: None },
    Case { id: "predicate_projection_relation", python: "item = E.get(\"i1\")\nreturn item.select(value=lambda row: row.score is not None and row.score > 0 and any(child.note == \"line-a\" for child in row.REL_COMPLETE_LINES))", existing: None, expect_live_error: None },
    Case { id: "predicate_map_relation", python: "item = E.get(\"i1\")\nreturn item.map(lambda row: {\"value\": row.score is not None and row.score > 0 and any(child.note == \"line-a\" for child in row.REL_COMPLETE_LINES)}, max_parents=1)", existing: None, expect_live_error: None },
    Case { id: "predicate_filter_relation", python: "item = E.get(\"i1\")\nreturn item.where(lambda row: row.score is not None and row.score > 0 and any(child.note == \"line-a\" for child in row.REL_COMPLETE_LINES)).select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_iteration_relation", python: "item = E.get(\"i1\")\nreturn item.iterate(lambda row: row.PING(), until=lambda row: row.score is not None and row.score > 0 and any(child.note == \"line-a\" for child in row.REL_COMPLETE_LINES), max_steps=1).select(\"id\")", existing: None, expect_live_error: None },
    Case { id: "predicate_any_relation", python: "item = E.get(\"i1\")\nreturn {\"value\": any(child.note == \"line-a\" for child in item.REL_COMPLETE_LINES)}", existing: None, expect_live_error: None },
    Case { id: "predicate_all_relation", python: "item = E.get(\"i1\")\nreturn {\"value\": all(child.note != \"missing\" for child in item.REL_COMPLETE_LINES)}", existing: None, expect_live_error: None },

Case { id: "value_closure_scalar_method_format", python: "name = \"hello\"\nreturn f\"{name.upper()}\"", existing: None, expect_live_error: None },
Case { id: "compute_inferred_callsite_inputs", python: "class Inferred(Program):\n    @compute\n    def titles(self, rows):\n        return [row.title for row in rows]\n    @compute\n    def first(self, row):\n        return row.title\n    def build(self):\n        return {'titles': self.titles(E.query().take(2)), 'first': self.first(E.get('i1'))}\n", existing: None, expect_live_error: None },
Case { id: "value_closure_nullable_collection", python: "class Closure(Program):\n    @compute\n    def calc(self, value: list[Row.score] | None) -> int:\n        return 0 if value is None else sum(item or 0 for item in value)\n    def build(self):\n        return self.calc(E.query().take(2).select(\"score\"))\n", existing: None, expect_live_error: None },
Case { id: "value_closure_nested_projection", python: "record = E.query().take(2).select(header=lambda row: {\"n\": row.score})\nreturn record.select(n=lambda row: (row.header.n or 0) + 1)", existing: None, expect_live_error: None },
Case { id: "value_closure_nested_read", python: "record = {\"header\": {\"id\": \"i1\"}}\nreturn E.get(record.header.id).select(\"title\")", existing: None, expect_live_error: None },
Case { id: "value_closure_nested_write", python: "record = {\"header\": {\"title\": \"Changed\"}}\nreturn E.get(\"i1\").UPDATE(title=record.header.title, score=2, owner=\"alice\").select(\"title\")", existing: None, expect_live_error: None },
Case { id: "value_closure_nested_format", python: "record = {\"header\": {\"n\": 7}}\nreturn f\"n={record.header.n}\"", existing: None, expect_live_error: None },
Case { id: "value_closure_field_scalar", python: "class Closure(Program):\n    @compute\n    def calc(self, value: int) -> int:\n        return value + 1\n    def build(self):\n        item = E.get(\"i1\")\n        return self.calc((item.score or 0) + 2)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_bool_scalar", python: "class Closure(Program):\n    @compute\n    def calc(self, value: bool) -> bool:\n        return not value\n    def build(self):\n        return self.calc(False)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_nullable_scalar", python: "class Closure(Program):\n    @compute\n    def calc(self, value: int | None) -> int:\n        return 0 if value is None else value\n    def build(self):\n        return self.calc(None)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_empty_collection", python: "class Closure(Program):\n    @compute\n    def calc(self, value: list[Row.score]) -> int:\n        return len(value)\n    def build(self):\n        return self.calc(E.query().where(lambda row: row.id == \"missing\").select(\"score\"))\n", existing: None, expect_live_error: None },
Case { id: "value_closure_structural_singleton", python: "class Closure(Program):\n    @compute\n    def calc(self, value: Row) -> int:\n        return value.n\n    def build(self):\n        return self.calc({\"n\": 7})\n", existing: None, expect_live_error: None },
Case { id: "value_closure_literal", python: "class Closure(Program):\n    @compute\n    def calc(self, value: int) -> int:\n        return value + 1\n    def build(self):\n        value = 42\n        return self.calc(value)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_column", python: "class Closure(Program):\n    @compute\n    def calc(self, value: int | None) -> int:\n        return (value or 0) + 1\n    def build(self):\n        value = E.query().take(2).select(\"score\")\n        return value.map(lambda row: {\"value\": self.calc(row.score)}, max_parents=256)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_empty", python: "class Closure(Program):\n    @compute\n    def calc(self, value: int | None) -> int:\n        return (value or 0) + 1\n    def build(self):\n        value = E.query().where(lambda row: row.id == \"missing\").select(\"score\")\n        return value.map(lambda row: {\"value\": self.calc(row.score)}, max_parents=256)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_collection", python: "class Closure(Program):\n    @compute\n    def calc(self, value: list[Row.score]) -> int:\n        return sum(item or 0 for item in value)\n    def build(self):\n        value = E.query().take(2).select(\"score\")\n        return self.calc(value)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_array", python: "class Closure(Program):\n    @compute\n    def calc(self, value: list[int]) -> int:\n        return sum(value)\n    def build(self):\n        value = [2, 3]\n        return self.calc(value)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_record", python: "class Closure(Program):\n    @compute\n    def calc(self, value: Row) -> int:\n        return value.header.n\n    def build(self):\n        value = {\"header\": {\"n\": 7}}\n        return self.calc(value)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_empty_record", python: "class Closure(Program):\n    @compute\n    def calc(self, value: Row) -> int:\n        return 7\n    def build(self):\n        value = {}\n        return self.calc(value)\n", existing: None, expect_live_error: None },
Case { id: "value_closure_format", python: "return f\"hello {2 + 3}\"", existing: None, expect_live_error: None },
Case { id: "value_closure_nested", python: "record = {\"header\": {\"n\": 7}}\nreturn record.header.n", existing: None, expect_live_error: None },
Case { id: "value_closure_bound_format", python: "n = 7\nreturn f\"n={n}\"", existing: None, expect_live_error: None },
Case { id: "value_recursive_read_argument", python: "key = \"i\"\nn = \"1\"\nreturn E.get(key + n).select(\"title\")", existing: None, expect_live_error: None },
Case { id: "value_recursive_write_argument", python: "item = E.get(\"i1\")\nreturn item.UPDATE(title=item.title + \"!\", score=(item.score or 0) + 2, owner=\"alice\").select(\"title\", \"score\")", existing: None, expect_live_error: None },
Case { id: "value_recursive_bound_write_argument", python: "item = E.get(\"i1\")\ntext = item.title + \"!\"\nscore = (item.score or 0) + 2\nreturn item.UPDATE(title=text, score=score, owner=\"alice\").select(\"title\", \"score\")", existing: None, expect_live_error: None },
Case { id: "value_recursive_scoped_empty", python: "return E.query().take(2).map(lambda row: {}, max_parents=2)", existing: None, expect_live_error: None },
Case { id: "value_recursive_membership", python: "return {\"yes\": \"a\" in \"cat\", \"no\": 2 not in [1, 3]}", existing: None, expect_live_error: None },
Case { id: "value_recursive_bindings", python: "n = 2\nflag = True\nempty = None\nxs = [n, 3]\nalias = n\nreturn {\"n\": alias, \"flag\": flag, \"empty\": empty, \"xs\": xs}", existing: None, expect_live_error: None },
Case { id: "value_recursive_arithmetic", python: "item = E.get(\"i1\")\nreturn {\"n\": (item.score or 0) + 2, \"nested\": [{\"n\": ((item.score or 0) + 1) * 2}], \"negative\": -(item.score or 0)}", existing: None, expect_live_error: None },
Case { id: "value_recursive_conditional", python: "item = E.get(\"i1\")\nreturn {\"n\": item.score if item.score is not None and item.score > 0 and not False else 1 / 0}", existing: None, expect_live_error: None },
Case { id: "value_recursive_inline_field", python: "return {\"title\": E.get(\"i1\").title}", existing: None, expect_live_error: None },
Case { id: "value_recursive_length", python: "xs = [1, 2]\nreturn {\"n\": len([xs, []])}", existing: None, expect_live_error: None },
Case { id: "value_recursive_empty_record", python: "return {}", existing: None, expect_live_error: None },
Case { id: "value_recursive_projection", python: "return E.query().take(2).select(value=lambda row: {\"n\": (row.score or 0) + 1, \"xs\": [row.title, len([row.score, 1])]})", existing: None, expect_live_error: None },
Case { id: "value_recursive_scoped_relation", python: "return E.get(\"i1\").map(lambda row: {\"title\": row.title, \"lines\": row.REL_COMPLETE_LINES}, max_parents=1)", existing: None, expect_live_error: None },
Case { id: "value_recursive_lazy_projection", python: "return E.get(\"i1\").select(value=lambda row: row.score if row.score is not None and row.score > 0 and True else 1 / 0)", existing: None, expect_live_error: None },
Case { id: "value_recursive_root_scalar", python: "return 42", existing: None, expect_live_error: None },

Case { id: "record_value_nested_compute", python: "class Render(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.header.name + str(len(row.items))\n    def build(self):\n        item = E.get(\"i1\")\n        header = {\"name\": item.title}\n        items = E.query().take(2).select(\"title\")\n        record = {\"header\": header, \"items\": items}\n        return self.text(record)\n", existing: None, expect_live_error: None },
Case { id: "record_value_scalar_compute", python: "class Render(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.key + row.label\n    def build(self):\n        item = E.get(\"i1\")\n        key = item.id\n        label = \"constant\"\n        record = {\"key\": key, \"label\": label}\n        return self.text(record)\n", existing: None, expect_live_error: None },

Case { id: "record_value_singleton", python: "item = E.get(\"i1\")\nreturn {\"id\": item.id, \"title\": item.title}", existing: None, expect_live_error: None },
Case { id: "record_value_nested", python: "left = E.get(\"i1\")\nright = E.get(\"i2\")\nreturn {\"left\": {\"name\": left.title}, \"right\": right.title, \"values\": [left.score, right.score, None]}", existing: None, expect_live_error: None },
Case { id: "record_value_constants", python: "return {\"integer\": 9007199254740993, \"signed\": -9223372036854775808, \"ratio\": 1.25, \"flag\": False, \"nil\": None, \"items\": [{\"name\": \"constant\"}]}", existing: None, expect_live_error: None },
Case { id: "record_value_collection", python: "rows = E.query().take(2).select(\"title\")\nreturn {\"items\": rows}", existing: None, expect_live_error: None },
Case { id: "record_value_empty_collection", python: "rows = E.query().where(lambda row: row.id == \"missing\").select(\"title\")\nreturn {\"items\": rows}", existing: None, expect_live_error: None },
Case { id: "record_value_scalar_binding", python: "item = E.get(\"i1\")\nkey = item.id\nlabel = \"constant\"\nreturn {\"key\": key, \"label\": label}", existing: None, expect_live_error: None },
Case { id: "record_value_reuse", python: "item = E.get(\"i1\")\nheader = {\"name\": item.title}\nreturn {\"header\": header, \"copied\": header.name}", existing: None, expect_live_error: None },
Case { id: "record_value_projection", python: "item = E.get(\"i1\")\nrecord = {\"renamed\": item.title, \"count\": item.score}\nreturn record.select(\"renamed\", \"count\")", existing: None, expect_live_error: None },
Case { id: "record_value_compute", python: "class Render(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.name\n    def build(self):\n        item = E.get(\"i1\")\n        record = {\"name\": item.title}\n        return self.text(record)\n", existing: None, expect_live_error: None },
Case { id: "record_value_empty_singleton", python: "item = E.query().where(lambda row: row.id == \"missing\").take(1)\nreturn {\"name\": item.title}", existing: None, expect_live_error: Some("zero rows") },
Case { id: "operand_recursive_literals", python: "return E.get(\"i1\").map(lambda row: {\"text\": \"a\" + \"b\", \"integer\": 9007199254740993, \"signed\": -9223372036854775808, \"ratio\": 1.25, \"flag\": False, \"nil\": None, \"nested\": [{\"text\": \"x\", \"nums\": [1, 2]}]}, max_parents=1)", existing: None, expect_live_error: None },
Case { id: "root_build_statements", python: "class Documented(Program):\n    \"class documentation\"\n    def _identify(self, item):\n        return item.select(\"id\")\n    def build(self):\n        \"workflow documentation\"\n        item = E.get(\"i1\")\n        item.PING()\n        return self._identify(item)\n", existing: None, expect_live_error: None },
Case { id: "boolean_identity_false_keyword", python: "return E.get(identity=False).select(\"id\")", existing: Some("lang_boolean_identity_false"), expect_live_error: None },
Case { id: "boolean_identity_true_keyword", python: "return E.get(identity=True).select(\"id\")", existing: Some("lang_boolean_identity_true"), expect_live_error: None },
Case { id: "bound_get_field_keyword", python: "source = E.get(identity=\"i1\")\nout = E.get(identity=source.id)\nreturn source, out", existing: Some("lang_bound_get_field"), expect_live_error: None },
Case { id: "bound_get_scalar_keyword", python: "source = E.get(identity=\"i1\")\nkey = source.id\nout = E.get(identity=key)\nreturn source, out", existing: Some("lang_bound_get_scalar"), expect_live_error: None },
Case { id: "bound_get_empty_keyword", python: "source = E.query().where(lambda row: row.id == \"missing\").take(1)\nkey = source.id\nout = E.get(identity=key)\nreturn out", existing: Some("lang_bound_get_empty"), expect_live_error: Some("zero rows") },
Case { id: "scoped_flat_map_format", python: "class Nested(Program):\n    def build(self):\n        return E.query().take(2).flat_map(lambda parent: E.query().take(1).map(lambda child: {\"parent\": parent.title, \"label\": f\"{child.title.lower()}:{parent.title.lower()}\"}, max_parents=1))\n", existing: None, expect_live_error: None },
Case { id: "scoped_nested_records", python: "class NestedRecords(Program):\n    def build(self):\n        parents = E.query().take(2)\n        return parents.map(lambda parent: {\"id\": parent.id, \"children\": E.query().take(1).map(lambda child: {\"parent\": parent.title, \"child\": child.title}, max_parents=2)}, max_parents=2)\n", existing: None, expect_live_error: None },
Case { id: "scoped_nested_effects", python: "class NestedEffects(Program):\n    def build(self):\n        parents = E.query().take(2)\n        return parents.map(lambda parent: {\"id\": parent.id, \"children\": E.query().take(1).map(lambda child: {\"child\": child.id, \"sent\": parent.PING()}, max_parents=2)}, max_parents=2)\n", existing: None, expect_live_error: None },
Case { id: "assembly_row_compute", python: "class RowAssembly(Program):\n    @compute\n    def lower_title(self, row: Row) -> str:\n        return row.title.lower()\n    @compute\n    def report(self, row: Row) -> str:\n        return row.original + \"=\" + row.destination\n    def build(self):\n        items = E.query().select(\"id\", \"title\")\n        paired = items.map(lambda row: {\"original\": row.id, \"destination\": self.lower_title(row)}, max_parents=256)\n        return paired.map(lambda row: {\"value\": self.report(row)}, max_parents=256)\n", existing: None, expect_live_error: None },
Case { id: "assembly_singleton_capture", python: "class CapturedAssembly(Program):\n    def build(self):\n        header = E.get(\"i1\")\n        items = E.query()\n        return items.select(\"id\", heading=lambda row: header.title)\n", existing: None, expect_live_error: None },
Case { id: "type_projected_integer", python: "class TypedText(Program):\n    @compute\n    def format_row(self, row: Row) -> str:\n        return str(row.score)\n    def build(self):\n        items = E.get(\"i1\").select(\"score\")\n        report = self.format_row(items)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "type_projected_enum", python: "class TypedText(Program):\n    @compute\n    def format_row(self, row: Row) -> str:\n        return str(row.status)\n    def build(self):\n        items = E.get(\"i1\").select(\"status\")\n        report = self.format_row(items)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "type_projected_temporal", python: "class TypedText(Program):\n    @compute\n    def format_row(self, row: Row) -> str:\n        return str(row.recorded_at)\n    def build(self):\n        items = E.get(\"i1\").select(\"recorded_at\")\n        report = self.format_row(items)\n        return report\n", existing: None, expect_live_error: None },

Case { id: "text_projected_alias", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.renamed.split(\"1\")[0]\n\n    def build(self):\n        items = E.get(\"i1\").select(renamed=\"id\")\n        out = self.text(items)\n        return out\n", existing: Some("lang_render_projected_shape"), expect_live_error: None },
Case { id: "text_derived_alias", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"value={row.renamed}\"\n\n    def build(self):\n        items = E.get(\"i1\")\n        mapped = items.select(renamed=\"id\")\n        out = self.text(mapped)\n        return out\n", existing: Some("lang_render_derived_shape"), expect_live_error: None },
Case { id: "text_bindings_row", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"# {row.title}\"\n\n    def build(self):\n        items = E.get(\"i1\").select(\"id\", \"title\")\n        hdr = self.text(items)\n        return hdr\n", existing: Some("lang_bindings_render"), expect_live_error: None },
Case { id: "text_split_part", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return \"split_part_ok=\" + row.id.split(\"1\")[0]\n\n    def build(self):\n        items = E.get(\"i1\").select(\"id\")\n        hdr = self.text(items)\n        return hdr\n", existing: Some("lang_render_split_part"), expect_live_error: None },
Case { id: "text_conditional_membership", python: "class ConditionalText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.id + (\":matched\" if \"i\" in row.id else \":absent\")\n    def build(self):\n        items = E.get(\"i1\").select(\"id\")\n        report = self.text(items)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "text_row_duplicates", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n\n    def build(self):\n        items = E.query().take(2).select(\"title\")\n        repeated = items.union(items)\n        report = repeated.map(lambda row: {\"value\": self.text(row)}, max_parents=256)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "text_empty_aggregate", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"count={row.n}\"\n\n    def build(self):\n        items = E.query().where(lambda row: row.id == \"missing\")\n        counts = items.aggregate(n=agg.count())\n        report = self.text(counts)\n        return report\n", existing: None, expect_live_error: None },

Case { id: "text_per_row_zero", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"{row.title}\"\n\n    def build(self):\n        items = E.query().where(lambda row: row.id == \"missing\")\n        rendered = items.map(lambda row: {\"value\": self.text(row)}, max_parents=256)\n        return rendered\n", existing: Some("lang_per_row_render_zero"), expect_live_error: None },
Case { id: "text_per_row_many", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"{row.title} — {row.code}\"\n\n    def build(self):\n        items = E.query().take(2).select(\"id\", \"title\", \"code\")\n        rendered = items.map(lambda row: {\"value\": self.text(row)}, max_parents=256)\n        return rendered\n", existing: Some("lang_per_row_render_many"), expect_live_error: None },
Case { id: "text_synthetic_count", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"count={row.n}\"\n\n    def build(self):\n        items = E.query().take(2)\n        counts = items.aggregate(n=agg.count())\n        report = self.text(counts)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "text_synthetic_group", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"{row.title}: {row.n}\"\n\n    def build(self):\n        items = E.query().take(2)\n        counts = items.group_by(\"title\", n=agg.count())\n        report = counts.map(lambda row: {\"value\": self.text(row)}, max_parents=256)\n        return report\n", existing: None, expect_live_error: None },

Case { id: "text_multiline_whitespace", python: "class Whitespace(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return rf\"\"\"Header \\n\n            preserved\nid={row.id}\n尾\n\"\"\"\n\n    def build(self):\n        one = E.get(\"i1\")\n        report = self.text(one.select(\"id\"))\n        return report\n", existing: None, expect_live_error: None },
Case { id: "text_write_reuse", python: "class WriteText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"rendered {row.id}\"\n\n    def build(self):\n        one = E.get(\"i1\")\n        report = self.text(one.select(\"id\"))\n        changed = one.UPDATE(title=report, score=7, owner=\"alice\")\n        return changed.select(\"title\")\n", existing: None, expect_live_error: None },
Case { id: "text_collection_report", python: "class TextReport(Program):\n    @compute\n    def format_rows(self, rows: list[Value[ENTITY]]) -> str:\n        return \"\".join(f\"\\n- {row.title}\\n\" for row in rows)\n\n    def build(self):\n        items = E.query().take(2).select(\"id\", \"title\")\n        report = self.format_rows(items)\n        return report\n", existing: Some("lang_plain_template_foreach"), expect_live_error: None },
Case { id: "text_literal_binding", python: "class LiteralText(Program):\n    def build(self):\n        note = \"\"\"hello-matrix\n\"\"\"\n        one = E.query().take(1).select(\"title\")\n        return one, note\n", existing: Some("lang_heredoc_binding"), expect_live_error: None },
Case { id: "text_literal_equals", python: "class LiteralText(Program):\n    def build(self):\n        body = \"\"\"key = value\n\"\"\"\n        one = E.query().take(1).select(\"title\")\n        return one, body\n", existing: Some("lang_heredoc_body_with_equals"), expect_live_error: None },
Case { id: "reduce_lang_aggregate", python: "return E.query().aggregate(n=agg.count())", existing: Some("lang_aggregate"), expect_live_error: None },
Case { id: "reduce_lang_aggregate_sugar_count", python: "return E.query().aggregate(count=agg.count())", existing: Some("lang_aggregate_sugar_count"), expect_live_error: None },
Case { id: "reduce_lang_aggregate_sum", python: "return E.query().aggregate(t=agg.sum(\"score\"))", existing: Some("lang_aggregate_sum"), expect_live_error: None },
Case { id: "reduce_lang_group_by", python: "return E.query().group_by(\"owner\", n=agg.count())", existing: Some("lang_group_by"), expect_live_error: None },
Case { id: "reduce_lang_group_by_aggregate_chain", python: "return E.query().group_by(\"owner\", \"score\", n=agg.count(), title=agg.first(\"title\"))", existing: Some("lang_group_by_aggregate_chain"), expect_live_error: None },
Case { id: "reduce_lang_group_by_sugar", python: "return E.query().group_by(\"owner\", count=agg.count())", existing: Some("lang_group_by_sugar"), expect_live_error: None },
Case { id: "reduce_lang_group_by_multi", python: "return E.query().group_by(\"owner\", \"score\", n=agg.count())", existing: Some("lang_group_by_multi"), expect_live_error: None },
Case { id: "reduce_lang_group_by_first", python: "return E.query().group_by(\"owner\", title=agg.first(\"title\"))", existing: Some("lang_group_by_first"), expect_live_error: None },
Case { id: "reduce_lang_group_by_then_sort_agg_column", python: "return E.query().group_by(\"owner\", n=agg.count()).order_by(\"n\", descending=True)", existing: Some("lang_group_by_then_sort_agg_column"), expect_live_error: None },
Case { id: "reduce_lang_dedupe", python: "return E.query().distinct(\"owner\").take(20)", existing: Some("lang_dedupe"), expect_live_error: None },
Case { id: "reduce_aggregate_functions", python: "return E.query().aggregate(s=agg.sum(\"score\"), a=agg.avg(\"score\"), lo=agg.min(\"score\"), hi=agg.max(\"score\"), f=agg.first(\"title\"), l=agg.last(\"title\"))", existing: None, expect_live_error: None },
Case { id: "reduce_aggregate_empty", python: "return E.query().where(lambda row: row.id == \"missing\").aggregate(n=agg.count(), total=agg.sum(\"score\"))", existing: None, expect_live_error: None },
Case { id: "reduce_group_empty", python: "return E.query().where(lambda row: row.id == \"missing\").group_by(\"owner\", n=agg.count())", existing: None, expect_live_error: None },
Case { id: "reduce_aggregate_alias", python: "return E.query().select(points=\"score\").aggregate(total=agg.sum(\"points\"))", existing: None, expect_live_error: None },
Case { id: "set_lanes_group_then_global_aggregate", python: "return LANE.query(shelf=\"alpha\").group_by(\"shelf\", n=agg.count()).aggregate(total=agg.sum(\"n\"))", existing: Some("lang_group_then_global_aggregate"), expect_live_error: None },
Case { id: "reduce_distinct_alias", python: "return E.query().select(\"title\", handle=\"owner\").distinct(\"handle\")", existing: None, expect_live_error: None },
Case { id: "reduce_distinct_multi", python: "return E.query().distinct(\"owner\", \"score\")", existing: None, expect_live_error: None },
Case { id: "cert_take_one_field_bind", python: "items = E.query()\none = items.order_by(\"id\").take(1)\nvalue = one.id\nreturn one, value", existing: Some("lang_take_one_field_bind"), expect_live_error: None },
Case { id: "cert_take_one_field_empty_bind", python: "items = E.query()\none = items.where(lambda row: row.id == \"missing\").take(1)\nvalue = one.id\nreturn value", existing: Some("lang_take_one_field_empty_bind"), expect_live_error: Some("zero rows") },
Case { id: "cert_take_one_field_empty_argument", python: "items = E.query()\none = items.where(lambda row: row.id == \"missing\").take(1)\ntarget = E.get(\"i1\")\nout = target.UPDATE(title=one.id, score=42, owner=\"alice\")\nreturn out", existing: Some("lang_take_one_field_empty_argument"), expect_live_error: Some("zero rows") },
Case { id: "cert_take_one_method_invoke_empty", python: "items = E.query()\none = items.where(lambda row: row.id == \"missing\").take(1)\nout = one.UPDATE(title=\"must-not-write\", score=2, owner=\"alice\")\nreturn out", existing: Some("lang_take_one_method_invoke_empty"), expect_live_error: Some("zero rows") },
Case { id: "cert_bind_singleton_field_scalar", python: "item = E.get(\"i1\")\ntitle = item.title\nreturn title", existing: Some("lang_bind_singleton_field_scalar"), expect_live_error: None },
Case { id: "cert_get_singleton_field_scalar", python: "title = E.get(\"i1\").title\nreturn title", existing: Some("lang_get_singleton_field_scalar"), expect_live_error: None },
Case { id: "cert_get_singleton_field_password", python: "pw = VAULT.get(\"venmo\").password\nreturn pw", existing: Some("lang_get_singleton_field_password"), expect_live_error: None },
Case { id: "cert_get_singleton_field_empty", python: "return VAULT.get(\"missing\").password", existing: Some("lang_get_singleton_field_empty"), expect_live_error: Some("zero rows") },
Case { id: "cert_pipe_select_row_fields", python: "root = E.get(\"i1\")\nreturn root.select(\"title\")", existing: Some("lang_pipe_select_row_fields"), expect_live_error: None },
Case { id: "cert_bind_limit1_continuation", python: "root = E.query(owner=\"alice\")\none = root.take(1)\ntags = one.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_bind_limit1_continuation"), expect_live_error: None },
Case { id: "cert_relation_opaque_r_symbol", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.REL_TAGS)\nreturn tags", existing: Some("lang_relation_opaque_r_symbol"), expect_live_error: None },
Case { id: "cert_required_selection_default", python: "return STOCK.query()", existing: Some("lang_required_selection_default"), expect_live_error: None },
Case { id: "cert_required_selection_multi", python: "return LANE.query(shelf=\"alpha\")", existing: Some("lang_required_selection_multi"), expect_live_error: None },
Case { id: "cert_required_selection_empty", python: "return LANE.query(shelf=\"empty\")", existing: Some("lang_required_selection_empty"), expect_live_error: None },
Case { id: "cert_quoted_binding_literal", python: "item = E.get(\"i1\")\nreturn E.query().where(lambda row: row.title == \"item\")", existing: Some("lang_quoted_binding_literal"), expect_live_error: None },
Case { id: "cert_integer_where_gt_dry_coerce", python: "return E.query().where(lambda row: row.score is not None and row.score > 0)", existing: Some("lang_integer_where_gt_dry_coerce"), expect_live_error: None },
Case { id: "cert_ra4_pipe_monolith", python: "return E.query().where(lambda row: row.owner == \"alice\").order_by(\"title\").take(5).select(\"title\", \"owner\")", existing: Some("lang_ra4_pipe_monolith"), expect_live_error: None },
Case { id: "cert_ra4_pipe_bind_cut", python: "h = E.query()\nw = h.where(lambda row: row.owner == \"alice\")\no = w.order_by(\"title\")\nt = o.take(5)\nreturn t.select(\"title\", \"owner\")", existing: Some("lang_ra4_pipe_bind_cut"), expect_live_error: None },
Case { id: "cert_apply_get_multirow", python: "items = E.query().where(lambda row: row.owner == \"alice\").take(3)\ndetails = items.flat_map(lambda row: E.get(row.id))\nreturn details", existing: Some("lang_apply_get_multirow"), expect_live_error: None },
Case { id: "cert_apply_query_multirow", python: "items = E.query().where(lambda row: row.owner == \"alice\").take(2)\npeers = items.flat_map(lambda row: E.query(owner=row.owner))\nreturn peers", existing: Some("lang_apply_query_multirow"), expect_live_error: None },
Case { id: "cert_effect_action_ping", python: "item = E.get(\"i1\")\nreturn item.PING()", existing: Some("lang_effect_action_ping"), expect_live_error: None },
Case { id: "cert_effect_delete", python: "item = E.get(\"i2\")\nreturn item.DELETE()", existing: Some("lang_effect_delete"), expect_live_error: None },
Case { id: "cert_for_each_empty_ping", python: "items = E.query().where(lambda row: row.owner == \"no-such-matrix-owner\")\ndone = items.flat_map(lambda row: row.PING())\nreturn done", existing: Some("lang_for_each_empty_ping"), expect_live_error: None },

Case { id: "inline_membership", python: "kept = E.query().where(lambda row: row.owner in E.query().where(lambda row: row.owner == \"alice\").select(\"owner\"))\nreturn kept", existing: Some("lang_where_in_rowset_paren"), expect_live_error: None },
Case { id: "inline_alias_membership", python: "sent = E.query().where(lambda row: row.owner == \"alice\").select(email=\"owner\")\nrecv = E.query().where(lambda row: row.owner == \"bob\").select(email=\"owner\")\npeers = sent.union(recv).distinct()\nkept = E.query().where(lambda row: row.owner in peers.select(\"email\"))\nreturn kept", existing: Some("lang_union_rowset_alias_distinct"), expect_live_error: None },
Case { id: "set_lanes_inline_not_in_left", python: "left = LANE.query(shelf=\"alpha\")\nright = STOCK.query()\nkept = left.where(lambda row: row.title not in right.select(\"title\"))\nreturn kept", existing: Some("lang_where_not_in_universe_left"), expect_live_error: None },
Case { id: "set_lanes_inline_not_in_right", python: "left = STOCK.query()\nright = LANE.query(shelf=\"alpha\")\nkept = left.where(lambda row: row.title not in right.select(\"title\"))\nreturn kept", existing: Some("lang_where_not_in_universe_right"), expect_live_error: None },

Case { id: "set_lanes_union_empty_right", python: "kept = LANE.query(shelf=\"alpha\").select(\"title\")\nnone = LANE.query(shelf=\"empty\").select(\"title\")\npeers = kept.union(none)\nreturn peers", existing: Some("lang_union_empty_right"), expect_live_error: None },
Case { id: "set_lanes_not_in_universe_left", python: "left = LANE.query(shelf=\"alpha\")\nright = STOCK.query()\ntitles = right.select(\"title\")\nkept = left.where(lambda row: row.title not in titles)\nreturn kept", existing: Some("lang_where_not_in_universe_left"), expect_live_error: None },
Case { id: "set_lanes_not_in_universe_right", python: "left = STOCK.query()\nright = LANE.query(shelf=\"alpha\")\ntitles = right.select(\"title\")\nkept = left.where(lambda row: row.title not in titles)\nreturn kept", existing: Some("lang_where_not_in_universe_right"), expect_live_error: None },
Case { id: "union_collapses_duplicates", python: "one = E.get(\"i1\").select(\"owner\")\nreturn one.union(one)", existing: None, expect_live_error: None },
Case { id: "set_lanes_distinct_projection", python: "rows = LANE.query(shelf=\"alpha\").select(\"shelf\")\nunique = rows.distinct()\nreturn unique", existing: Some("lang_distinct_projected_values"), expect_live_error: None },

Case { id: "where_in_rowset", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nkept = E.query().where(lambda row: row.owner in alice)\nreturn kept", existing: Some("lang_where_in_rowset"), expect_live_error: None },
Case { id: "where_not_in_rowset", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\ndrop = E.query().where(lambda row: row.owner not in alice)\nreturn drop", existing: Some("lang_where_not_in_rowset"), expect_live_error: None },
Case { id: "where_in_rowset_paren", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nkept = E.query().where(lambda row: row.owner in alice)\nreturn kept", existing: Some("lang_where_in_rowset_paren"), expect_live_error: None },
Case { id: "union_rowset", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nbob = E.query().where(lambda row: row.owner == \"bob\").select(\"owner\")\npeers = alice.union(bob)\nkept = E.query().where(lambda row: row.owner in peers)\nreturn kept", existing: Some("lang_union_rowset"), expect_live_error: None },
Case { id: "union_rowset_alias", python: "sent = E.query().where(lambda row: row.owner == \"alice\").select(email=\"owner\")\nrecv = E.query().where(lambda row: row.owner == \"bob\").select(email=\"owner\")\npeers = sent.union(recv)\nkept = E.query().where(lambda row: row.owner in peers)\nreturn kept", existing: Some("lang_union_rowset_alias"), expect_live_error: None },
Case { id: "union_rowset_alias_distinct", python: "sent = E.query().where(lambda row: row.owner == \"alice\").select(email=\"owner\")\nrecv = E.query().where(lambda row: row.owner == \"bob\").select(email=\"owner\")\npeers = sent.union(recv).distinct()\nemails = peers.select(\"email\")\nkept = E.query().where(lambda row: row.owner in emails)\nreturn kept", existing: Some("lang_union_rowset_alias_distinct"), expect_live_error: None },
Case { id: "union_rowset_alias_existing", python: "recv = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nsent = E.query().where(lambda row: row.owner == \"bob\").select(\"title\")\npeers = recv.union(sent.select(owner=\"title\"))\nkept = E.query().where(lambda row: row.owner in peers)\nreturn kept", existing: Some("lang_union_rowset_alias_existing"), expect_live_error: None },

Case { id: "select_alias_where", python: "items = E.query()\nrenamed = items.select(\"owner\", handle=\"owner\").where(lambda row: row.handle == \"alice\")\nreturn renamed", existing: Some("lang_select_alias_where"), expect_live_error: None },
Case { id: "prefix_serial_limit", python: "return E.query().take(5).take(3).take(1).select(\"score\")", existing: None, expect_live_error: None },
Case { id: "prefix_union_single_relation", python: "a = E.get(\"i1\")\nb = E.get(\"i2\")\nreturn a.union(b).take(1).tags.select(\"id\")", existing: None, expect_live_error: None },
Case { id: "prefix_zero_rows", python: "return E.query().take(0)", existing: None, expect_live_error: None },
Case { id: "prefix_zero_extract", python: "return E.query().take(0).title", existing: None, expect_live_error: Some("zero rows") },
Case { id: "prefix_zero_count", python: "return E.query().take(0).aggregate(n=agg.count())", existing: None, expect_live_error: None },
Case { id: "record_literal_index", python: "return E.query().take(1).select(value=lambda r: {\"items\": [r.score, (r.score or 0) + 1]}).select(value=lambda r: r.value[\"items\"][1])", existing: None, expect_live_error: None },
Case { id: "projection_alias_topk_source_contract", python: "return E.query().select(rank='score').order_by('rank').take(2)", existing: None, expect_live_error: None },
Case { id: "alias_reproject_sort", python: "items = E.query()\nrenamed = items.select(\"title\", handle=\"owner\")\nreturn renamed.select(\"handle\").order_by(\"handle\")", existing: None, expect_live_error: None },
Case { id: "alias_replace_column", python: "items = E.query()\nreturn items.select(owner=\"title\")", existing: None, expect_live_error: None },

Case { id: "sort_limit", python: "return E.query().order_by(\"score\", descending=True).take(2).select(\"id\", \"score\")", existing: Some("lang_sort_limit"), expect_live_error: None },
Case { id: "sort_asc", python: "return E.query().order_by(\"score\", descending=False).take(3).select(\"id\", \"score\")", existing: Some("lang_sort_asc"), expect_live_error: None },
Case { id: "program_return_pipeline_filter_sort", python: "items = E.query()\nfiltered = items.where(lambda row: row.owner == \"alice\")\nsorted = filtered.order_by(\"title\").take(10).select(\"title\", \"owner\")\nreturn sorted", existing: Some("lang_program_return_pipeline_filter_sort"), expect_live_error: None },

Case { id: "relation_empty_fanout", python: "items = E.query().where(lambda row: row.id == \"missing\")\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_relation_empty_fanout"), expect_live_error: None },
Case { id: "relation_one_chain", python: "summary = E.get(\"i1\").summary\ndetail = summary.detail\nreturn detail.select(\"id\", \"body\")", existing: Some("lang_relation_one_chain"), expect_live_error: None },
Case { id: "relation_relation_lines", python: "lines = E.get(\"i1\").lines\nreturn lines.select(\"id\", \"note\")", existing: Some("lang_relation_lines"), expect_live_error: None },
Case { id: "relation_relation_tags_scoped", python: "return E.get(\"i1\").tags", existing: Some("lang_relation_tags_scoped"), expect_live_error: None },
Case { id: "relation_bind_projection_then_relation", python: "root = E.get(\"i1\")\ntrimmed = root.select(\"id\", \"title\")\ntags = trimmed.tags\nreturn tags", existing: Some("lang_bind_projection_then_relation"), expect_live_error: None },
Case { id: "relation_bind_relation_hop_one_one", python: "summary = E.get(\"i1\").summary\nreturn summary.select(\"headline\")", existing: Some("lang_bind_relation_hop_one_one"), expect_live_error: None },
Case { id: "relation_bind_filter_continuation", python: "root = E.query(owner=\"alice\")\nfiltered = root.where(lambda row: row.owner == \"alice\")\ntags = filtered.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_bind_filter_continuation"), expect_live_error: None },
Case { id: "relation_relation_many_from_plural_query", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_relation_many_from_plural_query"), expect_live_error: None },
Case { id: "relation_relation_prefer_embed_hit", python: "item = E.get(\"i1\")\ntags = item.tags\nreturn tags", existing: Some("lang_relation_prefer_embed_hit"), expect_live_error: None },
Case { id: "relation_relation_prefer_embed_miss", python: "items = E.query(owner=\"bob\").take(2)\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_relation_prefer_embed_miss"), expect_live_error: None },
Case { id: "relation_relation_integer_scoped_bindings", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.tags_by_score)\nreturn tags", existing: Some("lang_relation_integer_scoped_bindings"), expect_live_error: None },
Case { id: "relation_ra4_apply_relation_monolith", python: "return E.query().take(2).flat_map(lambda row: row.tags)", existing: Some("lang_ra4_apply_relation_monolith"), expect_live_error: None },
Case { id: "relation_ra4_apply_relation_bind_cut", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_ra4_apply_relation_bind_cut"), expect_live_error: None },
Case { id: "boolean_identity_false", python: "return E.get(False).select(\"id\")", existing: Some("lang_boolean_identity_false"), expect_live_error: None },
Case { id: "boolean_identity_true", python: "return E.get(True).select(\"id\")", existing: Some("lang_boolean_identity_true"), expect_live_error: None },
Case { id: "integer_identity", python: "return E.get(42).select(\"id\")", existing: Some("lang_integer_identity"), expect_live_error: None },
Case { id: "negative_identity", python: "return E.get(-42).select(\"id\")", existing: Some("lang_negative_identity"), expect_live_error: None },
Case { id: "large_identity", python: "return E.get(9007199254740993).select(\"id\")", existing: Some("lang_large_identity"), expect_live_error: None },
Case { id: "min_identity", python: "return E.get(-9223372036854775808).select(\"id\")", existing: Some("lang_min_identity"), expect_live_error: None },
Case { id: "compound_literal", python: "out = E.get(owner=\"alice\", item_id=\"i1\", name=\"main\")\nreturn out", existing: Some("lang_compound_literal"), expect_live_error: None },
Case { id: "compound_bound", python: "source = E.get(owner=\"alice\", item_id=\"i1\", name=\"main\")\nout = E.get(name=source.name, owner=source.owner, item_id=source.item_id)\nreturn source, out", existing: Some("lang_compound_bound"), expect_live_error: None },
Case { id: "bound_get_field", python: "source = E.get(\"i1\")\nout = E.get(source.id)\nreturn source, out", existing: Some("lang_bound_get_field"), expect_live_error: None },
Case { id: "bound_get_scalar", python: "source = E.get(\"i1\")\nkey = source.id\nout = E.get(key)\nreturn source, out", existing: Some("lang_bound_get_scalar"), expect_live_error: None },
Case { id: "bound_query_field", python: "source = E.get(\"i1\")\nout = E.query(owner=source.owner)\nreturn source, out", existing: Some("lang_bound_query_field"), expect_live_error: None },
Case { id: "bound_query_scalar", python: "source = E.get(\"i1\")\nowner = source.owner\nout = E.query(owner=owner)\nreturn source, out", existing: Some("lang_bound_query_scalar"), expect_live_error: None },
Case { id: "iterate_bound_identity", python: "source = E.get(\"c1\")\nkey = source.id\ncur = E.get(key)\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=2)\nreturn done", existing: Some("lang_iterate_bound_identity"), expect_live_error: None },
Case { id: "bound_get_empty", python: "source = E.query().where(lambda row: row.id == \"missing\").take(1)\nkey = source.id\nout = E.get(key)\nreturn out", existing: Some("lang_bound_get_empty"), expect_live_error: Some("zero rows") },
Case { id:"iterate_zero", python:"cur = E.get(\"c_done\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=3)\nreturn done", existing:Some("lang_iterate_until_zero_step"), expect_live_error:None },
Case { id:"iterate_exact", python:"cur = E.get(\"c1\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=2)\nreturn done", existing:None, expect_live_error:None },
Case { id:"iterate_success", python:"cur = E.get(\"c1\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=4)\nreturn done", existing:Some("lang_iterate_until_bound"), expect_live_error:None },
Case { id:"iterate_exhausted", python:"cur = E.get(\"c_stuck\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=2)\nreturn done", existing:Some("lang_iterate_bound_exhausted"), expect_live_error:Some("iterate_bound_exhausted") },
    Case { id: "iterate_expression_zero", python: "cur = E.get(\"c_done\")\nreturn cur.iterate(lambda row: row.TICK(), until=lambda row: any(row.phase == phase for phase in [\"done\", \"finished\"]), max_steps=3)", existing: None, expect_live_error: None },
    Case { id: "iterate_expression_exact", python: "cur = E.get(\"c1\")\nreturn cur.iterate(lambda row: row.TICK(), until=lambda row: row.id == \"c1\" and any(phase == \"done\" for phase in [row.phase]), max_steps=2)", existing: None, expect_live_error: None },
    Case { id: "iterate_expression_exhausted", python: "cur = E.get(\"c_stuck\")\nreturn cur.iterate(lambda row: row.TICK(), until=lambda row: any(row.phase == phase for phase in [\"done\"]), max_steps=2)", existing: None, expect_live_error: Some("iterate_bound_exhausted") },
    Case { id:"fanout_update", python:"rows = E.query().take(2)\nchanged = rows.flat_map(lambda row: row.UPDATE(title=row.title, score=9, owner=\"alice\"))\nreturn changed.select(\"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"fanout_captured_receiver", python:"target = E.get(\"i1\")\nrows = E.query().take(2)\nchanged = rows.flat_map(lambda row: target.UPDATE(title=row.title, score=9, owner=\"alice\"))\nreturn changed.select(\"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"fanout_captured_bounded", python:"target = E.query().take(1)\nrows = E.query().take(2)\nchanged = rows.flat_map(lambda row: target.UPDATE(title=row.title, score=9, owner=\"alice\"))\nreturn changed.select(\"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"fanout_captured_created", python:"target = E.CREATE(title=\"new\", score=0, owner=\"alice\")\nrows = E.query().take(2)\nchanged = rows.flat_map(lambda row: target.UPDATE(title=row.title, score=9, owner=\"alice\"))\nreturn changed.select(\"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"fanout_captured_empty", python:"target = E.query().where(lambda row: row.id == \"missing\").take(1)\nrows = E.query().take(2)\nchanged = rows.flat_map(lambda row: target.UPDATE(title=row.title, score=9, owner=\"alice\"))\nreturn changed.select(\"title\", \"score\", \"owner\")", existing:None, expect_live_error:Some("zero rows") },
    Case { id:"fanout_delete", python:"rows = E.query().take(2)\nreturn rows.flat_map(lambda row: row.DELETE())", existing:None, expect_live_error:None },
    Case { id:"fanout_empty", python:"rows = E.query().where(lambda row: row.id == \"missing\")\nreturn rows.flat_map(lambda row: row.DELETE())", existing:None, expect_live_error:None },
    Case { id:"fanout_read", python:"rows = E.query().take(2)\nagain = rows.flat_map(lambda row: E.get(row.id))\nreturn again.select(\"title\")", existing:None, expect_live_error:None },
    Case { id:"create", python:"made = E.CREATE(title=\"Matrix\", score=7, owner=\"alice\", active=True, tags=[\"x\", \"y\"])\nreturn made", existing:None, expect_live_error:None },
    Case { id:"update", python:"one = E.get(\"i1\")\nchanged = one.UPDATE(title=\"Changed\", score=8, owner=\"alice\")\nreturn changed.select(\"id\", \"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"delete", python:"one = E.get(\"i1\")\ngone = one.DELETE()\nreturn gone", existing:None, expect_live_error:None },
    Case { id:"broadcast", python:"done = E.BROADCAST(message=\"matrix\")\nreturn done", existing:None, expect_live_error:None },
    Case { id:"field_input", python:"one = E.get(\"i1\")\nmade = E.CREATE(title=one.title, score=8, owner=\"alice\")\nreturn made", existing:None, expect_live_error:None },
    Case { id:"scalar_input", python:"one = E.query().take(1)\ntext = one.title\nmade = E.CREATE(title=text, score=8, owner=\"alice\")\nreturn made", existing:None, expect_live_error:None },
    Case { id:"empty_write_receiver", python:"one = E.query().where(lambda row: row.id == \"missing\").take(1)\nchanged = one.UPDATE(title=\"never\")\nreturn changed", existing:None, expect_live_error:Some("zero rows") },
    Case {id:"empty_singleton",python:"one = E.query().where(lambda row: row.id == \"missing\").take(1)\nvalue = one.id\nreturn value",existing:None,expect_live_error:Some("zero rows")},
    Case {
        id: "singleton",
        python: "one = E.query().take(1)\nvalue = one.id\nreturn value",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "query",
        python: "return E.query()",
        expect_live_error: None,
        existing: Some("lang_query_all"),
    },
    Case {
        id: "get",
        python: "return E.get(\"i1\")",
        expect_live_error: None,
        existing: Some("lang_get_by_id"),
    },
    Case {
        id: "selection",
        python: "return E.query(owner=\"alice\")",
        expect_live_error: None,
        existing: Some("lang_predicate_brace_owner"),
    },
    Case {
        id: "take",
        python: "items = E.query()\nreturn items.take(3)",
        expect_live_error: None,
        existing: Some("lang_bind_first_limit"),
    },
    Case {
        id: "projection",
        python: "return E.query().take(1).select(\"id\", \"title\")",
        expect_live_error: None,
        existing: Some("lang_limit_projection"),
    },
    Case {
        id: "where",
        python: "return E.query().where(lambda row: row.score is not None and row.score > 10)",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "coerce",
        python: "return E.query().where(lambda row: row.score is not None and row.score >= int(\"10\"))",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "empty",
        python: "return E.query().where(lambda row: row.id == \"missing\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "grain",
        python: "return E.query().select(\"title\").where(lambda row: row.title == \"Alpha\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "quoted",
        python: "items = E.query()\nreturn items.where(lambda row: row.title == \"items\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "parallel",
        python: "a = E.get(\"i1\")\nb = E.get(\"i2\")\nreturn a, b",
        expect_live_error: None,
        existing: None,
    },
];
pub(super) fn cases() -> impl Iterator<Item = &'static Case> {
    CASES
        .iter()
        .chain(super::python_completion::CASES.iter())
        .chain(super::python_federated_parity::CASES.iter())
        .chain(super::python_render_parity::CASES.iter())
}
pub(super) fn program(body: &str, entity: &str) -> String {
    if body.starts_with("class ") {
        return body
            .replace("ENTITY", entity)
            .replace("E.", &format!("{entity}."));
    }
    format!(
        "class MatrixProgram(Program):\n    def build(self):\n{}\n",
        body.replace("E.", &format!("{entity}."))
            .lines()
            .map(|l| format!("        {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}
pub(super) fn parity_context(
    case: &Case,
    base: &str,
) -> (
    plasm_agent::execute_session::ExecuteSession,
    plasm_agent::server_state::PlasmHostState,
) {
    let mut schema = (*language_matrix::load_language_matrix_cgs()).clone();
    schema.http_backend = base.to_owned();
    let cgs = Arc::new(schema);
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base.to_owned()),
        ..Default::default()
    })
    .unwrap();
    let row = case.existing.and_then(find_row);
    if row.is_some_and(|r| r.federated) {
        if row.unwrap().id == "lang_federated_relation_target_entry" {
            let a = Arc::new(language_matrix::cgs_with_registry_entry_id(
                &cgs,
                language_matrix::MATRIX_FED_A,
            ));
            let b = Arc::new(language_matrix::cgs_with_registry_entry_id(
                &cgs,
                language_matrix::MATRIX_FED_B,
            ));
            return (
                language_matrix::matrix_federated_relation_target_session(a.clone(), b.clone()),
                language_matrix::matrix_federated_host_state(engine, a, b),
            );
        }
        let es = if row.unwrap().id == "lang_federated_auth_session_provides_mutation" {
            language_matrix::matrix_federated_auth_session_session(cgs.clone())
        } else {
            language_matrix::matrix_federated_duplicate_entity_session(cgs.clone())
        };
        return (
            es,
            language_matrix::matrix_federated_duplicate_entity_host_state(engine, cgs),
        );
    }
    let mut es = language_matrix::matrix_execute_session(cgs.clone());
    es.teaching_exposure.as_mut().unwrap().expose_entities(
        &[cgs.as_ref()],
        cgs.clone(),
        language_matrix::MATRIX_ENTRY_ID,
        &[
            "LangCursor",
            "CompoundBranch",
            "LangLane",
            "LangLaneStock",
            "LangVault",
            "LangOffer",
            "LangAuthSession",
            "LangLine",
        ],
    );
    (es, language_matrix::matrix_host_state(engine, cgs))
}
// Graph revision counters differ with valid scheduling and binding cuts. Keep
// identity, completeness and every domain field; normalize only embedded entity metadata.
fn normalize_embedded_revision_metadata(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => values
            .iter_mut()
            .for_each(normalize_embedded_revision_metadata),
        serde_json::Value::Object(fields) => {
            if fields.get("_ref").is_some_and(|reference| {
                reference.get("entity").and_then(|v| v.as_str()).is_some()
                    && reference.get("kind").and_then(|v| v.as_str()).is_some()
            }) {
                fields.remove("_last_updated");
                fields.remove("_version");
            }
            fields
                .values_mut()
                .for_each(normalize_embedded_revision_metadata);
        }
        _ => {}
    }
}

#[tokio::test]
async fn python_comparison_keeps_domain_fields_and_embedded_identity() {
    let mut value = serde_json::json!({"_version": 42, "_last_updated": 9, "children": [{"_ref": {"entity": "Child", "kind": "simple", "id": "c1"}, "_version": 3, "_last_updated": 5, "_completeness": "summary", "name": "child"}]});
    normalize_embedded_revision_metadata(&mut value);
    assert_eq!(
        value,
        serde_json::json!({"_version": 42, "_last_updated": 9, "children": [{"_ref": {"entity": "Child", "kind": "simple", "id": "c1"}, "_completeness": "summary", "name": "child"}]})
    );
}

pub(super) fn outputs(run: &PlasmPlanRunResult) -> Vec<Vec<serde_json::Value>> {
    run.return_steps
        .iter()
        .map(|step| {
            step.result
                .entities()
                .iter()
                .map(|entity| {
                    let mut row = plasm_runtime::entity_to_agent_row_json(entity, None);
                    normalize_embedded_revision_metadata(&mut row);
                    row
                })
                .collect()
        })
        .collect()
}
#[tokio::test]
async fn python_lowering_matrix_live_semantic_contract() {
    run_python_cases(cases()).await;
}

#[tokio::test]
async fn python_record_value_matrix() {
    run_python_cases(cases().filter(|case| case.id.starts_with("record_value_"))).await;
}

#[tokio::test]
async fn python_compute_input_inference_live() {
    run_python_cases(cases().filter(|case| case.id == "compute_inferred_callsite_inputs")).await;
}

#[tokio::test]
async fn python_compute_input_mode_follows_source_cardinality_live() {
    run_python_cases(cases().filter(|case| {
        matches!(
            case.id,
            "assembly_row_compute"
                | "repair_runtime_compute"
                | "text_per_row_zero"
                | "text_per_row_many"
                | "text_row_duplicates"
                | "text_empty_aggregate"
                | "text_synthetic_count"
                | "text_synthetic_group"
                | "text_multiline_whitespace"
                | "text_write_reuse"
                | "value_closure_column"
                | "value_closure_empty"
                | "value_closure_structural_singleton"
        )
    }))
    .await;
}

pub(super) async fn run_python_cases(selected: impl Iterator<Item = &'static Case>) {
    let mut failures = Vec::new();
    for case in selected {
        let result = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
                        let (es, host) = parity_context(case, &base);
                        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
                        let entity = witness_entity(case, &symbols);
                        let outcome = super::python_render_parity::python_outcome(case.id);
                        let compiled = compile_python_program(
                            &es,
                            &program(
                                &write_tokens(
                                    &super::python_federated_parity::source(case, &es),
                                    &symbols,
                                    language_matrix::MATRIX_ENTRY_ID,
                                ),
                                &entity,
                            ),
                        )
                        .await;
                        let compile_error = match outcome {
                            PythonOutcome::CompileError(message) => Some(message),
                            _ => None,
                        };
                        if let Err(error) = &compiled {
                            let expected = compile_error.unwrap_or_else(|| {
                                panic!("{} {} compile: {error}", case.id, "Python")
                            });
                            assert!(
                                error.contains(expected),
                                "{} compile expected {expected:?}: {error}",
                                case.id
                            );
                            return;
                        }
                        assert!(
                            !matches!(outcome, PythonOutcome::CompileError(_)),
                            "{} expected Python admission rejection",
                            case.id
                        );
                        let bundle = compiled.unwrap();
                        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
                        assert_comp_witness(&dry).unwrap();
                        if let Some(row) = super::python_coverage::covered_row(case) {
                            assert_planning_ir(
                                row,
                                &dry,
                                &serde_json::to_value(&bundle.artifact().comp).unwrap(),
                            )
                            .unwrap_or_else(|e| {
                                panic!("{} shared planning assertion: {e}", case.id)
                            });
                        }

                        let run = Box::pin(run_plasm_comp(
                            &es,
                            &host,
                            &es.prompt_hash,
                            &format!("python-matrix-{}", case.id),
                            &bundle,
                            true,
                            None,
                            None,
                            Some(dry),
                            None,
                        ))
                        .await;
                        let expected_error = match outcome {
                            PythonOutcome::LiveError(message) => Some(message),
                            PythonOutcome::ExplicitQualification => None,
                            _ => case.expect_live_error,
                        };
                        if let Some(expected) = expected_error {
                            let error = run.expect_err(
                                "expected execution failure after successful compilation",
                            );
                            super::python_expectations::assert_failure(case.id, &error);
                            assert!(
                                error.diagnostic().contains(expected),
                                "{}: {}",
                                case.id,
                                error.diagnostic()
                            );
                            return;
                        }
                        let run =
                            run.unwrap_or_else(|e| panic!("{} live: {}", case.id, e.diagnostic()));
                        super::python_expectations::assert_coverage(case.id, &run);
                        if matches!(case.id, "union_collapses_duplicates") {
                            let expected = 1;
                            assert_eq!(
                                run.return_steps[0].result.entities().len(),
                                expected,
                                "{} multiplicity",
                                case.id
                            );
                        }

                        if matches!(outcome, PythonOutcome::ExplicitQualification) {
                            super::python_render_parity::assert_replacement(case, &run);
                        }
                        if let Some(id) = case.existing {
                            super::assert_live::assert_row(
                                find_row(id).expect("existing semantic matrix row"),
                                &run,
                            )
                            .unwrap();
                        }
                        if case.existing.is_none() {
                            super::python_expectations::assert_supplemental(case.id, &run);
                        }
                    });
            })
            .unwrap()
            .join();
        if result.is_err() {
            failures.push(case.id);
        }
    }
    assert!(
        failures.is_empty(),
        "Python conformance failures: {failures:?}"
    );
}

#[tokio::test]
async fn python_lowering_matrix_rejects_invalid_semantics_at_compile() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().id",
        "items = E.query()\nreturn {\"id\": items.id}",
        "item = E.get(\"i1\")\nreturn {\"bad\": item.absent}",
        "item = E.get(\"i1\").select(\"title\")\nreturn {\"id\": item.id}",
        "item = E.get(\"i1\")\nrecord = {\"id\": item.id}\nreturn record.tags",
        "item = E.get(\"i1\")\nrecord = {\"id\": item.id}\nreturn record.m1()",
        "item = E.get(\"i1\")\nrecord = {\"name\": item.title}\nreturn record.absent",
        "return E.query(score=10)", // RA-2: field is not a backend selection slot.
        "return E.query().select(\"absent\")",
        "return E.query().select(\"title\").where(lambda row: row.score is not None and row.score > 0)", // RA-2 current grain.
        "return E.query().where(lambda row: row.absent == 0)",
        "return E.query().where(lambda row: row.score < \"invalid\")",
        "return E.query().take(4294967296)",
        "return E.query().take(-1)",
        "return E.query().take(True)",
        "return E.query().map(lambda row: row)",
        "return E.create(title=\"write\")",
        "return E.query().where(lambda row: other.score > 0)",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
}

#[tokio::test]
async fn python_lowering_matrix_federated_symbols_keep_qualified_ownership() {
    let es = language_matrix::matrix_federated_duplicate_entity_session(
        language_matrix::load_language_matrix_cgs(),
    );
    let bundle=compile_python_program(&es,"class Reads(Program):\n    def build(self):\n        a = e1.query()\n        b = e2.query()\n        return a, b\n").await.unwrap();
    let owner = |id: &str| {
        let plasm_core::plasm_monad::PlasmStepPayload::Invoke(p) =
            &bundle.artifact().comp.steps[id]
        else {
            panic!("expected read")
        };
        p.qualified_entity.as_ref().unwrap().entry_id.clone()
    };
    assert_eq!(owner("a"), language_matrix::MATRIX_FED_A);
    assert_eq!(owner("b"), language_matrix::MATRIX_FED_B);
}

pub(super) fn write_tokens(
    body: &str,
    symbols: &plasm_core::symbol_tuning::SymbolMap,
    entry: &str,
) -> String {
    let mut body = body.to_owned();
    for (token, entity, method) in [
        ("OFFER_CREATE", "LangOffer", "create"),
        ("LOGIN", "LangAuthSession", "login"),
        ("TOUCH", "LangItem", "secured-touch"),
    ] {
        if body.contains(token) {
            body = body.replace(token, &symbols.method_sym_for(entry, entity, method));
        }
    }
    for (token, entity) in [("OFFER.", "LangOffer"), ("AUTH.", "LangAuthSession")] {
        if body.contains(token) {
            body = body.replace(
                token,
                &format!("{}.", symbols.entity_sym_for(entry, entity)),
            );
        }
    }
    for method in ["create", "update", "delete", "broadcast", "ping"] {
        if !body.contains(&method.to_uppercase()) {
            continue;
        }
        body = body.replace(
            &method.to_uppercase(),
            &symbols.method_sym_for(entry, "LangItem", method),
        );
    }
    for (token, entity) in [
        ("LANE.", "LangLane"),
        ("STOCK.", "LangLaneStock"),
        ("VAULT.", "LangVault"),
    ] {
        if !body.contains(token) {
            continue;
        }
        body = body.replace(
            token,
            &format!("{}.", symbols.entity_sym_for(entry, entity)),
        );
    }
    if body.contains("REL_TAGS") {
        body = body.replace(
            "REL_TAGS",
            &symbols.ident_sym_relation_for(entry, "LangItem", "tags"),
        );
    }
    if body.contains("TICK") {
        body = body.replace("TICK", &symbols.method_sym_for(entry, "LangCursor", "tick"));
    }
    if body.contains("LINE_ENTITY") {
        body = body.replace("LINE_ENTITY", &symbols.entity_sym_for(entry, "LangLine"));
    }
    if body.contains("REL_COMPLETE_LINES") {
        body = body.replace(
            "REL_COMPLETE_LINES",
            &symbols.ident_sym_relation_for(entry, "LangItem", "complete_lines"),
        );
    }
    if body.contains("REL_UNPROVEN_LINES") {
        body = body.replace(
            "REL_UNPROVEN_LINES",
            &symbols.ident_sym_relation_for(entry, "LangItem", "lines"),
        );
    }
    body
}
#[tokio::test]
async fn python_write_matrix_rejects_invalid_operands_and_receivers() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "item = E.get(\"i1\")\nrecord = {\"id\": item.id}\nreturn record.PING()",
        "return E.query().flat_map(lambda row: row.UPDATE(title=row.absent))",
        "rows = E.query()\nreturn rows.flat_map(lambda self: self.DELETE())",
        "return E.query().flat_map(lambda row, other: row.DELETE())",
        "return E.query().flat_map(lambda row: row.DELETE()).DELETE()",
        "return E.query().flat_map(lambda row: E.CREATE(title=row))",
        "return E.query().flat_map(lambda row: other.DELETE())",
        "return E.query().flat_map(lambda row: row.title)",
        "target = E.query()\nreturn E.query().flat_map(lambda row: target.UPDATE(title=row.title))",
        "return E.CREATE()",
        "return E.CREATE(title=\"x\", absent=1)",
        "return E.CREATE(title=\"x\", score=\"invalid\")",
        "return E.CREATE(\"x\")",
        "return E.CREATE(**payload)",
        "return E.UPDATE(title=\"x\")",
        "return E.DELETE()",
        "return E.PING()",
        "return E.query().UPDATE(title=\"x\")",
        "one = E.get(\"i1\")\nreturn E.CREATE(title=one.absent)",
        "many = E.query()\nreturn E.CREATE(title=many.title)",
        "one = E.get(\"i1\")\nreturn E.CREATE(title=one)",
        "return E.CREATE(title=str(1, 2, 3, 4))",
    ] {
        let body = write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID);
        assert!(
            compile_python_program(&es, &program(&body, &entity))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
    let es = language_matrix::matrix_federated_duplicate_entity_session(
        language_matrix::load_language_matrix_cgs(),
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let method = symbols.method_sym_for(language_matrix::MATRIX_FED_B, "LangItem", "create");
    assert!(compile_python_program(
        &es,
        &program(&format!("return E.{method}(title=\"x\")"), "e1")
    )
    .await
    .is_err());
}

#[tokio::test]
async fn python_write_review_identity_seals_operands_and_order() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = program(
        &write_tokens(
            "E.BROADCAST(message=\"first\")\nE.BROADCAST(message=\"second\")\nreturn E.get(\"i1\")",
            &symbols,
            language_matrix::MATRIX_ENTRY_ID,
        ),
        &entity,
    );
    let compile = async |source: &str| compile_python_program(&es, source).await.unwrap();
    let original = compile(&source).await;
    let renamed = compile(&source.replace("MatrixProgram", "AnotherName")).await;
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &original.artifact().comp,
        &renamed.artifact().comp
    ));
    for changed in [
        source.replace("first", "changed"),
        source
            .replace("first", "temporary")
            .replace("second", "first")
            .replace("temporary", "second"),
    ] {
        assert!(!plasm_core::plasm_monad::comp_semantic_eq(
            &original.artifact().comp,
            &compile(&changed).await.artifact().comp
        ));
    }
    let comp = &original.artifact().comp;
    assert_eq!(
        comp.steps.len(),
        3,
        "discarding results must not discard effects"
    );
    assert!(comp
        .bind
        .topo
        .windows(2)
        .all(|pair| comp.bind.deps[&pair[1]].contains(&pair[0])));
}

#[tokio::test]
async fn python_fanout_discarded_effects_and_row_scope_are_sealed() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = program(
        &write_tokens(
            "rows = E.query().take(2)\nrows.flat_map(lambda row: row.PING())\nreturn E.query()",
            &symbols,
            language_matrix::MATRIX_ENTRY_ID,
        ),
        &entity,
    );
    let bundle = compile_python_program(&es, &source).await.unwrap();
    let comp = &bundle.artifact().comp;
    assert!(
        !comp.steps.contains_key("_"),
        "compiler row scope must not become an executable take"
    );
    let (effect, _) = comp
        .steps
        .iter()
        .find(|(_, step)| matches!(step, plasm_core::plasm_monad::PlasmStepPayload::MapBody(_)))
        .unwrap();
    let final_id = comp.bind.topo.last().unwrap();
    assert!(
        comp.bind.deps[final_id].contains(&plasm_core::plasm_monad::StepId::new(effect).unwrap())
    );
    let renamed = compile_python_program(
        &es,
        &source.replace("lambda row: row.", "lambda item: item."),
    )
    .await
    .unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        comp,
        &renamed.artifact().comp
    ));
}

#[tokio::test]
async fn python_captured_receiver_review_seals_identity_and_dependencies() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = program(&write_tokens("target = E.get(\"i1\")\nrows = E.query().take(2)\nchanged = rows.flat_map(lambda row: target.UPDATE(title=row.title))\nreturn changed", &symbols, language_matrix::MATRIX_ENTRY_ID), &entity);
    let bundle = compile_python_program(&es, &source).await.unwrap();
    let comp = &bundle.artifact().comp;
    let changed = plasm_core::plasm_monad::StepId::new("changed").unwrap();
    for dependency in ["target", "rows"] {
        assert!(comp.bind.deps[&changed]
            .contains(&plasm_core::plasm_monad::StepId::new(dependency).unwrap()));
    }
    assert!(!comp.steps.contains_key("_"));
    let other = compile_python_program(&es, &source.replace("i1", "i2"))
        .await
        .unwrap();
    assert!(!plasm_core::plasm_monad::comp_semantic_eq(
        comp,
        &other.artifact().comp
    ));
}

#[tokio::test]
async fn python_iteration_admission_and_review_seal() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let code = "seed = E.get(\"i1\")\ndone = seed.iterate(lambda row: row.UPDATE(score=1), until=lambda row: row.score is not None and row.score >= 1, max_steps=2)\nreturn done";
    let compile = async |body: &str| {
        compile_python_program(
            &es,
            &program(
                &write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID),
                &entity,
            ),
        )
        .await
    };
    let valid = compile(code).await.unwrap();
    for bad in [
        code.replace("E.get(\"i1\")", "E.query()"),
        code.replace("E.get(\"i1\")", "E.query().take(1)"),
        code.replace("E.get(\"i1\")", "E.get(\"i1\").select(\"id\", \"score\")"),
        code.replace(", max_steps=2", ""),
        code.replace("max_steps=2", "max_steps=0"),
        code.replace("max_steps=2", "max_steps=-1"),
        code.replace("max_steps=2", "max_steps=True"),
        code.replace("max_steps=2", "max_steps=4294967296"),
        code.replace("max_steps=2", "max_steps=limit"),
        code.replace("max_steps=2", "max_steps=2, limit=3"),
        code.replace("until=lambda row: row.score is not None and row.score >= 1, ", ""),
        code.replace("row.score is not None and row.score >= 1", "row.absent >= 1"),
        code.replace("row.score is not None and row.score >= 1", "other.score >= 1"),
        code.replace("row.score is not None and row.score >= 1", "row.score >= \"invalid\""),
        code.replace("row.UPDATE(score=1)", "E.get(row.id)"),
        code.replace("row.UPDATE(score=1)", "row.iterate(lambda nested: nested.UPDATE(score=1), until=lambda nested: nested.score > 1, max_steps=2)"),
    ] { assert!(compile(&bad).await.is_err(),"accepted {bad}"); }
    let renamed = compile(
        &code
            .replace("lambda row:", "lambda item:")
            .replace("row.", "item."),
    )
    .await
    .unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &valid.artifact().comp,
        &renamed.artifact().comp
    ));
    for changed in [
        code.replace("max_steps=2", "max_steps=3"),
        code.replace(
            "row.score is not None and row.score >= 1",
            "row.score is not None and row.score >= 2",
        ),
    ] {
        assert!(!plasm_core::plasm_monad::comp_semantic_eq(
            &valid.artifact().comp,
            &compile(&changed).await.unwrap().artifact().comp
        ));
    }
    let discarded = compile(
        &code
            .replace("done = seed.iterate", "seed.iterate")
            .replace("return done", "return E.get(\"i1\")"),
    )
    .await
    .unwrap();
    let comp = &discarded.artifact().comp;
    let effect = comp
        .steps
        .iter()
        .find(|(_, step)| {
            matches!(
                step,
                plasm_core::plasm_monad::PlasmStepPayload::UnfoldUntil(_)
            )
        })
        .unwrap()
        .0;
    assert!(comp.bind.deps[comp.bind.topo.last().unwrap()]
        .contains(&plasm_core::plasm_monad::StepId::new(effect).unwrap()));
}

#[tokio::test]
async fn python_predicate_relation_position_matrix() {
    run_python_cases(
        CASES
            .iter()
            .filter(|case| case.id.starts_with("predicate_")),
    )
    .await;
}

#[tokio::test]
async fn python_predicate_filter_preserves_receiver() {
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let case = Case {
        id: "predicate_receiver",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, host) = parity_context(&case, &base);
    let body = "item = E.get(\"i1\")\nchosen = item.where(lambda row: any(child.note == \"line-a\" for child in row.REL_COMPLETE_LINES))\nreturn chosen.PING()";
    let bundle = compile_fixture(&es, body).await.unwrap();
    let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    let run = Box::pin(run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "predicate-receiver",
        &bundle,
        true,
        None,
        None,
        Some(dry),
        None,
    ))
    .await
    .unwrap();
    let invocations: usize = run
        .return_steps
        .iter()
        .flat_map(|step| step.result.operations.entries())
        .map(|ack| ack.logical_invocations)
        .sum();
    assert_eq!(
        invocations, 1,
        "a selected parent retains exactly its own receiver authority"
    );
}

#[tokio::test]
async fn python_predicate_value_position_closure() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    // The same recursive constructor is admitted at each consuming boundary.
    // Domain reads inside skipped branches are a separate conditional-scope obligation.
    for expression in [
        "None is None",
        "None is not 1",
        "1 is not None and 2 != None",
        "any(value > 1 for value in [0, 2, 3])",
        "True and any(value > 1 for value in [0, 2, 3])",
        "False or all(value > 0 for value in [1, 2, 3])",
        "not any(value < 0 for value in [1, 2, 3])",
        "any(value > 1 for value in [0, 2]) if True else False",
        "any(all(value > 0 for value in group) for group in [[1, 2], [3]])",
        "1 + 2 < 4 * 2",
        "0 < 1 + 2 < 10",
        "2 in (1, 1 + 1, 3)",
        "any(value > 0 for value in (1, 2))",
        "\"i1\" in E.get(\"i1\").select(\"id\")",
    ] {
        for body in [
            format!("return {expression}"),
            format!("return {{\"value\": {expression}}}"),
            format!("return E.query().where(lambda row: {expression})"),
            format!("return E.query().select(value=lambda row: {expression})"),
            format!("return E.query().map(lambda row: {{\"value\": {expression}}}, max_parents=10)"),
            format!("return E.get(\"i1\").iterate(lambda row: row.PING(), until=lambda row: {expression}, max_steps=2)"),
            format!("return E.CREATE(title=\"closure\", score=1, owner=\"alice\", active={expression})"),
        ] {
            compile_fixture(&es, &body).await.unwrap_or_else(|e| panic!("{body}: {e}"));
        }
    }
}

#[tokio::test]
async fn python_predicate_iteration_expression_matrix() {
    run_python_cases(
        CASES
            .iter()
            .filter(|case| case.id.starts_with("iterate_expression_")),
    )
    .await;
}

#[tokio::test]
async fn python_quantified_predicate_admission() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    for body in [
        "return E.query().where(lambda row: any(child.note == \"first\" for child in row.REL_COMPLETE_LINES))",
        "return E.query().where(lambda row: all(child.note != \"missing\" for child in row.REL_COMPLETE_LINES))",
        "item = E.get(\"i1\")\nreturn {\"found\": any(child.note == \"first\" for child in item.REL_COMPLETE_LINES)}",
        "return E.get(\"i1\").iterate(lambda row: row.PING(), until=lambda row: any(child.note == \"first\" for child in row.REL_COMPLETE_LINES), max_steps=2)",
        "return E.get(\"i1\").iterate(lambda row: row.PING(), until=lambda row: row.score is not None and row.score >= 1 and row.score <= 100, max_steps=2)",
        "return {\"found\": any(value > 1 for value in [0, 2, 3]), \"empty\": all([])}",
        "return {\"found\": any(value > 1 for values in [[0], [2, 3]] for value in values)}",
        "return E.query().select(found=lambda row: any(row.score is not None and value > row.score for value in [0, 20, 40]))",
        "return E.query().where(lambda row: row.title is not None)",
        "return E.query().where(lambda row: row.score is not None and row.score > 1 + 2)",
        "return E.query().where(lambda row: row.score == row.score)",
        "threshold = E.get(\"i1\")\nreturn E.query().where(lambda row: threshold.score is not None and row.score is not None and threshold.score <= row.score)",
        "return E.get(\"i1\").iterate(lambda row: row.PING(), until=lambda row: row.id in E.get(\"i1\").select(\"id\"), max_steps=1)",

    ] {
        compile_fixture(&es, body).await.unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}

#[tokio::test]
async fn python_quantified_predicate_live_values() {
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let case = Case {
        id: "quantifiers",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, host) = parity_context(&case, &base);
    for (index, (body, expected)) in [
        ("item = E.get(\"i1\")\nreturn {\"value\": any(child.note == \"line-a\" for child in item.REL_COMPLETE_LINES)}", true),
        ("item = E.get(\"i1\")\nreturn {\"value\": all(child.note != \"missing\" for child in item.REL_COMPLETE_LINES)}", true),
        ("item = E.get(\"i1\")\nreturn {\"value\": any(child.note == \"missing\" for child in item.REL_COMPLETE_LINES)}", false),
        ("item = E.get(\"i1\")\nreturn {\"value\": all(child.note == \"line-a\" for child in item.REL_COMPLETE_LINES)}", false),
        ("item = E.get(\"i1\")\nreturn {\"value\": any(child.note == \"line-a\" for child in item.REL_COMPLETE_LINES if child.note != \"line-a\")}", false),
        ("item = E.get(\"i1\")\nreturn {\"value\": all(child.note == \"line-a\" for child in item.REL_COMPLETE_LINES if child.note == \"missing\")}", true),
        ("return {\"value\": any(value > 1 for value in [0, 2, 3])}", true),
        ("return {\"value\": any(value > 1 for values in [[0], [2, 3]] for value in values)}", true),

        ("return {\"value\": \"i1\" in E.get(\"i1\").select(\"id\")}", true),
        ("return {\"value\": \"absent\" not in E.get(\"i1\").select(\"id\")}", true),
        ("return {\"value\": False and \"i1\" in E.get(\"missing\").select(\"id\")}", false),

        ("return {\"value\": \"alice\" in (E.get(\"i1\").owner or \"\")}", true),
        ("return {\"value\": \"alice\" in [\"alice\"]}", true),
        ("return {\"value\": any([])}", false),
        ("return {\"value\": all([])}", true),
        ("return {\"value\": 3 < 2 < (E.get(\"missing\").score or 0)}", false),
        ("return {\"value\": 0 < (E.get(\"i1\").score or 0) < 100}", true),
        ("return {\"value\": any(value > 0 or 1 / 0 > 0 for value in [1])}", true),
        ("return {\"value\": all(value < 0 and 1 / 0 > 0 for value in [1])}", false),
        ("return {\"value\": False and (E.get(\"missing\").score or 0) > 0}", false),
        ("return {\"value\": False and any(child.note == \"x\" for child in E.get(\"missing\").REL_COMPLETE_LINES)}", false),
        ("return {\"value\": True or any(child.note == \"x\" for child in E.get(\"missing\").REL_COMPLETE_LINES)}", true),
        ("return {\"value\": any(child.note == \"x\" for child in E.get(\"missing\").REL_COMPLETE_LINES) if False else True}", true),
        ("item = E.get(\"i1\")\nreturn {\"value\": True and any(child.note == \"line-a\" for child in item.REL_COMPLETE_LINES)}", true),
        ("item = E.get(\"i1\")\nreturn {\"value\": False and any(child.note == \"line-a\" for child in item.REL_UNPROVEN_LINES)}", false),
    ].into_iter().enumerate() {
        let bundle = compile_fixture(&es, body).await.unwrap_or_else(|e| panic!("{body}: {e}"));
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = Box::pin(run_plasm_comp(&es, &host, &es.prompt_hash, &format!("quantifier-{index}"), &bundle, true, None, None, Some(dry), None)).await.unwrap_or_else(|e| panic!("{body}: {e}"));
        assert_eq!(outputs(&run)[0][0]["value"], serde_json::json!(expected), "{body}");
    }
}

#[tokio::test]
async fn python_predicate_lazy_branch_preserves_joined_types() {
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let case = Case {
        id: "branch_types",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, host) = parity_context(&case, &base);
    for (index, (body, expected)) in [
        (
            "return {\"value\": 7 if True else E.get(\"missing\").title}",
            serde_json::json!(7),
        ),
        (
            "return {\"value\": E.get(\"missing\").score if False else \"text\"}",
            serde_json::json!("text"),
        ),
        (
            "return {\"value\": [1, 2] if True else E.get(\"missing\").title}",
            serde_json::json!([1, 2]),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let bundle = compile_fixture(&es, body).await.unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = Box::pin(run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            &format!("branch-types-{index}"),
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        ))
        .await
        .unwrap();
        assert_eq!(outputs(&run)[0][0]["value"], expected, "{body}");
    }
}

#[tokio::test]
async fn python_quantified_predicate_rejects_unproven_collection() {
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let case = Case {
        id: "quantifier_unknown",
        python: "",
        existing: None,
        expect_live_error: None,
    };
    let (es, host) = parity_context(&case, &base);
    for operation in ["any", "all"] {
        let body = format!("item = E.get(\"i1\")\nreturn {{\"value\": {operation}(child.note == \"line-a\" for child in item.REL_UNPROVEN_LINES)}}");
        let bundle = compile_fixture(&es, &body).await.unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let error = Box::pin(run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            operation,
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        ))
        .await
        .expect_err("unproven relation must not establish a quantified result");
        assert_eq!(error.code, "collection_incomplete");
        assert_eq!(error.cause, plasm_runtime::FailureCause::ResponseContract);
    }
}

#[tokio::test]
async fn python_iteration_until_binding_is_a_sealed_dependency() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for operand in ["expected.score", "threshold"] {
        let scalar = if operand == "threshold" {
            "threshold = expected.score\n"
        } else {
            ""
        };
        let body = format!("expected = E.get(\"i1\")\n{scalar}seed = E.get(\"i2\")\ndone = seed.iterate(lambda row: row.PING(), until=lambda row: row.score is not None and {operand} is not None and row.score >= {operand}, max_steps=3)\nreturn done");
        let code = program(
            &write_tokens(&body, &symbols, language_matrix::MATRIX_ENTRY_ID),
            &entity,
        );
        let bundle = compile_python_program(&es, &code).await.unwrap();
        let comp = &bundle.artifact().comp;
        let dependency = if operand == "threshold" {
            "threshold"
        } else {
            "expected"
        };
        assert!(
            comp.bind.deps[&plasm_core::plasm_monad::StepId::new("done").unwrap()]
                .contains(&plasm_core::plasm_monad::StepId::new(dependency).unwrap())
        );
        assert!(compile_python_program(
            &es,
            &code.replace(
                &format!("{entity}.get(\"i1\")"),
                &format!("{entity}.query()")
            )
        )
        .await
        .is_err());
    }
}

#[tokio::test]
async fn python_bound_reads_reject_invalid_references() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for python in [
        "rows = E.query()\nreturn E.get(rows.id)",
        "rows = E.query()\nreturn E.query(owner=rows.owner)",
        "row = E.get(\"i1\")\nreturn E.get(row.absent)",
        "row = E.get(\"i1\").select(\"title\")\nreturn E.query(owner=row.owner)",
    ] {
        assert!(
            compile_python_program(&es, &program(python, &entity))
                .await
                .is_err(),
            "Python accepted {python}"
        );
    }
    for python in [
        "one = E.query()\nreturn E.query().where(lambda row: row.title == one.title)",
        "one = E.get(\"i1\")\nreturn E.query().where(lambda row: row.title == one.absent)",
        "one = E.get(\"i1\")\nreturn E.query().where(lambda self: self.title == self.title)",
        "one = E.get(\"i1\").select(\"id\")\nreturn E.query().where(lambda row: row.title == one.title)",
        "return E.get(unknown)",
        "return E.query(owner=unknown)",
        "return E.get(None)",
        "return E.get([\"i1\"])",
    ] {
        assert!(
            compile_python_program(&es, &program(python, &entity))
                .await
                .is_err(),
            "Python accepted {python}"
        );
    }
}

#[tokio::test]
async fn python_identity_admission_and_review_seal() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let mut es = language_matrix::matrix_execute_session(cgs.clone());
    es.teaching_exposure.as_mut().unwrap().expose_entities(
        &[cgs.as_ref()],
        cgs.clone(),
        language_matrix::MATRIX_ENTRY_ID,
        &["CompoundBranch"],
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let item = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let compound = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "CompoundBranch");
    for (python, entity) in [
        ("return E.get(1.5)", &item),
        ("return E.get(owner=\"alice\", name=\"main\")", &compound),
        (
            "return E.get(owner=\"alice\", item_id=\"i1\", name=\"main\", extra=\"x\")",
            &compound,
        ),
        ("return E.get(\"main\")", &compound),
    ] {
        assert!(
            compile_python_program(&es, &program(python, entity))
                .await
                .is_err(),
            "Python accepted {python}"
        );
    }
    for body in [
        "return E.get(owner=\"alice\", owner=\"bob\", item_id=\"i1\", name=\"main\")",
        "return E.get(**{\"owner\": \"alice\", \"item_id\": \"i1\", \"name\": \"main\"})",
        "return E.get(\"alice\", item_id=\"i1\", name=\"main\")",
        "return E.get(owner=unknown, item_id=\"i1\", name=\"main\")",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &compound))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
    for body in [
        "return E.get()",
        "return E.get(\"i1\", \"i2\")",
        "return E.get(\"i1\", identity=\"i2\")",
        "return E.get(identity=\"i1\", identity=\"i2\")",
        "return E.get(id=\"i1\")",
        "return E.get(identity=\"i1\", extra=\"x\")",
        "return E.get(identity=None)",
        "return E.get(identity=1.5)",
        "return E.get(identity=unknown)",
        "rows = E.query()\nreturn E.get(identity=rows.id)",
        "return E.get(**{\"identity\": \"i1\"})",
        "return E.get(9223372036854775808)",
        "return E.get(-9223372036854775809)",
        "return E.get(-1.5)",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &item))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
    for identity in [
        "\"i1\"",
        "True",
        "False",
        "42",
        "-1",
        "-9223372036854775808",
        "source.id",
    ] {
        let positional = format!("source = E.get(\"i1\")\nreturn E.get({identity})");
        let keyword = format!("source = E.get(\"i1\")\nreturn E.get(identity={identity})");
        let a = compile_python_program(&es, &program(&positional, &item))
            .await
            .unwrap();
        let b = compile_python_program(&es, &program(&keyword, &item))
            .await
            .unwrap();
        assert!(
            plasm_core::plasm_monad::comp_semantic_eq(&a.artifact().comp, &b.artifact().comp),
            "identity call spelling changed the reviewed plan: {identity}"
        );
    }
    let compile = async |body: &str| {
        compile_python_program(&es, &program(body, &compound))
            .await
            .unwrap()
    };
    let a = compile("return E.get(owner=\"alice\", item_id=\"i1\", name=\"main\")").await;
    let b = compile("return E.get(name=\"main\", item_id=\"i1\", owner=\"alice\")").await;
    let changed = compile("return E.get(owner=\"bob\", item_id=\"i1\", name=\"main\")").await;
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &a.artifact().comp,
        &b.artifact().comp
    ));
    assert!(!plasm_core::plasm_monad::comp_semantic_eq(
        &a.artifact().comp,
        &changed.artifact().comp
    ));
}

#[tokio::test]
async fn python_relations_reject_invalid_scope_and_preserve_symbols() {
    let cgs = language_matrix::load_language_matrix_cgs();
    let es = language_matrix::matrix_execute_session(cgs);
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().tags",
        "return E.query().take(2).tags",
        "return E.query().flat_map(lambda row: row.id)",
        "return E.query().flat_map(lambda row: other.tags)",
        "return E.query().flat_map(lambda row: row.absent)",
        "items = E.query()\nreturn items.flat_map(lambda self: self.tags)",
        "return E.query().flat_map(lambda row: row.tags.lines)",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
    let relation =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "tags");
    let compile = async |body: &str| {
        compile_python_program(&es, &program(body, &entity))
            .await
            .unwrap()
    };
    let wire =
        compile("items = E.query().take(2)\nreturn items.flat_map(lambda row: row.tags)").await;
    let tuned = compile(&format!(
        "items = E.query().take(2)\nreturn items.flat_map(lambda item: item.{relation})"
    ))
    .await;
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &wire.artifact().comp,
        &tuned.artifact().comp
    ));
    let scalar = compile("one = E.get(\"i1\")\nreturn one.tags").await;
    let tuned_scalar = compile(&format!("one = E.get(\"i1\")\nreturn one.{relation}")).await;
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &scalar.artifact().comp,
        &tuned_scalar.artifact().comp
    ));
}

#[tokio::test]
async fn python_relation_symbols_keep_federated_source_ownership() {
    let es = language_matrix::matrix_federated_duplicate_entity_session(
        language_matrix::load_language_matrix_cgs(),
    );
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let relation =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_FED_B, "LangItem", "children");
    assert!(
        plasm_core::SymbolMap::is_opaque_r_sym(&relation),
        "expected r# token, got {relation}"
    );
    let source = format!("class Relations(Program):\n    def build(self):\n        parent = e2.get(\"i1\")\n        children = parent.{relation}\n        return children\n");
    let bundle = compile_python_program(&es, &source).await.unwrap();
    let comp = serde_json::to_value(&bundle.artifact().comp).unwrap();
    let relation =
        super::ir_helpers::comp_relation_named(&comp, "children").expect("typed relation payload");
    assert_eq!(
        relation["target"]["entry_id"],
        language_matrix::MATRIX_FED_B
    );
    assert_eq!(relation["source"], "parent");
    assert!(
        compile_python_program(&es, &source.replace("e2.get", "e1.get"))
            .await
            .is_err(),
        "foreign relation symbol must not be reinterpreted in the other catalog"
    );
}

#[tokio::test]
async fn python_ordering_rejects_invalid_fields_and_arguments() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().order_by()",
        "return E.query().order_by(1)",
        "return E.query().order_by(\"absent\")",
        "return E.query().select(\"title\").order_by(\"score\")",
        "return E.query().order_by(\"score\", descending=1)",
        "return E.query().order_by(\"score\", reverse=True)",
        "return E.query().order_by(\"score\", True)",
        "return E.query().order_by(\"score desc\")",
        "return E.query().order_by(\"score\", **{})",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
}

#[tokio::test]
async fn python_projection_aliases_reject_invalid_grain_and_arguments() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().select(handle=1)",
        "return E.query().select(handle=\"missing\")",
        "return E.query().select(\"owner\", owner=\"title\")",
        "return E.query().select(**{})",
        "return E.query().select(\"title\").select(handle=\"owner\")",
        "return E.query().select(handle=\"owner\").where(lambda row: row.owner == \"alice\")",
        "return E.query().select(handle=\"owner\").order_by(\"owner\")",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
}

#[tokio::test]
async fn python_set_operations_reject_invalid_row_shapes() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().select(\"owner\").union(E.query().select(\"title\"))",
        "return E.query().union([1, 2])",
        "return E.query().select(\"owner\").union()",
        "rhs = E.query().select(\"id\", \"owner\")\nreturn E.query().where(lambda row: row.owner in rhs)",
        "return E.query().where(lambda row: row.owner in 1)",
        "return E.query().where(lambda row: row.owner in missing)",
        "row = E.query().select(\"owner\")\nreturn E.query().where(lambda row: row.owner in row)",
        "rhs = E.query().select(\"owner\")\nreturn E.query().select(\"title\").where(lambda row: row.owner in rhs)",
        "rhs = E.query().select(\"id\", \"owner\")\nreturn rhs.union(rhs).tags",
    ] { assert!(compile_python_program(&es, &program(body, &entity)).await.is_err(), "accepted {body}"); }
}

#[tokio::test]
async fn python_inline_membership_requires_closed_read_rowsets() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().where(lambda row: row.owner in E.query(owner=row.owner).select(\"owner\"))",
        "return E.query().where(lambda row: row.owner in E.query().where(lambda other: other.owner == row.owner).select(\"owner\"))",
        "row = E.get(\"i1\")\nreturn E.query().where(lambda row: row.owner in row.select(\"owner\"))",
        "return E.query().where(lambda row: row.owner in E.CREATE(title=\"hidden\").select(\"owner\"))",
        "return E.query().where(lambda row: row.owner in E.query().flat_map(lambda other: other.UPDATE(title=\"hidden\")).select(\"owner\"))",
        "return E.query().where(lambda row: row.owner in E.query().select(\"id\", \"owner\"))",
        "return E.query().where(lambda row: row.owner in E.get(\"i1\").score)",
    ] {
        let body = write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID);
        assert!(compile_python_program(&es, &program(&body, &entity)).await.is_err(), "accepted {body}");
    }
}

#[tokio::test]
async fn python_matrix_relation_fixture_is_stable_under_hydration() {
    let base = hermit_lang_matrix::language_matrix_python_hermit_base_url().await;
    let client = reqwest::Client::new();
    let query = format!("{base}/language/v1/tags?item_id=fixture-parent");
    let before: serde_json::Value = client
        .get(&query)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(before.as_array().unwrap().len(), 2);
    for row in before.as_array().unwrap() {
        let mut url = reqwest::Url::parse(&format!("{base}/language/v1/tags/")).unwrap();
        url.path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .push(row["id"].as_str().unwrap());
        let hydrated: serde_json::Value = client
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(&hydrated, row);
    }
    client
        .get(format!("{base}/language/v1/tags/unseen-embedded-tag"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let after: serde_json::Value = client
        .get(&query)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        before, after,
        "hydrating a reference must not grow scoped query results"
    );
    let other: serde_json::Value = client
        .get(format!("{base}/language/v1/tags?item_id=other-parent"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(other
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["item_id"] == "other-parent"));
    assert_ne!(
        before, other,
        "different receivers retain different tag identities"
    );
}

#[tokio::test]
async fn python_reductions_reject_invalid_descriptors_and_grain() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().aggregate()",
        "return E.query().group_by(n=agg.count())",
        "return E.query().group_by(\"owner\")",
        "return E.query().aggregate(n=count())",
        "return E.query().aggregate(n=agg.count(\"id\"))",
        "return E.query().aggregate(n=agg.sum())",
        "return E.query().aggregate(n=agg.sum(\"missing\"))",
        "return E.query().aggregate(n=agg.sum(\"score\", 1))",
        "return E.query().aggregate(n=agg.sum(field=\"score\"))",
        "return E.query().aggregate(n=agg.unknown(\"score\"))",
        "return E.query().aggregate(**values)",
        "return E.query().group_by(\"owner\", owner=agg.count())",
        "return E.query().group_by(\"owner\", \"owner\", n=agg.count())",
        "return E.query().select(\"title\").aggregate(n=agg.sum(\"score\"))",
        "return E.query().select(\"title\").group_by(\"owner\", n=agg.count())",
        "return E.query().group_by(\"owner\", n=agg.count()).aggregate(n=agg.sum(\"score\"))",
        "return E.query().distinct(\"owner\", \"owner\")",
        "return E.query().distinct(\"missing\")",
        "return E.query().select(\"title\").distinct(\"owner\")",
        "return E.query().distinct(keys=\"owner\")",
        "agg = E.query()\nreturn agg",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity))
                .await
                .is_err(),
            "accepted {body}"
        );
    }
}

#[tokio::test]
async fn python_reduction_schema_preserves_derived_key_and_value_types() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let body = "groups = E.query().group_by(\"owner\", n=agg.count())\nregrouped = groups.group_by(\"n\", original=agg.first(\"n\"))\nreturn regrouped";
    let bundle = compile_python_program(&es, &program(body, &entity))
        .await
        .unwrap();
    let comp = serde_json::to_value(&bundle.artifact().comp).unwrap();
    let fields = comp["steps"]["regrouped"]["compute"]["schema"]["fields"]
        .as_array()
        .unwrap();
    assert_eq!(fields.len(), 2);
    for field in fields {
        assert_eq!(field["value_kind"], "integer", "{field}");
        assert_eq!(field["value_type"]["shape"]["field_type"], "integer");
        assert_eq!(field["value_type"]["nullable"], field["name"] == "original");
    }
}

#[tokio::test]
async fn python_text_compute_seals_code_inputs_and_rejects_hidden_dependencies() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = |expression: &str, input: &str| {
        format!(
        "class Text(Program):\n    @compute\n    def report(self, rows: list[Value[{entity}]]) -> str:\n        return {expression}\n    def build(self):\n        one = {input}\n        text = self.report(one)\n        return text\n"
    )
    };
    let input = format!("{entity}.get(\"i1\")");
    let compile = async |expression: &str| {
        compile_python_program(&es, &source(expression, &input))
            .await
            .unwrap()
    };
    let first = compile("f'item={rows[0].title}'").await;
    let changed = compile("f'changed={rows[0].title}'").await;
    assert!(!plasm_core::plasm_monad::comp_semantic_eq(
        &first.artifact().comp,
        &changed.artifact().comp
    ));
    let comp = serde_json::to_value(&first.artifact().comp).unwrap();
    assert_eq!(comp["steps"]["text"]["compute"]["op"]["kind"], "python");
    assert_eq!(comp["steps"]["text"]["compute"]["source"], "one");
    assert!(comp["bind"]["deps"]["text"]
        .as_array()
        .unwrap()
        .iter()
        .any(|dep| dep == "one"));
    for expression in [
        "f'{rows[0].absent}'",
        "f'{hidden.title}'",
        "f'{open(\"secret\")}'",
        "f'{rows[0].title:{hidden}}'",
    ] {
        assert!(
            compile_python_program(&es, &source(expression, &input))
                .await
                .is_err(),
            "accepted {expression}"
        );
    }
    for expression in [
        "f'{rows}'",
        "'{}'.format(rows[0])",
        "f'{rows[0].__class__}'",
    ] {
        assert!(compile_python_program(&es, &source(expression, &input))
            .await
            .is_ok());
    }
    let projected = format!("{entity}.get(\"i1\").select(\"id\")");
    assert!(
        compile_python_program(&es, &source("f'{rows[0].title}'", &projected))
            .await
            .is_err()
    );
    let projected = format!("{entity}.get(\"i1\").select(\"title\")");
    assert!(
        compile_python_program(&es, &source("f'{rows[0].title}'", &projected))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn python_inferred_rows_check_types_and_preserve_plural_cardinality() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = |expression: &str, input: &str, result: &str| {
        format!(
        "class Text(Program):\n    @compute\n    def report(self, rows: list[Row]) -> list[str]:\n        return {expression}\n    def build(self):\n        rows = {input}\n        text = self.report(rows)\n        return {result}\n"
    )
    };
    let input = format!("{entity}.query().group_by(\"title\", n=agg.count())");
    let scalar = source("f'{rows.title}: {rows.n:04d}'", &input, "text")
        .replace("rows: list[Row]", "row: Row");
    assert!(compile_python_program(&es, &scalar).await.is_err());
    let scalar_value = format!("class Scalar(Program):\n    @compute\n    def double(self, value: int) -> int:\n        return value * 2\n    def build(self):\n        return self.double({entity}.query().select(\"score\"))\n");
    assert!(
        compile_python_program(&es, &scalar_value).await.is_err(),
        "a scalar value annotation cannot turn plural values into row-wise work"
    );
    let valid = source(
        "[f'{row.title}: {row.n:04d}' for row in rows]",
        &input,
        "text",
    );
    let compiled = compile_python_program(&es, &valid).await.unwrap();
    let comp = serde_json::to_value(&compiled.artifact().comp).unwrap();
    assert_eq!(comp["steps"]["text"]["compute"]["op"]["per_row"], false);
    for corruption in 0..5 {
        let mut wire = serde_json::to_value(&compiled.artifact().comp).unwrap();
        let steps = &mut wire["steps"];
        match corruption {
            0 => steps["text"]["compute"]["op"]["per_row"] = serde_json::json!(true),
            1 => steps["text"]["compute"]["op"]["contract_version"] = serde_json::json!(1),
            2 => steps["text"]["compute"]["op"]["catalog_hash"] = serde_json::json!("wrong"),
            3 | 4 => {
                for f in steps["text"]["compute"]["op"]["input_schema"]["fields"]
                    .as_array_mut()
                    .unwrap()
                {
                    if f["name"] == "n" {
                        f["value_kind"] = serde_json::json!("string");
                    }
                }
                if corruption == 4 {
                    for f in steps["rows"]["compute"]["schema"]["fields"]
                        .as_array_mut()
                        .unwrap()
                    {
                        if f["name"] == "n" {
                            f["value_kind"] = serde_json::json!("string");
                        }
                    }
                }
            }
            _ => unreachable!(),
        }
        let mut artifact = compiled.artifact().clone();
        artifact.comp = serde_json::from_value(wire).unwrap();
        let result =
            plasm_agent::plasm_compile::PlasmCompBundle::new(artifact).and_then(|bundle| {
                evaluate_plasm_comp_dry(&es, &bundle)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
        assert!(
            result.is_err(),
            "accepted row contract corruption {corruption}"
        );
    }
    let fields = comp["steps"]["text"]["compute"]["op"]["input_schema"]["fields"]
        .as_array()
        .unwrap();
    assert!(fields
        .iter()
        .any(|f| f["name"] == "n" && f["value_kind"] == "integer"));
    for expression in [
        "row.n.strip()",
        "f'{row.absent}'",
        "f'{hidden}'",
        "open('secret')",
    ] {
        assert!(
            compile_python_program(&es, &source(expression, &input, "text"))
                .await
                .is_err(),
            "accepted {expression}"
        );
    }
    assert!(
        compile_python_program(&es, &source("[str(row.n) for row in rows]", &input, "text"))
            .await
            .is_ok(),
        "plural compute values can be returned without scalar extraction"
    );
    assert!(
        compile_python_program(&es, &source("[str(row) for row in rows]", &input, "text"))
            .await
            .is_ok()
    );
    let single = format!("{entity}.query().aggregate(n=agg.count())");
    assert!(
        compile_python_program(
            &es,
            &source("[str(row.n) for row in rows]", &single, "text")
        )
        .await
        .is_err(),
        "singleton input must reject a collection annotation"
    );
    let singleton_source = source("f'{row.n}'", &single, "text")
        .replace("rows: list[Row]", "row: Row")
        .replace("-> list[str]", "-> str");
    assert!(compile_python_program(&es, &singleton_source).await.is_ok());
    let projected = format!("{entity}.query().take(2).select(renamed=\"title\")");
    assert!(compile_python_program(
        &es,
        &source("[row.renamed for row in rows]", &projected, "text")
    )
    .await
    .is_ok());
    assert!(compile_python_program(
        &es,
        &source("[row.title for row in rows]", &projected, "text")
    )
    .await
    .is_err());
}

#[tokio::test]
async fn python_plasm_dag_prompt_examples_compile_against_the_matrix() {
    let prompt = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../plasm-core/src/prompt_render/assets/python-plasm-dag.txt"
    ));
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let root = prompt
        .lines()
        .find(|line| line.starts_with("class ") && line.ends_with("(Program):"))
        .expect("complete root example");
    let start = prompt.find(root).expect("root source offset");
    let source = prompt[start..]
        .lines()
        .take_while(|line| *line == root || line.starts_with(' ') || line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .replace("e1", &entity);
    assert!(source.contains("def build(self):") && !source.contains("```"));
    compile_python_program(&es, &source)
        .await
        .expect("plain-source teaching example");
}

#[path = "python_value_contract.rs"]
pub(super) mod value_contract_matrix;

#[tokio::test]
async fn python_teaching_card_signatures_compile_against_fixture() {
    use plasm_agent::execute_session::ExecuteSession;
    use plasm_core::prompt_render::python::{prepare_python_teaching_wave, PythonTeachingState};
    use std::sync::Arc;
    let cgs = Arc::new(
        plasm_core::loader::load_schema_dir(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/schemas/python_dag_slice"),
        )
        .unwrap(),
    );
    let exposure = plasm_core::TeachingExposureSession::new(&cgs, "fixture", &["Item", "Tag"]);
    let wave = prepare_python_teaching_wave(&exposure, &PythonTeachingState::default()).unwrap();
    assert_eq!(wave.capabilities.len(), cgs.capabilities.len());
    assert!(wave.capabilities.iter().all(|c| c.unavailable.is_none()));
    let symbols = exposure.to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let tag = symbols.entity_sym_for("fixture", "Tag");
    let touch = symbols.method_sym_for("fixture", "Item", "item_touch");
    let publish = symbols.method_sym_for("fixture", "Item", "item_publish");
    let relation = symbols
        .exposed_relation_symbol_rows()
        .into_iter()
        .find(|r| r.entity == "Item" && r.wire == "tags")
        .unwrap()
        .symbol;
    let contexts = indexmap::IndexMap::from([(
        "fixture".into(),
        Arc::new(plasm_core::CgsContext::entry("fixture", cgs.clone())),
    )]);
    let es = ExecuteSession::new(
        "teaching".into(),
        String::new(),
        cgs.clone(),
        contexts,
        "fixture".into(),
        String::new(),
        String::new(),
        None,
        vec!["Item".into(), "Tag".into()],
        Some(exposure),
        None,
        cgs.catalog_cgs_hash_hex(),
        None,
    );
    let witnesses = [
        (
            &format!("{item}.get(identity:"),
            format!("{item}.get('i0')"),
        ),
        (&format!("{item}.query() ->"), format!("{item}.query()")),
        (
            &format!("{tag}.query(item_id:"),
            format!("{tag}.query(item_id='i0')"),
        ),
        (&format!("{item}.{touch}()"), format!("{item}.{touch}()")),
        (
            &format!("{item}.{publish}(content:"),
            format!("{item}.{publish}(content='document')"),
        ),
        (
            &format!("{relation}: Many[{tag}]"),
            format!("{item}.get('i0').{relation}"),
        ),
    ];
    for (declaration, expression) in witnesses {
        assert!(
            wave.declarations.contains(declaration),
            "missing {declaration}"
        );
        let source =
            format!("class Example(Program):\n    def build(self):\n        return {expression}\n");
        compile_python_program(&es, &source)
            .await
            .unwrap_or_else(|e| panic!("advertised {expression}: {e}"));
    }
}

#[path = "python_union.rs"]
pub(super) mod union_matrix;

#[tokio::test]
async fn python_signed_literals_preserve_numeric_domains() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let compile = async |body: &str| {
        compile_python_program(
            &es,
            &program(
                &write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID),
                &entity,
            ),
        )
        .await
    };
    for value in [
        "-7",
        "+7",
        "-9223372036854775808",
        "+9223372036854775807",
        "-True",
        "+False",
    ] {
        for body in [
            format!("return E.CREATE(title=\"signed\", owner=\"bot\", score={value})"),
            format!(
                "return E.query().where(lambda row: row.score is not None and row.score > {value})"
            ),
        ] {
            compile(&body)
                .await
                .unwrap_or_else(|e| panic!("rejected {body}: {e}"));
        }
    }
    for value in [
        "-9223372036854775809",
        "+9223372036854775808",
        "-1e999",
        "+1e999",
        "-1j",
    ] {
        let body = format!("return E.CREATE(title=\"signed\", owner=\"bot\", score={value})");
        assert!(compile(&body).await.is_err(), "accepted {body}");
    }
}

/// Compile an abstract fixture program after assigning this session's opaque symbols.
pub(super) async fn compile_fixture(
    es: &plasm_agent::execute_session::ExecuteSession,
    body: &str,
) -> Result<plasm_agent::PlasmCompBundle, String> {
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    compile_python_program(
        es,
        &program(
            &write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID),
            &entity,
        ),
    )
    .await
}

/// Use the same catalog/entity context for registry and live witnesses.
pub(super) fn witness_entity(
    case: &Case,
    symbols: &plasm_core::symbol_tuning::SymbolMap,
) -> String {
    symbols.entity_sym_for(
        if case
            .existing
            .and_then(find_row)
            .is_some_and(|r| r.federated)
        {
            language_matrix::MATRIX_FED_A
        } else {
            language_matrix::MATRIX_ENTRY_ID
        },
        if case.id.starts_with("iterate_") {
            "LangCursor"
        } else if case.id.starts_with("compound_") {
            "CompoundBranch"
        } else {
            "LangItem"
        },
    )
}

#[tokio::test]
async fn python_record_values_preserve_domains_and_seal_dependencies() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = format!(
        r#"class Compare(Program):
    @compute
    def text(self, row: Row) -> str:
        return str(row.score)
    def build(self):
        item = {entity}.get("i1")
        direct = self.text(item.select("id", "score", "status", "recorded_at"))
        record = {{"id": item.id, "score": item.score, "status": item.status, "recorded_at": item.recorded_at}}
        copied = self.text(record)
        return direct, copied
"#
    );
    let compiled = compile_python_program(&es, &source).await.unwrap();
    let wire = serde_json::to_value(&compiled.artifact().comp).unwrap();
    let fields = |label: &str| {
        wire["steps"][label]["compute"]["op"]["input_schema"]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| {
                (
                    field["name"].as_str().unwrap().to_owned(),
                    field["value_type"].clone(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        fields("direct"),
        fields("copied"),
        "construction erased field contracts"
    );
    assert!(
        fields("copied")
            .values()
            .all(|contract| !contract["domain"].is_null()),
        "lost CGS domains"
    );
    let mut corrupted = wire.clone();
    corrupted["steps"]["copied"]["compute"]["op"]["input_schema"]["fields"][0]["value_type"]
        ["nullable"] = serde_json::json!(true);
    let mut artifact = compiled.artifact().clone();
    artifact.comp = serde_json::from_value(corrupted).unwrap();
    let result = plasm_agent::plasm_compile::PlasmCompBundle::new(artifact).and_then(|bundle| {
        evaluate_plasm_comp_dry(&es, &bundle)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
    assert!(result.is_err(), "accepted forged record input contract");
    let changed = compile_python_program(&es, &source.replace("item.score,", "item.id,")).await;
    assert!(
        changed.is_err()
            || !plasm_core::plasm_monad::comp_semantic_eq(
                &compiled.artifact().comp,
                &changed.unwrap().artifact().comp
            ),
        "record dependency change was absent from the reviewed plan"
    );
}

#[tokio::test]
async fn python_recursive_values_matrix() {
    run_python_cases(cases().filter(|c| c.id.starts_with("value_recursive_"))).await;
}

#[tokio::test]
async fn python_recursive_values_keep_lazy_effect_and_authority_boundaries() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    for (body, diagnostic) in [
        (
            "return {'v': 1 if True else E.CREATE(title='must-not-run')}",
            "lazy value expressions cannot introduce effects",
        ),
        ("return {'v': E.query().score + 1}", "singleton"),
        ("return {'v': 1 in 2}", "unsupported-operator"),
        ("return {'v': [1] < 2}", "unsupported-operator"),
        (
            "row = {'id': 'i1'}\nreturn row.UPDATE(title='forged')",
            "receiver",
        ),
    ] {
        let error = compile_fixture(&es, body).await.expect_err(body);
        assert!(
            error.contains(diagnostic),
            "{body}: expected {diagnostic}: {error}"
        );
    }
}

#[tokio::test]
async fn python_value_closure_matrix() {
    run_python_cases(cases().filter(|c| c.id.starts_with("value_closure_"))).await;
}

#[tokio::test]
async fn python_flat_map_delete_then_create_is_an_ordered_effect_collection() {
    let case = Case {
        id: "ordered_delete_create_effects",
        python: "return E.get('i1').flat_map(lambda row: [row.DELETE(), E.CREATE(title='replacement', score=7, owner='alice')])",
        existing: None,
        expect_live_error: None,
    };
    let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
    let (es, host) = parity_context(&case, &base);
    let bundle = compile_fixture(&es, case.python)
        .await
        .unwrap_or_else(|error| panic!("{}: {error}", case.python));
    use plasm_core::plasm_monad::{PlasmStepPayload, SurfaceKind};
    let scope = bundle
        .artifact()
        .comp
        .steps
        .values()
        .find_map(|step| match step {
            PlasmStepPayload::MapBody(body) => Some(body.as_ref()),
            _ => None,
        })
        .expect("correlated body");
    let writes = scope
        .body
        .bind
        .topo
        .iter()
        .filter_map(|id| match scope.body.steps.get(id.as_str()) {
            Some(PlasmStepPayload::Invoke(operation))
                if matches!(
                    operation.plan_kind,
                    SurfaceKind::Delete | SurfaceKind::Create
                ) =>
            {
                Some((id, operation.plan_kind))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(writes.len(), 2, "one delete and one create in the scope");
    assert_eq!(writes[0].1, SurfaceKind::Delete);
    assert_eq!(writes[1].1, SurfaceKind::Create);
    assert!(
        scope
            .body
            .bind
            .deps
            .get(writes[1].0)
            .is_some_and(|deps| deps.contains(writes[0].0)),
        "create must depend on the preceding delete"
    );
    let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
    assert_comp_witness(&dry).unwrap();
    let run = Box::pin(run_plasm_comp(
        &es,
        &host,
        &es.prompt_hash,
        "python-matrix-ordered-delete-create",
        &bundle,
        true,
        None,
        None,
        Some(dry),
        None,
    ))
    .await
    .unwrap();
    let result = &run.return_steps[0].result;
    assert!(
        result.entities().is_empty(),
        "an effect list emits no data rows"
    );
    assert_eq!(
        result
            .operations
            .entries()
            .iter()
            .map(|ack| ack.completed)
            .sum::<usize>(),
        2,
        "both ordered writes must dispatch"
    );
}

#[tokio::test]
async fn python_flat_map_effect_collection_rejects_read_values() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    for source in [
        "return E.get('i1').flat_map(lambda row: [row.DELETE(), E.get('i2')])",
        "return E.get('i1').flat_map(lambda row: [E.CREATE(title='replacement', score=7, owner='alice'), E.get('i2')])",
    ] {
        let error = compile_fixture(&es, source).await.unwrap_err();
        assert!(
            error.contains("a callback result cannot mix effects and values"),
            "{source}: {error}"
        );
    }
}

#[tokio::test]
async fn python_value_closure_rejects_ambiguous_and_forged_inputs() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    for (argument, diagnostic) in [
        (
            "E.get('i1').select('title', 'score')",
            "exactly one value column",
        ),
        ("'wrong'", "input differs from its annotation"),
        ("{'a': 1, 'b': 2}", "exactly one value column"),
    ] {
        let source = format!("class Invalid(Program):\n    @compute\n    def calc(self, value: int) -> int:\n        return value + 1\n    def build(self):\n        return self.calc({argument})\n");
        let error = compile_fixture(&es, &source).await.unwrap_err();
        assert!(error.contains(diagnostic), "{argument}: {error}");
    }
    for source in [
        "record = {'header': {'n': 1}}\nreturn record.header.absent",
        "record = {'header': {'id': 'i1'}}\nreturn record.header.UPDATE(title='forged')",
    ] {
        assert!(compile_fixture(&es, source).await.is_err(), "{source}");
    }
    let case = cases().find(|c| c.id == "value_closure_literal").unwrap();
    let compiled = compile_fixture(&es, case.python).await.unwrap();
    let wire = serde_json::to_value(&compiled.artifact().comp).unwrap();
    for (key, replacement) in [
        ("per_row", serde_json::json!(false)),
        ("contract_version", serde_json::json!(6)),
        ("catalog_hash", serde_json::json!("forged")),
        ("entity", serde_json::json!("LangItem")),
        (
            "input_schema",
            serde_json::json!({"fields": [{"name": "value", "value_kind": "integer", "value_type": null}]}),
        ),
    ] {
        let mut corrupted = wire.clone();
        let op = corrupted["steps"]
            .as_object_mut()
            .unwrap()
            .values_mut()
            .find_map(|step| step.get_mut("compute").and_then(|c| c.get_mut("op")))
            .unwrap();
        op[key] = replacement;
        let mut artifact = compiled.artifact().clone();
        artifact.comp = serde_json::from_value(corrupted).unwrap();
        let result =
            plasm_agent::plasm_compile::PlasmCompBundle::new(artifact).and_then(|bundle| {
                evaluate_plasm_comp_dry(&es, &bundle)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
        assert!(result.is_err(), "accepted forged {key}");
    }
}

#[tokio::test]
async fn python_expression_semantics_are_not_a_plasm_whitelist() {
    let source = "class Semantics(Program):\n    def build(self):\n        return {'ordered': [1] < [2], 'negative_bool': -True, 'positive_bool': +False, 'truthy': 7 if 'yes' else 0, 'any_text': any(x for x in ['', 'yes']), 'all_text': all(x for x in ['yes', '']), 'mixed_equal': 1 == '1', 'string': str(123)}\n";
    assert_eq!(
        super::datetime::run(source).await.unwrap(),
        serde_json::json!({"ordered":true,"negative_bool":-1,"positive_bool":0,"truthy":7,"any_text":true,"all_text":false,"mixed_equal":false,"string":"123"})
    );
}

#[tokio::test]
async fn python_scoped_parameters_shadow_without_changing_effects_or_relations() {
    const SHADOW_CASES: &[Case] = &[
        Case {
            id: "fanout_delete",
            python: "rows = E.query().take(2)\nreturn rows.flat_map(lambda rows: rows.DELETE())",
            existing: None,
            expect_live_error: None,
        },
        Case {
            id: "relation_relation_many_from_plural_query",
            python: "items = E.query().take(2)\nreturn items.flat_map(lambda items: items.tags)",
            existing: Some("lang_relation_many_from_plural_query"),
            expect_live_error: None,
        },
    ];
    run_python_cases(SHADOW_CASES.iter()).await;
}

#[tokio::test]
async fn python_membership_captures_projected_fanout_rows() {
    for case_id in ["inline_membership", "cert_federated_e1_query"] {
        let base = hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
        let case = cases().find(|c| c.id == case_id).unwrap();
        let (es, host) = parity_context(case, &base);
        let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
        let entity = witness_entity(case, &symbols);
        let search_entity = if case_id.starts_with("cert_federated") {
            "e2"
        } else {
            &entity
        };
        let source = program(&format!("left = E.query()\nright = E.query()\nrows = left.union(right).where(lambda r: r.owner is not None)\nchecks = rows.flat_map(lambda r: {search_entity}.search(q=r.title), max_parents=256)\nvalues = checks.select(\"owner\")\nreturn rows.where(lambda r: r.owner not in values)"), &entity);
        let bundle = compile_python_program(&es, &source).await.unwrap();
        let result = plasm_agent::plasm_plan_run::run_plasm_comp_python(
            &es,
            &host,
            &es.prompt_hash,
            "fanout-membership",
            &bundle,
            true,
            None,
            None,
            None,
            None,
        )
        .await;
        let run = result.unwrap_or_else(|e| panic!("{case_id}: {}", e.diagnostic()));
        assert_eq!(
            outputs(&run),
            vec![Vec::<serde_json::Value>::new()],
            "{case_id}"
        );
    }
}

#[tokio::test]
async fn python_runtime_failure_repair_matrix() {
    run_python_cases(
        CASES
            .iter()
            .filter(|case| case.id.starts_with("repair_runtime_")),
    )
    .await;
}

/// Recursive input declarations must survive stateful producers and embedded values.
#[tokio::test]
async fn row_engine_input_contracts_cover_stateful_and_embedded_sources() {
    let witnesses = [
        "predicate_null_test",
        "predicate_truth_iteration",
        "predicate_iteration_relation",
        "render_parity_lang_render_relation_shape",
        "complete_for_each_update",
    ];
    let selected: Vec<_> = cases()
        .filter(|case| witnesses.contains(&case.id))
        .collect();
    assert_eq!(selected.len(), witnesses.len());
    run_python_cases(selected.into_iter()).await;
}

#[tokio::test]
async fn projection_alias_topk_uses_source_contract() {
    run_python_cases(cases().filter(|case| case.id == "projection_alias_topk_source_contract"))
        .await;
}

#[tokio::test]
async fn prefix_and_record_access_regressions() {
    run_python_cases(cases().filter(|case| {
        matches!(
            case.id,
            "prefix_serial_limit"
                | "prefix_union_single_relation"
                | "prefix_zero_rows"
                | "prefix_zero_count"
                | "prefix_zero_extract"
                | "record_literal_index"
        )
    }))
    .await;
}

#[tokio::test]
async fn python_nominal_boolean_refinement_selects_only_matching_effects() {
    use axum::{
        extract::Path,
        routing::{get, post},
        Json, Router,
    };
    use std::sync::Mutex;
    // Observe real HTTP writes independently of plan receipts or result shaping.
    let writes = Arc::new(Mutex::new(Vec::<String>::new()));
    let captured = writes.clone();
    let app = Router::new()
        .route(
            "/language/v1/items",
            get(|| async {
                Json(serde_json::json!([
                    {"id":"yes", "title":"Selected", "active":true},
                    {"id":"no", "title":"Excluded", "active":false}
                ]))
            }),
        )
        .route(
            "/language/v1/items/{id}",
            get(|Path(id): Path<String>| async move {
                Json(serde_json::json!({"id":id, "title":"Item", "active":id == "yes"}))
            }),
        )
        .route(
            "/language/v1/items/{id}/ping",
            post(move |Path(id): Path<String>| {
                let captured = captured.clone();
                async move {
                    captured.lock().unwrap().push(id.clone());
                    Json(serde_json::json!({"ok":true, "id":id}))
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    for (predicate, expected) in [
        ("r.active is True", "yes"),
        ("r.active is not False", "yes"),
        ("r.active is False", "no"),
        ("r.active is not True", "no"),
        ("r.active", "yes"),
    ] {
        for body in [
            format!("selected = E.query().where(lambda r: {predicate})\nreturn selected.flat_map(lambda r: r.PING())"),
            format!("return E.query().flat_map(lambda r: r.PING() if {predicate} else None)"),
        ] {
        writes.lock().unwrap().clear();
        let case = Case {
            id: "nominal_boolean_effects",
            python: "",
            existing: None,
            expect_live_error: None,
        };
        let (es, host) = parity_context(&case, &base);

        let bundle = compile_fixture(&es, &body)
            .await
            .unwrap_or_else(|e| panic!("{predicate}: {e}"));
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        assert!(
            writes.lock().unwrap().is_empty(),
            "planning dispatched an effect"
        );
        Box::pin(run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            "nominal-boolean-effects",
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        ))
        .await
        .unwrap();
        assert_eq!(
            *writes.lock().unwrap(),
            vec![expected.to_string()],
            "{predicate}"
        );
    }
    }
    server.abort();
}

#[tokio::test]
async fn python_compute_independent_inputs_compile() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let source = "class Inputs(Program):\n    def build(self):\n        return self.count(E.query().take(2), E.get('i1'))\n    @compute\n    def count(self, left: list[Row], right: Row) -> int:\n        return len(left) + len([right])\n";
    compile_fixture(&es, source).await.unwrap();
}

#[tokio::test]
async fn python_compute_dictionary_and_multiple_inputs_live() {
    use axum::{extract::Path, routing::get, Json, Router};
    let app = Router::new()
        .route(
            "/language/v1/items",
            get(|| async {
                Json(serde_json::json!([
                    {"id":"i1", "title":"one"}, {"id":"i2", "title":"two"}
                ]))
            }),
        )
        .route(
            "/language/v1/items/{id}",
            get(|Path(id): Path<String>| async move {
                Json(serde_json::json!({"id":id, "title":if id == "i1" { "one" } else { "two" }}))
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let cases = [
        ("class P(Program):\n    def build(self):\n        return self.extract(E.query())\n    @compute\n    def extract(self, rows: list[Row]) -> list[dict]:\n        out = []\n        for row in rows:\n            out.append({'name': row.title})\n        return out\n", serde_json::json!([{"name":"one"},{"name":"two"}])),
        ("class P(Program):\n    def build(self):\n        return self.count(E.query(), E.get('i1'))\n    @compute\n    def count(self, left: list[Row], right: Row) -> dict:\n        out = {}\n        out['left'] = len(left)\n        out['right'] = len([right])\n        return out\n", serde_json::json!({"left":2,"right":1})),
        ("class P(Program):\n    def build(self):\n        one = E.get('i1')\n        return self.count(one, one)\n    @compute\n    def count(self, row: Row, other: Row):\n        n = len([other])\n        return {'name': row.title, 'count': n}\n", serde_json::json!({"name":"one","count":1})),
        ("class P(Program):\n    def build(self):\n        value = self.make(E.query())\n        return self.read(value)\n    @compute\n    def make(self, rows: list[Row]):\n        result = {}\n        for row in rows:\n            result[str(row.id)] = str(row.title)\n        return result\n    @compute\n    def read(self, value: dict[str, str]) -> str:\n        return value['i1']\n", serde_json::json!("one")),
        ("class P(Program):\n    def build(self):\n        return self.add(True)\n    @compute\n    def add(self, value: int) -> str:\n        return str(value)\n", serde_json::json!("True")),
        ("class P(Program):\n    def build(self):\n        return self.pair(E.get('i1'))\n    @compute\n    def pair(self, row: Row):\n        return row.id, row.title\n", serde_json::json!(["i1", "one"])),
        ("class P(Program):\n    def build(self):\n        item = E.get(\"i1\")\n        first = item.title\n        item = E.get(\"i2\")\n        return f\"{first}/{item.title}\"\n", serde_json::json!("one/two")),
        ("class P(Program):\n    def build(self):\n        return \"done\"\n        missing()\n", serde_json::json!("done")),
        ("class P(Program):\n    def build(self):\n        def title(row: Row, /):\n            return {\"name\": row.title}\n        return self.names(E.query().map(title, max_parents=2))\n    @compute\n    def names(self, rows: list[Row]) -> str:\n        return \",\".join(str(row.name) for row in rows)\n", serde_json::json!("one,two")),
        ("class P(Program):\n    def build(self):\n        return E.query().take(1).select(n=lambda row: sum(row for row in [1, 2])).n\n", serde_json::json!(3)),
    ];
    for (index, (source, expected)) in cases.iter().enumerate() {
        let case = Case {
            id: "compute_dictionary_inputs",
            python: "",
            existing: None,
            expect_live_error: None,
        };
        let (es, host) = parity_context(&case, &base);
        let bundle = compile_fixture(&es, source)
            .await
            .unwrap_or_else(|e| panic!("case {index}: {e:?}"));
        // Roundtrip and independent dry validation exercise the serialized contracts.
        let encoded = serde_json::to_value(&bundle.artifact().comp).unwrap();
        let mut artifact = bundle.artifact().clone();
        artifact.comp = serde_json::from_value(encoded).unwrap();
        let bundle = plasm_agent::plasm_compile::PlasmCompBundle::new(artifact).unwrap();
        let dry = evaluate_plasm_comp_dry(&es, &bundle).unwrap();
        let run = Box::pin(run_plasm_comp(
            &es,
            &host,
            &es.prompt_hash,
            &format!("compute-inputs-{index}"),
            &bundle,
            true,
            None,
            None,
            Some(dry),
            None,
        ))
        .await
        .unwrap_or_else(|e| panic!("case {index}: {e:?}"));
        assert_eq!(outputs(&run)[0][0]["value"], *expected, "case {index}");
    }
    server.abort();
}
