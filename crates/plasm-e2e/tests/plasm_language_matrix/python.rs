//! Differential semantic cases. Python parses independently; both execute the canonical comp.
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
    pub(super) plasm: &'static str,
    pub(super) python: &'static str,
    pub(super) existing: Option<&'static str>,
    pub(super) expect_live_error: Option<&'static str>,
}
pub(super) const CASES: &[Case] = &[
Case { id: "type_projected_integer", plasm: "items = LangItem(\"i1\") | select score\nreport = items => <<TEXT\n{{ score }}\nTEXT\nreport", python: "class TypedText(Program):\n    @compute\n    def format_row(self, row: Row) -> str:\n        return str(row.score)\n    def build(self):\n        items = E.get(\"i1\").select(\"score\")\n        report = self.format_row(items)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "type_projected_enum", plasm: "items = LangItem(\"i1\") | select status\nreport = items => <<TEXT\n{{ status }}\nTEXT\nreport", python: "class TypedText(Program):\n    @compute\n    def format_row(self, row: Row) -> str:\n        return str(row.status)\n    def build(self):\n        items = E.get(\"i1\").select(\"status\")\n        report = self.format_row(items)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "type_projected_temporal", plasm: "items = LangItem(\"i1\") | select recorded_at\nreport = items => <<TEXT\n{{ recorded_at }}\nTEXT\nreport", python: "class TypedText(Program):\n    @compute\n    def format_row(self, row: Row) -> str:\n        return str(row.recorded_at)\n    def build(self):\n        items = E.get(\"i1\").select(\"recorded_at\")\n        report = self.format_row(items)\n        return report\n", existing: None, expect_live_error: None },

Case { id: "text_projected_alias", plasm: "", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.renamed.split(\"1\")[0]\n\n    def build(self):\n        items = E.get(\"i1\").select(renamed=\"id\")\n        out = self.text(items)\n        return out\n", existing: Some("lang_render_projected_shape"), expect_live_error: None },
Case { id: "text_derived_alias", plasm: "", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"value={row.renamed}\"\n\n    def build(self):\n        items = E.get(\"i1\")\n        mapped = items.select(renamed=\"id\")\n        out = self.text(mapped)\n        return out\n", existing: Some("lang_render_derived_shape"), expect_live_error: None },
Case { id: "text_bindings_row", plasm: "", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"# {row.title}\"\n\n    def build(self):\n        items = E.get(\"i1\").select(\"id\", \"title\")\n        hdr = self.text(items)\n        return hdr\n", existing: Some("lang_bindings_render"), expect_live_error: None },
Case { id: "text_split_part", plasm: "", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return \"split_part_ok=\" + row.id.split(\"1\")[0]\n\n    def build(self):\n        items = E.get(\"i1\").select(\"id\")\n        hdr = self.text(items)\n        return hdr\n", existing: Some("lang_render_split_part"), expect_live_error: None },
Case { id: "text_row_duplicates", plasm: "items = LangItem | take 2 | select title\nrepeated = items | union items\nreport = repeated => <<TEXT\n{{ title }}\nTEXT\nreport", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return row.title\n\n    def build(self):\n        items = E.query().take(2).select(\"title\")\n        repeated = items.union(items)\n        report = self.text(repeated)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "text_empty_aggregate", plasm: "items = LangItem | where id=\"missing\"\ncounts = items | summarize n=count()\nreport = counts => <<TEXT\ncount={{ n }}\nTEXT\nreport", python: "class RowText(Program):\n    @compute\n    def text(self, row: list[Row]) -> str:\n        return f\"count={row[0].n}\"\n\n    def build(self):\n        items = E.query().where(lambda row: row.id == \"missing\")\n        counts = items.aggregate(n=agg.count())\n        report = self.text(counts)\n        return report\n", existing: None, expect_live_error: None },

Case { id: "text_per_row_zero", plasm: "", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"{row.title}\"\n\n    def build(self):\n        items = E.query().where(lambda row: row.id == \"missing\")\n        rendered = self.text(items)\n        return rendered\n", existing: Some("lang_per_row_render_zero"), expect_live_error: None },
Case { id: "text_per_row_many", plasm: "", python: "class RowText(Program):\n    @compute\n    def text(self, row: Value[ENTITY]) -> str:\n        return f\"{row.title} — {row.code}\"\n\n    def build(self):\n        items = E.query().take(2).select(\"id\", \"title\", \"code\")\n        rendered = self.text(items)\n        return rendered\n", existing: Some("lang_per_row_render_many"), expect_live_error: None },
Case { id: "text_synthetic_count", plasm: "items = LangItem | take 2\ncounts = items | summarize n=count()\nreport = counts => <<TEXT\ncount={{ n }}\nTEXT\nreport", python: "class RowText(Program):\n    @compute\n    def text(self, row: list[Row]) -> str:\n        return f\"count={row[0].n}\"\n\n    def build(self):\n        items = E.query().take(2)\n        counts = items.aggregate(n=agg.count())\n        report = self.text(counts)\n        return report\n", existing: None, expect_live_error: None },
Case { id: "text_synthetic_group", plasm: "items = LangItem | take 2\ncounts = items | summarize by title n=count()\nreport = counts => <<TEXT\n{{ title }}: {{ n }}\nTEXT\nreport", python: "class RowText(Program):\n    @compute\n    def text(self, row: Row) -> str:\n        return f\"{row.title}: {row.n}\"\n\n    def build(self):\n        items = E.query().take(2)\n        counts = items.group_by(\"title\", n=agg.count())\n        report = self.text(counts)\n        return report\n", existing: None, expect_live_error: None },

Case { id: "text_multiline_whitespace", plasm: "report = <<TEXT\nHeader \\n\n            preserved\nid=i1\n尾\nTEXT\nreport", python: "class Whitespace(Program):\n    @compute\n    def text(self, rows: list[Value[ENTITY]]) -> str:\n        return rf\"\"\"Header \\n\n            preserved\nid={rows[0].id}\n尾\n\"\"\"\n\n    def build(self):\n        one = E.get(\"i1\")\n        report = self.text(one)\n        return report.content\n", existing: None, expect_live_error: None },
Case { id: "text_write_reuse", plasm: "one = LangItem(\"i1\")\nhdr = one => <<TEXT\nrendered {{ id }}\nTEXT\nchanged = one.update(title=hdr.content, score=7, owner=\"alice\")\nchanged | select title", python: "class WriteText(Program):\n    @compute\n    def text(self, rows: list[Value[ENTITY]]) -> str:\n        return f\"rendered {rows[0].id}\"\n\n    def build(self):\n        one = E.get(\"i1\")\n        report = self.text(one)\n        changed = one.UPDATE(title=report.content, score=7, owner=\"alice\")\n        return changed.select(\"title\")\n", existing: None, expect_live_error: None },
Case { id: "text_collection_report", plasm: "", python: "class TextReport(Program):\n    @compute\n    def format_rows(self, rows: list[Value[ENTITY]]) -> str:\n        return \"\".join(f\"\\n- {row.title}\\n\" for row in rows)\n\n    def build(self):\n        items = E.query().take(2).select(\"id\", \"title\")\n        report = self.format_rows(items)\n        return report.content\n", existing: Some("lang_plain_template_foreach"), expect_live_error: None },
Case { id: "text_literal_binding", plasm: "", python: "class LiteralText(Program):\n    def build(self):\n        note = \"\"\"hello-matrix\n\"\"\"\n        one = E.query().take(1).select(\"title\")\n        return one, note\n", existing: Some("lang_heredoc_binding"), expect_live_error: None },
Case { id: "text_literal_equals", plasm: "", python: "class LiteralText(Program):\n    def build(self):\n        body = \"\"\"key = value\n\"\"\"\n        one = E.query().take(1).select(\"title\")\n        return one, body\n", existing: Some("lang_heredoc_body_with_equals"), expect_live_error: None },
Case { id: "reduce_lang_aggregate", plasm: "", python: "return E.query().aggregate(n=agg.count())", existing: Some("lang_aggregate"), expect_live_error: None },
Case { id: "reduce_lang_aggregate_sugar_count", plasm: "", python: "return E.query().aggregate(count=agg.count())", existing: Some("lang_aggregate_sugar_count"), expect_live_error: None },
Case { id: "reduce_lang_aggregate_sum", plasm: "", python: "return E.query().aggregate(t=agg.sum(\"score\"))", existing: Some("lang_aggregate_sum"), expect_live_error: None },
Case { id: "reduce_lang_group_by", plasm: "", python: "return E.query().group_by(\"owner\", n=agg.count())", existing: Some("lang_group_by"), expect_live_error: None },
Case { id: "reduce_lang_group_by_aggregate_chain", plasm: "", python: "return E.query().group_by(\"owner\", \"score\", n=agg.count(), title=agg.first(\"title\"))", existing: Some("lang_group_by_aggregate_chain"), expect_live_error: None },
Case { id: "reduce_lang_group_by_sugar", plasm: "", python: "return E.query().group_by(\"owner\", count=agg.count())", existing: Some("lang_group_by_sugar"), expect_live_error: None },
Case { id: "reduce_lang_group_by_multi", plasm: "", python: "return E.query().group_by(\"owner\", \"score\", n=agg.count())", existing: Some("lang_group_by_multi"), expect_live_error: None },
Case { id: "reduce_lang_group_by_first", plasm: "", python: "return E.query().group_by(\"owner\", title=agg.first(\"title\"))", existing: Some("lang_group_by_first"), expect_live_error: None },
Case { id: "reduce_lang_group_by_then_sort_agg_column", plasm: "", python: "return E.query().group_by(\"owner\", n=agg.count()).order_by(\"n\", descending=True)", existing: Some("lang_group_by_then_sort_agg_column"), expect_live_error: None },
Case { id: "reduce_lang_dedupe", plasm: "", python: "return E.query().distinct(\"owner\").take(20)", existing: Some("lang_dedupe"), expect_live_error: None },
Case { id: "reduce_aggregate_functions", plasm: "LangItem | summarize s=sum(score), a=avg(score), lo=min(score), hi=max(score), f=first(title), l=last(title)", python: "return E.query().aggregate(s=agg.sum(\"score\"), a=agg.avg(\"score\"), lo=agg.min(\"score\"), hi=agg.max(\"score\"), f=agg.first(\"title\"), l=agg.last(\"title\"))", existing: None, expect_live_error: None },
Case { id: "reduce_aggregate_empty", plasm: "LangItem | where id=\"missing\" | summarize n=count(), total=sum(score)", python: "return E.query().where(lambda row: row.id == \"missing\").aggregate(n=agg.count(), total=agg.sum(\"score\"))", existing: None, expect_live_error: None },
Case { id: "reduce_group_empty", plasm: "LangItem | where id=\"missing\" | summarize by owner n=count()", python: "return E.query().where(lambda row: row.id == \"missing\").group_by(\"owner\", n=agg.count())", existing: None, expect_live_error: None },
Case { id: "reduce_aggregate_alias", plasm: "LangItem | select points=score | summarize total=sum(points)", python: "return E.query().select(points=\"score\").aggregate(total=agg.sum(\"points\"))", existing: None, expect_live_error: None },
Case { id: "set_lanes_group_then_global_aggregate", plasm: "", python: "return LANE.query(shelf=\"alpha\").group_by(\"shelf\", n=agg.count()).aggregate(total=agg.sum(\"n\"))", existing: Some("lang_group_then_global_aggregate"), expect_live_error: None },
Case { id: "reduce_distinct_alias", plasm: "LangItem | select handle=owner, title | distinct by handle", python: "return E.query().select(\"title\", handle=\"owner\").distinct(\"handle\")", existing: None, expect_live_error: None },
Case { id: "reduce_distinct_multi", plasm: "LangItem | distinct by owner, score", python: "return E.query().distinct(\"owner\", \"score\")", existing: None, expect_live_error: None },
Case { id: "cert_take_one_field_bind", plasm: "", python: "items = E.query()\none = items.order_by(\"id\").take(1)\nvalue = one.id\nreturn one, value", existing: Some("lang_take_one_field_bind"), expect_live_error: None },
Case { id: "cert_take_one_field_empty_bind", plasm: "", python: "items = E.query()\none = items.where(lambda row: row.id == \"missing\").take(1)\nvalue = one.id\nreturn value", existing: Some("lang_take_one_field_empty_bind"), expect_live_error: Some("zero rows") },
Case { id: "cert_take_one_field_empty_argument", plasm: "", python: "items = E.query()\none = items.where(lambda row: row.id == \"missing\").take(1)\ntarget = E.get(\"i1\")\nout = target.UPDATE(title=one.id, score=42, owner=\"alice\")\nreturn out", existing: Some("lang_take_one_field_empty_argument"), expect_live_error: Some("zero rows") },
Case { id: "cert_take_one_method_invoke_empty", plasm: "", python: "items = E.query()\none = items.where(lambda row: row.id == \"missing\").take(1)\nout = one.UPDATE(title=\"must-not-write\", score=2, owner=\"alice\")\nreturn out", existing: Some("lang_take_one_method_invoke_empty"), expect_live_error: Some("zero rows") },
Case { id: "cert_bind_singleton_field_scalar", plasm: "", python: "item = E.get(\"i1\")\ntitle = item.title\nreturn title", existing: Some("lang_bind_singleton_field_scalar"), expect_live_error: None },
Case { id: "cert_get_singleton_field_scalar", plasm: "", python: "title = E.get(\"i1\").title\nreturn title", existing: Some("lang_get_singleton_field_scalar"), expect_live_error: None },
Case { id: "cert_get_singleton_field_password", plasm: "", python: "pw = VAULT.get(\"venmo\").password\nreturn pw", existing: Some("lang_get_singleton_field_password"), expect_live_error: None },
Case { id: "cert_get_singleton_field_empty", plasm: "", python: "return VAULT.get(\"missing\").password", existing: Some("lang_get_singleton_field_empty"), expect_live_error: Some("zero rows") },
Case { id: "cert_pipe_select_row_fields", plasm: "", python: "root = E.get(\"i1\")\nreturn root.select(\"title\")", existing: Some("lang_pipe_select_row_fields"), expect_live_error: None },
Case { id: "cert_bind_limit1_continuation", plasm: "", python: "root = E.query(owner=\"alice\")\none = root.take(1)\ntags = one.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_bind_limit1_continuation"), expect_live_error: None },
Case { id: "cert_relation_opaque_r_symbol", plasm: "", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.REL_TAGS)\nreturn tags", existing: Some("lang_relation_opaque_r_symbol"), expect_live_error: None },
Case { id: "cert_required_selection_default", plasm: "", python: "return STOCK.query()", existing: Some("lang_required_selection_default"), expect_live_error: None },
Case { id: "cert_required_selection_multi", plasm: "", python: "return LANE.query(shelf=\"alpha\")", existing: Some("lang_required_selection_multi"), expect_live_error: None },
Case { id: "cert_required_selection_empty", plasm: "", python: "return LANE.query(shelf=\"empty\")", existing: Some("lang_required_selection_empty"), expect_live_error: None },
Case { id: "cert_quoted_binding_literal", plasm: "", python: "item = E.get(\"i1\")\nreturn E.query().where(lambda row: row.title == \"item\")", existing: Some("lang_quoted_binding_literal"), expect_live_error: None },
Case { id: "cert_integer_where_gt_dry_coerce", plasm: "", python: "return E.query().where(lambda row: row.score > 0)", existing: Some("lang_integer_where_gt_dry_coerce"), expect_live_error: None },
Case { id: "cert_ra4_pipe_monolith", plasm: "", python: "return E.query().where(lambda row: row.owner == \"alice\").order_by(\"title\").take(5).select(\"title\", \"owner\")", existing: Some("lang_ra4_pipe_monolith"), expect_live_error: None },
Case { id: "cert_ra4_pipe_bind_cut", plasm: "", python: "h = E.query()\nw = h.where(lambda row: row.owner == \"alice\")\no = w.order_by(\"title\")\nt = o.take(5)\nreturn t.select(\"title\", \"owner\")", existing: Some("lang_ra4_pipe_bind_cut"), expect_live_error: None },
Case { id: "cert_apply_get_multirow", plasm: "", python: "items = E.query().where(lambda row: row.owner == \"alice\").take(3)\ndetails = items.flat_map(lambda row: E.get(row.id))\nreturn details", existing: Some("lang_apply_get_multirow"), expect_live_error: None },
Case { id: "cert_apply_query_multirow", plasm: "", python: "items = E.query().where(lambda row: row.owner == \"alice\").take(2)\npeers = items.flat_map(lambda row: E.query(owner=row.owner))\nreturn peers", existing: Some("lang_apply_query_multirow"), expect_live_error: None },
Case { id: "cert_effect_action_ping", plasm: "", python: "item = E.get(\"i1\")\nreturn item.PING()", existing: Some("lang_effect_action_ping"), expect_live_error: None },
Case { id: "cert_effect_delete", plasm: "", python: "item = E.get(\"i2\")\nreturn item.DELETE()", existing: Some("lang_effect_delete"), expect_live_error: None },
Case { id: "cert_for_each_empty_ping", plasm: "", python: "items = E.query().where(lambda row: row.owner == \"no-such-matrix-owner\")\ndone = items.flat_map(lambda row: row.PING())\nreturn done", existing: Some("lang_for_each_empty_ping"), expect_live_error: None },

Case { id: "inline_membership", plasm: "", python: "kept = E.query().where(lambda row: row.owner in E.query().where(lambda row: row.owner == \"alice\").select(\"owner\"))\nreturn kept", existing: Some("lang_where_in_rowset_paren"), expect_live_error: None },
Case { id: "inline_alias_membership", plasm: "", python: "sent = E.query().where(lambda row: row.owner == \"alice\").select(email=\"owner\")\nrecv = E.query().where(lambda row: row.owner == \"bob\").select(email=\"owner\")\npeers = sent.union(recv).distinct()\nkept = E.query().where(lambda row: row.owner in peers.select(\"email\"))\nreturn kept", existing: Some("lang_union_rowset_alias_distinct"), expect_live_error: None },
Case { id: "set_lanes_inline_not_in_left", plasm: "", python: "left = LANE.query(shelf=\"alpha\")\nright = STOCK.query()\nkept = left.where(lambda row: row.title not in right.select(\"title\"))\nreturn kept", existing: Some("lang_where_not_in_universe_left"), expect_live_error: None },
Case { id: "set_lanes_inline_not_in_right", plasm: "", python: "left = STOCK.query()\nright = LANE.query(shelf=\"alpha\")\nkept = left.where(lambda row: row.title not in right.select(\"title\"))\nreturn kept", existing: Some("lang_where_not_in_universe_right"), expect_live_error: None },

Case { id: "set_lanes_union_empty_right", plasm: "", python: "kept = LANE.query(shelf=\"alpha\").select(\"title\")\nnone = LANE.query(shelf=\"empty\").select(\"title\")\npeers = kept.union(none)\nreturn peers", existing: Some("lang_union_empty_right"), expect_live_error: None },
Case { id: "set_lanes_not_in_universe_left", plasm: "", python: "left = LANE.query(shelf=\"alpha\")\nright = STOCK.query()\ntitles = right.select(\"title\")\nkept = left.where(lambda row: row.title not in titles)\nreturn kept", existing: Some("lang_where_not_in_universe_left"), expect_live_error: None },
Case { id: "set_lanes_not_in_universe_right", plasm: "", python: "left = STOCK.query()\nright = LANE.query(shelf=\"alpha\")\ntitles = right.select(\"title\")\nkept = left.where(lambda row: row.title not in titles)\nreturn kept", existing: Some("lang_where_not_in_universe_right"), expect_live_error: None },
Case { id: "union_collapses_duplicates", plasm: "one = LangItem(\"i1\") | select owner\nboth = one | union one\nboth", python: "one = E.get(\"i1\").select(\"owner\")\nreturn one.union(one)", existing: None, expect_live_error: None },
Case { id: "set_lanes_distinct_projection", plasm: "", python: "rows = LANE.query(shelf=\"alpha\").select(\"shelf\")\nunique = rows.distinct()\nreturn unique", existing: Some("lang_distinct_projected_values"), expect_live_error: None },

Case { id: "where_in_rowset", plasm: "", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nkept = E.query().where(lambda row: row.owner in alice)\nreturn kept", existing: Some("lang_where_in_rowset"), expect_live_error: None },
Case { id: "where_not_in_rowset", plasm: "", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\ndrop = E.query().where(lambda row: row.owner not in alice)\nreturn drop", existing: Some("lang_where_not_in_rowset"), expect_live_error: None },
Case { id: "where_in_rowset_paren", plasm: "", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nkept = E.query().where(lambda row: row.owner in alice)\nreturn kept", existing: Some("lang_where_in_rowset_paren"), expect_live_error: None },
Case { id: "union_rowset", plasm: "", python: "alice = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nbob = E.query().where(lambda row: row.owner == \"bob\").select(\"owner\")\npeers = alice.union(bob)\nkept = E.query().where(lambda row: row.owner in peers)\nreturn kept", existing: Some("lang_union_rowset"), expect_live_error: None },
Case { id: "union_rowset_alias", plasm: "", python: "sent = E.query().where(lambda row: row.owner == \"alice\").select(email=\"owner\")\nrecv = E.query().where(lambda row: row.owner == \"bob\").select(email=\"owner\")\npeers = sent.union(recv)\nkept = E.query().where(lambda row: row.owner in peers)\nreturn kept", existing: Some("lang_union_rowset_alias"), expect_live_error: None },
Case { id: "union_rowset_alias_distinct", plasm: "", python: "sent = E.query().where(lambda row: row.owner == \"alice\").select(email=\"owner\")\nrecv = E.query().where(lambda row: row.owner == \"bob\").select(email=\"owner\")\npeers = sent.union(recv).distinct()\nemails = peers.select(\"email\")\nkept = E.query().where(lambda row: row.owner in emails)\nreturn kept", existing: Some("lang_union_rowset_alias_distinct"), expect_live_error: None },
Case { id: "union_rowset_alias_existing", plasm: "", python: "recv = E.query().where(lambda row: row.owner == \"alice\").select(\"owner\")\nsent = E.query().where(lambda row: row.owner == \"bob\").select(\"title\")\npeers = recv.union(sent.select(owner=\"title\"))\nkept = E.query().where(lambda row: row.owner in peers)\nreturn kept", existing: Some("lang_union_rowset_alias_existing"), expect_live_error: None },

Case { id: "select_alias_where", plasm: "", python: "items = E.query()\nrenamed = items.select(\"owner\", handle=\"owner\").where(lambda row: row.handle == \"alice\")\nreturn renamed", existing: Some("lang_select_alias_where"), expect_live_error: None },
Case { id: "alias_reproject_sort", plasm: "items = LangItem\nrenamed = items | select handle = owner, title\nout = renamed | select handle | order by handle\nout", python: "items = E.query()\nrenamed = items.select(\"title\", handle=\"owner\")\nreturn renamed.select(\"handle\").order_by(\"handle\")", existing: None, expect_live_error: None },
Case { id: "alias_replace_column", plasm: "items = LangItem\nout = items | select owner = title\nout", python: "items = E.query()\nreturn items.select(owner=\"title\")", existing: None, expect_live_error: None },

Case { id: "sort_limit", plasm: "", python: "return E.query().order_by(\"score\", descending=True).take(2).select(\"id\", \"score\")", existing: Some("lang_sort_limit"), expect_live_error: None },
Case { id: "sort_asc", plasm: "", python: "return E.query().order_by(\"score\", descending=False).take(3).select(\"id\", \"score\")", existing: Some("lang_sort_asc"), expect_live_error: None },
Case { id: "program_return_pipeline_filter_sort", plasm: "items = LangItem\nfiltered = items | where owner=\"alice\"\nsorted = filtered | order by title | take 10 | select title, owner", python: "items = E.query()\nfiltered = items.where(lambda row: row.owner == \"alice\")\nsorted = filtered.order_by(\"title\").take(10).select(\"title\", \"owner\")\nreturn sorted", existing: Some("lang_program_return_pipeline_filter_sort"), expect_live_error: None },

Case { id: "relation_empty_fanout", plasm: "", python: "items = E.query().where(lambda row: row.id == \"missing\")\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_relation_empty_fanout"), expect_live_error: None },
Case { id: "relation_one_chain", plasm: "", python: "summary = E.get(\"i1\").summary\ndetail = summary.detail\nreturn detail.select(\"id\", \"body\")", existing: Some("lang_relation_one_chain"), expect_live_error: None },
Case { id: "relation_relation_lines", plasm: "", python: "lines = E.get(\"i1\").lines\nreturn lines.select(\"id\", \"note\")", existing: Some("lang_relation_lines"), expect_live_error: None },
Case { id: "relation_relation_tags_scoped", plasm: "", python: "return E.get(\"i1\").tags", existing: Some("lang_relation_tags_scoped"), expect_live_error: None },
Case { id: "relation_bind_projection_then_relation", plasm: "", python: "root = E.get(\"i1\")\ntrimmed = root.select(\"id\", \"title\")\ntags = trimmed.tags\nreturn tags", existing: Some("lang_bind_projection_then_relation"), expect_live_error: None },
Case { id: "relation_bind_relation_hop_one_one", plasm: "", python: "summary = E.get(\"i1\").summary\nreturn summary.select(\"headline\")", existing: Some("lang_bind_relation_hop_one_one"), expect_live_error: None },
Case { id: "relation_bind_filter_continuation", plasm: "", python: "root = E.query(owner=\"alice\")\nfiltered = root.where(lambda row: row.owner == \"alice\")\ntags = filtered.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_bind_filter_continuation"), expect_live_error: None },
Case { id: "relation_relation_many_from_plural_query", plasm: "", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_relation_many_from_plural_query"), expect_live_error: None },
Case { id: "relation_relation_prefer_embed_hit", plasm: "", python: "item = E.get(\"i1\")\ntags = item.tags\nreturn tags", existing: Some("lang_relation_prefer_embed_hit"), expect_live_error: None },
Case { id: "relation_relation_prefer_embed_miss", plasm: "", python: "items = E.query(owner=\"bob\").take(2)\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_relation_prefer_embed_miss"), expect_live_error: None },
Case { id: "relation_relation_integer_scoped_bindings", plasm: "", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.tags_by_score)\nreturn tags", existing: Some("lang_relation_integer_scoped_bindings"), expect_live_error: None },
Case { id: "relation_ra4_apply_relation_monolith", plasm: "", python: "return E.query().take(2).flat_map(lambda row: row.tags)", existing: Some("lang_ra4_apply_relation_monolith"), expect_live_error: None },
Case { id: "relation_ra4_apply_relation_bind_cut", plasm: "", python: "items = E.query().take(2)\ntags = items.flat_map(lambda row: row.tags)\nreturn tags", existing: Some("lang_ra4_apply_relation_bind_cut"), expect_live_error: None },
Case { id: "boolean_identity_false", plasm: "LangItem(false) | select id", python: "return E.get(False).select(\"id\")", existing: Some("lang_boolean_identity_false"), expect_live_error: None },
Case { id: "boolean_identity_true", plasm: "LangItem(true) | select id", python: "return E.get(True).select(\"id\")", existing: Some("lang_boolean_identity_true"), expect_live_error: None },
Case { id: "integer_identity", plasm: "LangItem(42) | select id", python: "return E.get(42).select(\"id\")", existing: Some("lang_integer_identity"), expect_live_error: None },
Case { id: "negative_identity", plasm: "LangItem(-42) | select id", python: "return E.get(-42).select(\"id\")", existing: Some("lang_negative_identity"), expect_live_error: None },
Case { id: "large_identity", plasm: "LangItem(9007199254740993) | select id", python: "return E.get(9007199254740993).select(\"id\")", existing: Some("lang_large_identity"), expect_live_error: None },
Case { id: "min_identity", plasm: "LangItem(-9223372036854775808) | select id", python: "return E.get(-9223372036854775808).select(\"id\")", existing: Some("lang_min_identity"), expect_live_error: None },
Case { id: "compound_literal", plasm: "out = CompoundBranch(owner=\"alice\", item_id=\"i1\", name=\"main\")\nout", python: "out = E.get(owner=\"alice\", item_id=\"i1\", name=\"main\")\nreturn out", existing: Some("lang_compound_literal"), expect_live_error: None },
Case { id: "compound_bound", plasm: "source = CompoundBranch(owner=\"alice\", item_id=\"i1\", name=\"main\")\nout = CompoundBranch(owner=source.owner, item_id=source.item_id, name=source.name)\nsource,out", python: "source = E.get(owner=\"alice\", item_id=\"i1\", name=\"main\")\nout = E.get(name=source.name, owner=source.owner, item_id=source.item_id)\nreturn source, out", existing: Some("lang_compound_bound"), expect_live_error: None },
Case { id: "bound_get_field", plasm: "source = LangItem(\"i1\")\nout = LangItem(source.id)\nsource,out", python: "source = E.get(\"i1\")\nout = E.get(source.id)\nreturn source, out", existing: Some("lang_bound_get_field"), expect_live_error: None },
Case { id: "bound_get_scalar", plasm: "source = LangItem(\"i1\")\nkey = source.id\nout = LangItem(key)\nsource,out", python: "source = E.get(\"i1\")\nkey = source.id\nout = E.get(key)\nreturn source, out", existing: Some("lang_bound_get_scalar"), expect_live_error: None },
Case { id: "bound_query_field", plasm: "source = LangItem(\"i1\")\nout = LangItem{owner=source.owner}\nsource,out", python: "source = E.get(\"i1\")\nout = E.query(owner=source.owner)\nreturn source, out", existing: Some("lang_bound_query_field"), expect_live_error: None },
Case { id: "bound_query_scalar", plasm: "source = LangItem(\"i1\")\nowner = source.owner\nout = LangItem{owner=owner}\nsource,out", python: "source = E.get(\"i1\")\nowner = source.owner\nout = E.query(owner=owner)\nreturn source, out", existing: Some("lang_bound_query_scalar"), expect_live_error: None },
Case { id: "iterate_bound_identity", plasm: "source = LangCursor(\"c1\")\nkey = source.id\ncur = LangCursor(key)\ndone = iterate cur step LangCursor(_.id).tick() until phase = \"done\" take 2\ndone", python: "source = E.get(\"c1\")\nkey = source.id\ncur = E.get(key)\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=2)\nreturn done", existing: Some("lang_iterate_bound_identity"), expect_live_error: None },
Case { id: "bound_get_empty", plasm: "source = LangItem | where id = \"missing\" | take 1\nkey = source.id\nout = LangItem(key)\nout", python: "source = E.query().where(lambda row: row.id == \"missing\").take(1)\nkey = source.id\nout = E.get(key)\nreturn out", existing: Some("lang_bound_get_empty"), expect_live_error: Some("zero rows") },
Case { id:"iterate_zero", plasm:"cur = LangCursor(\"c_done\")\ndone = iterate cur step LangCursor(_.id).tick() until phase = \"done\" take 3\ndone", python:"cur = E.get(\"c_done\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=3)\nreturn done", existing:Some("lang_iterate_until_zero_step"), expect_live_error:None },
Case { id:"iterate_exact", plasm:"cur = LangCursor(\"c1\")\ndone = iterate cur step LangCursor(_.id).tick() until phase = \"done\" take 2\ndone", python:"cur = E.get(\"c1\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=2)\nreturn done", existing:None, expect_live_error:None },
Case { id:"iterate_success", plasm:"cur = LangCursor(\"c1\")\ndone = iterate cur step LangCursor(_.id).tick() until phase = \"done\" take 4\ndone", python:"cur = E.get(\"c1\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=4)\nreturn done", existing:Some("lang_iterate_until_bound"), expect_live_error:None },
Case { id:"iterate_exhausted", plasm:"cur = LangCursor(\"c_stuck\")\ndone = iterate cur step LangCursor(_.id).tick() until phase = \"done\" take 2\ndone", python:"cur = E.get(\"c_stuck\")\ndone = cur.iterate(lambda row: row.TICK(), until=lambda row: row.phase == \"done\", max_steps=2)\nreturn done", existing:Some("lang_iterate_bound_exhausted"), expect_live_error:Some("iterate_bound_exhausted") },
    Case { id:"fanout_update", plasm:"rows = LangItem | take 2\nchanged = rows => _.update(title=_.title, score=9, owner=\"alice\")\nchanged | select title, score, owner", python:"rows = E.query().take(2)\nchanged = rows.flat_map(lambda row: row.UPDATE(title=row.title, score=9, owner=\"alice\"))\nreturn changed.select(\"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"fanout_delete", plasm:"rows = LangItem | take 2\ngone = rows => _.delete()\ngone", python:"rows = E.query().take(2)\nreturn rows.flat_map(lambda row: row.DELETE())", existing:None, expect_live_error:None },
    Case { id:"fanout_empty", plasm:"rows = LangItem | where id = \"missing\"\ngone = rows => _.delete()\ngone", python:"rows = E.query().where(lambda row: row.id == \"missing\")\nreturn rows.flat_map(lambda row: row.DELETE())", existing:None, expect_live_error:None },
    Case { id:"fanout_read", plasm:"rows = LangItem | take 2\nagain = rows => LangItem(_.id)\nagain | select title", python:"rows = E.query().take(2)\nagain = rows.flat_map(lambda row: E.get(row.id))\nreturn again.select(\"title\")", existing:None, expect_live_error:None },
    Case { id:"create", plasm:"made = LangItem.create(title=\"Matrix\", score=7, owner=\"alice\", active=true, tags=[\"x\", \"y\"])\nmade", python:"made = E.CREATE(title=\"Matrix\", score=7, owner=\"alice\", active=True, tags=[\"x\", \"y\"])\nreturn made", existing:None, expect_live_error:None },
    Case { id:"update", plasm:"one = LangItem(\"i1\")\nchanged = one.update(title=\"Changed\", score=8, owner=\"alice\")\nchanged | select id, title, score, owner", python:"one = E.get(\"i1\")\nchanged = one.UPDATE(title=\"Changed\", score=8, owner=\"alice\")\nreturn changed.select(\"id\", \"title\", \"score\", \"owner\")", existing:None, expect_live_error:None },
    Case { id:"delete", plasm:"one = LangItem(\"i1\")\ngone = one.delete()\ngone", python:"one = E.get(\"i1\")\ngone = one.DELETE()\nreturn gone", existing:None, expect_live_error:None },
    Case { id:"broadcast", plasm:"done = LangItem.broadcast(message=\"matrix\")\ndone", python:"done = E.BROADCAST(message=\"matrix\")\nreturn done", existing:None, expect_live_error:None },
    Case { id:"field_input", plasm:"one = LangItem(\"i1\")\nmade = LangItem.create(title=one.title, score=8, owner=\"alice\")\nmade", python:"one = E.get(\"i1\")\nmade = E.CREATE(title=one.title, score=8, owner=\"alice\")\nreturn made", existing:None, expect_live_error:None },
    Case { id:"scalar_input", plasm:"one = LangItem | take 1\ntext = one.title\nmade = LangItem.create(title=text, score=8, owner=\"alice\")\nmade", python:"one = E.query().take(1)\ntext = one.title\nmade = E.CREATE(title=text, score=8, owner=\"alice\")\nreturn made", existing:None, expect_live_error:None },
    Case { id:"empty_write_receiver", plasm:"one = LangItem | where id = \"missing\" | take 1\nchanged = one.update(title=\"never\")\nchanged", python:"one = E.query().where(lambda row: row.id == \"missing\").take(1)\nchanged = one.UPDATE(title=\"never\")\nreturn changed", existing:None, expect_live_error:Some("zero rows") },
    Case {id:"empty_singleton",plasm:"one = LangItem | where id = \"missing\" | take 1\nvalue = one.id\nvalue",python:"one = E.query().where(lambda row: row.id == \"missing\").take(1)\nvalue = one.id\nreturn value",existing:None,expect_live_error:Some("zero rows")},
    Case {
        id: "singleton",
        plasm: "one = LangItem | take 1\nvalue = one.id\nvalue",
        python: "one = E.query().take(1)\nvalue = one.id\nreturn value",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "query",
        plasm: "LangItem",
        python: "return E.query()",
        expect_live_error: None,
        existing: Some("lang_query_all"),
    },
    Case {
        id: "get",
        plasm: "LangItem(\"i1\")",
        python: "return E.get(\"i1\")",
        expect_live_error: None,
        existing: Some("lang_get_by_id"),
    },
    Case {
        id: "selection",
        plasm: "LangItem{owner=\"alice\"}",
        python: "return E.query(owner=\"alice\")",
        expect_live_error: None,
        existing: Some("lang_predicate_brace_owner"),
    },
    Case {
        id: "take",
        plasm: "items = LangItem\nitems | take 3",
        python: "items = E.query()\nreturn items.take(3)",
        expect_live_error: None,
        existing: Some("lang_bind_first_limit"),
    },
    Case {
        id: "projection",
        plasm: "projected = LangItem | take 1 | select id, title\nprojected",
        python: "return E.query().take(1).select(\"id\", \"title\")",
        expect_live_error: None,
        existing: Some("lang_limit_projection"),
    },
    Case {
        id: "where",
        plasm: "LangItem | where score > 10",
        python: "return E.query().where(lambda row: row.score > 10)",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "coerce",
        plasm: "LangItem | where score >= \"10\"",
        python: "return E.query().where(lambda row: row.score >= \"10\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "empty",
        plasm: "LangItem | where id = \"missing\"",
        python: "return E.query().where(lambda row: row.id == \"missing\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "grain",
        plasm: "LangItem | select title | where title = \"Alpha\"",
        python: "return E.query().select(\"title\").where(lambda row: row.title == \"Alpha\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "quoted",
        plasm: "items = LangItem\nitems | where title = \"items\"",
        python: "items = E.query()\nreturn items.where(lambda row: row.title == \"items\")",
        expect_live_error: None,
        existing: None,
    },
    Case {
        id: "parallel",
        plasm: "a = LangItem(\"i1\")\nb = LangItem(\"i2\")\na,b",
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
fn program(body: &str, entity: &str) -> String {
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
fn parity_context(
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

#[test]
fn python_comparison_keeps_domain_fields_and_embedded_identity() {
    let mut value = serde_json::json!({"_version": 42, "_last_updated": 9, "children": [{"_ref": {"entity": "Child", "kind": "simple", "id": "c1"}, "_version": 3, "_last_updated": 5, "_completeness": "summary", "name": "child"}]});
    normalize_embedded_revision_metadata(&mut value);
    assert_eq!(
        value,
        serde_json::json!({"_version": 42, "_last_updated": 9, "children": [{"_ref": {"entity": "Child", "kind": "simple", "id": "c1"}, "_completeness": "summary", "name": "child"}]})
    );
}

fn outputs(run: &PlasmPlanRunResult) -> Vec<Vec<serde_json::Value>> {
    run.return_steps
        .iter()
        .map(|step| {
            step.result
                .entities
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
#[test]
fn python_lowering_matrix_live_semantic_contract() {
    let mut failures = Vec::new();
    for case in cases() {
        let result = std::thread::Builder::new()
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        let mut results = Vec::new();
                        for python in [false, true] {
                            let base =
                                hermit_lang_matrix::fresh_python_parity_hermit_base_url().await;
                            let (es, host) = parity_context(case, &base);
                            let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
                            let entity = symbols.entity_sym_for(
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
                            );
                            let outcome = super::python_render_parity::python_outcome(case.id);
                            let compiled = if python {
                                compile_python_program(
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
                            } else {
                                compile_plasm_program(
                                    &PromptPipelineConfig::default(),
                                    None,
                                    &es,
                                    case.id,
                                    &super::python_coverage::baseline_program(case, &es),
                                )
                                .map_err(|error| error.to_string())
                            };
                            let compile_error = if python {
                                if let PythonOutcome::CompileError(message) = outcome {
                                    Some(message)
                                } else {
                                    None
                                }
                            } else {
                                case.expect_live_error
                            };
                            if let Err(error) = &compiled {
                                let expected = compile_error.unwrap_or_else(|| {
                                    panic!(
                                        "{} {} compile: {error}",
                                        case.id,
                                        if python { "Python" } else { "native" }
                                    )
                                });
                                assert!(
                                    error.contains(expected),
                                    "{} compile expected {expected:?}: {error}",
                                    case.id
                                );
                                results.push((Vec::new(), Vec::new()));
                                continue;
                            }
                            assert!(
                                !(python && matches!(outcome, PythonOutcome::CompileError(_))),
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
                                &format!("python-matrix-{}-{python}", case.id),
                                &bundle,
                                true,
                                None,
                                None,
                                Some(dry),
                                None,
                            ))
                            .await;
                            let expected_error = if python {
                                match outcome {
                                    PythonOutcome::LiveError(message) => Some(message),
                                    PythonOutcome::ExplicitQualification => None,
                                    _ => case.expect_live_error,
                                }
                            } else {
                                case.expect_live_error
                            };
                            if let Some(expected) = expected_error {
                                let error = run.expect_err(
                                    "expected execution failure after successful compilation",
                                );
                                assert!(error.contains(expected), "{}: {error}", case.id);
                                results.push((Vec::new(), Vec::new()));
                                continue;
                            }
                            let run = run.unwrap_or_else(|e| panic!("{} live: {e}", case.id));
                            if matches!(case.id, "union_collapses_duplicates") {
                                let expected = 1;
                                assert_eq!(
                                    run.return_steps[0].result.entities.len(),
                                    expected,
                                    "{} multiplicity",
                                    case.id
                                );
                            }

                            if python && matches!(outcome, PythonOutcome::ExplicitQualification) {
                                super::python_render_parity::assert_replacement(case, &run);
                            }
                            if let Some(id) = case.existing {
                                super::assert_live::assert_row(
                                    find_row(id).expect("existing semantic matrix row"),
                                    &run,
                                )
                                .unwrap();
                            }
                            results.push((
                                outputs(&run),
                                run.return_steps
                                    .iter()
                                    .map(|s| s.result.coverage)
                                    .collect::<Vec<_>>(),
                            ));
                        }
                        if matches!(
                            super::python_render_parity::python_outcome(case.id),
                            PythonOutcome::ExplicitQualification
                        ) {
                            return;
                        }
                        assert_eq!(
                            results[0], results[1],
                            "{} typed row values and order",
                            case.id
                        );
                    });
            })
            .unwrap()
            .join();
        if result.is_err() {
            failures.push(case.id);
        }
    }
    assert!(failures.is_empty(), "Python parity failures: {failures:?}");
}

#[test]
fn python_lowering_matrix_rejects_invalid_semantics_at_compile() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().id",
        "return E.query(score=10)", // RA-2: field is not a backend selection slot.
        "return E.query().select(\"absent\")",
        "return E.query().select(\"title\").where(lambda row: row.score > 0)", // RA-2 current grain.
        "return E.query().where(lambda row: row.absent == 0)",
        "return E.query().where(lambda row: row.score == \"invalid\")",
        "items = E.query()\nitems = E.query()\nreturn items",
        "return E.query().take(0)",
        "return E.query().take(-1)",
        "return E.query().take(True)",
        "return E.query().map(lambda row: row)",
        "return E.create(title=\"write\")",
        "return E.query().where(lambda row: other.score > 0)",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity)).is_err(),
            "accepted {body}"
        );
    }
}

#[test]
fn python_lowering_matrix_federated_symbols_keep_qualified_ownership() {
    let es = language_matrix::matrix_federated_duplicate_entity_session(
        language_matrix::load_language_matrix_cgs(),
    );
    let bundle=compile_python_program(&es,"class Reads(Program):\n    def build(self):\n        a = e1.query()\n        b = e2.query()\n        return a, b\n").unwrap();
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

fn write_tokens(body: &str, symbols: &plasm_core::symbol_tuning::SymbolMap, entry: &str) -> String {
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
    if body.contains("REL_LINES") {
        body = body.replace(
            "REL_LINES",
            &symbols.ident_sym_relation_for(entry, "LangItem", "lines"),
        );
    }
    body
}
#[test]
fn python_write_matrix_rejects_invalid_operands_and_receivers() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().flat_map(lambda row: row.UPDATE(title=row.absent))",
        "rows = E.query()\nreturn rows.flat_map(lambda rows: rows.DELETE())",
        "return E.query().flat_map(lambda row, other: row.DELETE())",
        "return E.query().flat_map(lambda row: row.DELETE()).DELETE()",
        "return E.query().flat_map(lambda row: E.CREATE(title=row))",
        "return E.query().flat_map(lambda row: other.DELETE())",
        "return E.query().flat_map(lambda row: row.title)",
        "return E.query().flat_map(lambda row: row.DELETE(), max_parents=2)",
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
        "return E.CREATE(title=str(123))",
    ] {
        let body = write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID);
        assert!(
            compile_python_program(&es, &program(&body, &entity)).is_err(),
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
    .is_err());
}

#[test]
fn python_write_review_identity_seals_operands_and_order() {
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
    let compile = |source: &str| compile_python_program(&es, source).unwrap();
    let original = compile(&source);
    let renamed = compile(&source.replace("MatrixProgram", "AnotherName"));
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
            &compile(&changed).artifact().comp
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

#[test]
fn python_fanout_discarded_effects_and_row_scope_are_sealed() {
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
    let bundle = compile_python_program(&es, &source).unwrap();
    let comp = &bundle.artifact().comp;
    assert!(
        !comp.steps.contains_key("_"),
        "compiler row scope must not become an executable take"
    );
    let (effect, _) = comp
        .steps
        .iter()
        .find(|(_, step)| {
            matches!(
                step,
                plasm_core::plasm_monad::PlasmStepPayload::FlatMapApply(_)
            )
        })
        .unwrap();
    let final_id = comp.bind.topo.last().unwrap();
    assert!(
        comp.bind.deps[final_id].contains(&plasm_core::plasm_monad::StepId::new(effect).unwrap())
    );
    let renamed = compile_python_program(
        &es,
        &source.replace("lambda row: row.", "lambda item: item."),
    )
    .unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        comp,
        &renamed.artifact().comp
    ));
}

#[test]
fn python_iteration_admission_and_review_seal() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let code = "seed = E.get(\"i1\")\ndone = seed.iterate(lambda row: row.UPDATE(score=1), until=lambda row: row.score >= 1, max_steps=2)\nreturn done";
    let compile = |body: &str| {
        compile_python_program(
            &es,
            &program(
                &write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID),
                &entity,
            ),
        )
    };
    let valid = compile(code).unwrap();
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
        code.replace("until=lambda row: row.score >= 1, ", ""),
        code.replace("row.score >= 1", "row.absent >= 1"),
        code.replace("row.score >= 1", "other.score >= 1"),
        code.replace("row.score >= 1", "row.score >= \"invalid\""),
        code.replace("row.score >= 1", "row.score >= 1 and row.score <= 2"),
        code.replace("row.UPDATE(score=1)", "E.get(row.id)"),
        code.replace("row.UPDATE(score=1)", "row.iterate(lambda nested: nested.UPDATE(score=1), until=lambda nested: nested.score > 1, max_steps=2)"),
    ] { assert!(compile(&bad).is_err(),"accepted {bad}"); }
    let renamed = compile(&code.replace("lambda row: row.", "lambda item: item.")).unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &valid.artifact().comp,
        &renamed.artifact().comp
    ));
    for changed in [
        code.replace("max_steps=2", "max_steps=3"),
        code.replace("row.score >= 1", "row.score >= 2"),
    ] {
        assert!(!plasm_core::plasm_monad::comp_semantic_eq(
            &valid.artifact().comp,
            &compile(&changed).unwrap().artifact().comp
        ));
    }
    let discarded = compile(
        &code
            .replace("done = seed.iterate", "seed.iterate")
            .replace("return done", "return E.get(\"i1\")"),
    )
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

#[test]
fn python_iteration_until_binding_is_a_sealed_dependency() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for operand in ["expected.score", "threshold"] {
        let scalar = if operand == "threshold" {
            "threshold = expected.score\n"
        } else {
            ""
        };
        let body = format!("expected = E.get(\"i1\")\n{scalar}seed = E.get(\"i2\")\ndone = seed.iterate(lambda row: row.PING(), until=lambda row: row.score >= {operand}, max_steps=3)\nreturn done");
        let code = program(
            &write_tokens(&body, &symbols, language_matrix::MATRIX_ENTRY_ID),
            &entity,
        );
        let bundle = compile_python_program(&es, &code).unwrap();
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
        .is_err());
    }
}

#[test]
fn python_bound_reads_reject_invalid_references() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for (plasm, python) in [
        (
            "rows = LangItem\nout = LangItem(rows.id)\nout",
            "rows = E.query()\nreturn E.get(rows.id)",
        ),
        (
            "rows = LangItem\nout = LangItem{owner=rows.owner}\nout",
            "rows = E.query()\nreturn E.query(owner=rows.owner)",
        ),
        (
            "row = LangItem(\"i1\")\nout = LangItem(row.absent)\nout",
            "row = E.get(\"i1\")\nreturn E.get(row.absent)",
        ),
        (
            "row = LangItem(\"i1\") | select title\nout = LangItem{owner=row.owner}\nout",
            "row = E.get(\"i1\").select(\"title\")\nreturn E.query(owner=row.owner)",
        ),
    ] {
        let original = compile_plasm_program(&Default::default(), None, &es, "invalid-read", plasm)
            .map_err(|e| e.to_string())
            .and_then(|bundle| {
                evaluate_plasm_comp_dry(&es, &bundle)
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            });
        assert!(
            original.is_err(),
            "original admitted through dry preflight: {plasm}"
        );
        assert!(
            compile_python_program(&es, &program(python, &entity)).is_err(),
            "Python accepted {python}"
        );
    }
    for python in [
        "return E.get(unknown)",
        "return E.query(owner=unknown)",
        "return E.get(None)",
        "return E.get([\"i1\"])",
    ] {
        assert!(
            compile_python_program(&es, &program(python, &entity)).is_err(),
            "Python accepted {python}"
        );
    }
}

#[test]
fn python_identity_admission_and_review_match_original() {
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
    for (plasm, python, entity) in [
        ("LangItem(1.5)", "return E.get(1.5)", &item),
        (
            "CompoundBranch(owner=\"alice\", name=\"main\")",
            "return E.get(owner=\"alice\", name=\"main\")",
            &compound,
        ),
        (
            "CompoundBranch(owner=\"alice\", item_id=\"i1\", name=\"main\", extra=\"x\")",
            "return E.get(owner=\"alice\", item_id=\"i1\", name=\"main\", extra=\"x\")",
            &compound,
        ),
        (
            "CompoundBranch(\"main\")",
            "return E.get(\"main\")",
            &compound,
        ),
    ] {
        assert!(
            compile_plasm_program(&Default::default(), None, &es, "invalid-identity", plasm)
                .is_err(),
            "native accepted {plasm}"
        );
        assert!(
            compile_python_program(&es, &program(python, entity)).is_err(),
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
            compile_python_program(&es, &program(body, &compound)).is_err(),
            "accepted {body}"
        );
    }
    for body in [
        "return E.get(9223372036854775808)",
        "return E.get(-9223372036854775809)",
        "return E.get(-1.5)",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &item)).is_err(),
            "accepted {body}"
        );
    }
    let compile = |body: &str| compile_python_program(&es, &program(body, &compound)).unwrap();
    let a = compile("return E.get(owner=\"alice\", item_id=\"i1\", name=\"main\")");
    let b = compile("return E.get(name=\"main\", item_id=\"i1\", owner=\"alice\")");
    let changed = compile("return E.get(owner=\"bob\", item_id=\"i1\", name=\"main\")");
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &a.artifact().comp,
        &b.artifact().comp
    ));
    assert!(!plasm_core::plasm_monad::comp_semantic_eq(
        &a.artifact().comp,
        &changed.artifact().comp
    ));
}

#[test]
fn python_relations_reject_invalid_scope_and_preserve_symbols() {
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
        "items = E.query()\nreturn items.flat_map(lambda items: items.tags)",
        "return E.query().flat_map(lambda row: row.tags.lines)",
    ] {
        assert!(
            compile_python_program(&es, &program(body, &entity)).is_err(),
            "accepted {body}"
        );
    }
    let relation =
        symbols.ident_sym_relation_for(language_matrix::MATRIX_ENTRY_ID, "LangItem", "tags");
    let compile = |body: &str| compile_python_program(&es, &program(body, &entity)).unwrap();
    let wire = compile("items = E.query().take(2)\nreturn items.flat_map(lambda row: row.tags)");
    let tuned = compile(&format!(
        "items = E.query().take(2)\nreturn items.flat_map(lambda item: item.{relation})"
    ));
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &wire.artifact().comp,
        &tuned.artifact().comp
    ));
    let scalar = compile("one = E.get(\"i1\")\nreturn one.tags");
    let tuned_scalar = compile(&format!("one = E.get(\"i1\")\nreturn one.{relation}"));
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &scalar.artifact().comp,
        &tuned_scalar.artifact().comp
    ));
}

#[test]
fn python_relation_symbols_keep_federated_source_ownership() {
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
    let foreign_native = format!("parent = e1(\"i1\")\nchildren = parent.{relation}\nchildren");
    assert!(
        compile_plasm_program(
            &Default::default(),
            None,
            &es,
            "foreign-relation",
            &foreign_native
        )
        .is_err(),
        "original syntax must reject a foreign relation symbol too"
    );
    let source = format!("class Relations(Program):\n    def build(self):\n        parent = e2.get(\"i1\")\n        children = parent.{relation}\n        return children\n");
    let bundle = compile_python_program(&es, &source).unwrap();
    let comp = serde_json::to_value(&bundle.artifact().comp).unwrap();
    let relation =
        super::ir_helpers::comp_relation_named(&comp, "children").expect("typed relation payload");
    assert_eq!(
        relation["target"]["entry_id"],
        language_matrix::MATRIX_FED_B
    );
    assert_eq!(relation["source"], "parent");
    assert!(
        compile_python_program(&es, &source.replace("e2.get", "e1.get")).is_err(),
        "foreign relation symbol must not be reinterpreted in the other catalog"
    );
}

#[test]
fn python_ordering_rejects_invalid_fields_and_arguments() {
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
            compile_python_program(&es, &program(body, &entity)).is_err(),
            "accepted {body}"
        );
    }
}

#[test]
fn python_projection_aliases_reject_invalid_grain_and_arguments() {
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
            compile_python_program(&es, &program(body, &entity)).is_err(),
            "accepted {body}"
        );
    }
}

#[test]
fn python_set_operations_reject_invalid_row_shapes() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for body in [
        "return E.query().select(\"owner\").union(E.query().select(\"title\"))",
        "return E.query().union([1, 2])",
        "return E.query().select(\"owner\").union()",
        "rhs = E.query().select(\"id\", \"owner\")\nreturn E.query().where(lambda row: row.owner in rhs)",
        "return E.query().where(lambda row: row.owner in [\"alice\"])",
        "return E.query().where(lambda row: row.owner in missing)",
        "row = E.query().select(\"owner\")\nreturn E.query().where(lambda row: row.owner in row)",
        "rhs = E.query().select(\"owner\")\nreturn E.query().select(\"title\").where(lambda row: row.owner in rhs)",
        "rhs = E.query().select(\"id\", \"owner\")\nreturn rhs.union(rhs).tags",
    ] { assert!(compile_python_program(&es, &program(body, &entity)).is_err(), "accepted {body}"); }
}

#[test]
fn python_inline_membership_requires_closed_read_rowsets() {
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
        "return E.query().where(lambda row: row.owner in E.get(\"i1\").owner)",
    ] {
        let body = write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID);
        assert!(compile_python_program(&es, &program(&body, &entity)).is_err(), "accepted {body}");
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

#[test]
fn python_reductions_reject_invalid_descriptors_and_grain() {
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
            compile_python_program(&es, &program(body, &entity)).is_err(),
            "accepted {body}"
        );
    }
}

#[test]
fn python_reduction_schema_preserves_derived_key_and_value_types() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    for (native, python) in [(
        "groups = LangItem | summarize by owner n=count()\nregrouped = groups | summarize by n original=first(n)\nregrouped",
        "groups = E.query().group_by(\"owner\", n=agg.count())\nregrouped = groups.group_by(\"n\", original=agg.first(\"n\"))\nreturn regrouped",
    )] {
        let bundles = [
            compile_plasm_program(&Default::default(), None, &es, "schema-test", native).unwrap(),
            compile_python_program(&es, &program(python, &entity)).unwrap(),
        ];
        for bundle in bundles {
            let comp = serde_json::to_value(&bundle.artifact().comp).unwrap();
            let fields = comp["steps"]["regrouped"]["compute"]["schema"]["fields"].as_array().unwrap();
            assert_eq!(fields.len(), 2);
            for field in fields { assert_eq!(field["value_kind"], if field["name"] == "n" { "integer" } else { "number" }, "{field}"); }
        }
    }
}

#[test]
fn python_text_compute_seals_code_inputs_and_rejects_hidden_dependencies() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = |expression: &str, input: &str| {
        format!(
        "class Text(Program):\n    @compute\n    def report(self, rows: list[Value[{entity}]]) -> str:\n        return {expression}\n    def build(self):\n        one = {input}\n        text = self.report(one)\n        return text\n"
    )
    };
    let input = format!("{entity}.get(\"i1\")");
    let compile =
        |expression: &str| compile_python_program(&es, &source(expression, &input)).unwrap();
    let first = compile("f'item={rows[0].title}'");
    let changed = compile("f'changed={rows[0].title}'");
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
        "f'{rows}'",
        "f'{rows[0].__class__}'",
        "'{}'.format(rows[0])",
        "f'{open(\"secret\")}'",
        "f'{rows[0].title:{hidden}}'",
    ] {
        assert!(
            compile_python_program(&es, &source(expression, &input)).is_err(),
            "accepted {expression}"
        );
    }
    let projected = format!("{entity}.get(\"i1\").select(\"id\")");
    assert!(compile_python_program(&es, &source("f'{rows[0].title}'", &projected)).is_err());
    let projected = format!("{entity}.get(\"i1\").select(\"title\")");
    assert!(compile_python_program(&es, &source("f'{rows[0].title}'", &projected)).is_ok());
}

#[test]
fn python_inferred_rows_check_types_and_preserve_plural_cardinality() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let source = |expression: &str, input: &str, result: &str| {
        format!(
        "class Text(Program):\n    @compute\n    def report(self, row: Row) -> str:\n        return {expression}\n    def build(self):\n        rows = {input}\n        text = self.report(rows)\n        return {result}\n"
    )
    };
    let input = format!("{entity}.query().group_by(\"title\", n=agg.count())");
    let valid = source("f'{row.title}: {row.n:04d}'", &input, "text");
    let compiled = compile_python_program(&es, &valid).unwrap();
    let comp = serde_json::to_value(&compiled.artifact().comp).unwrap();
    assert_eq!(comp["steps"]["text"]["compute"]["op"]["per_row"], true);
    for corruption in 0..5 {
        let mut wire = serde_json::to_value(&compiled.artifact().comp).unwrap();
        let steps = &mut wire["steps"];
        match corruption {
            0 => steps["text"]["compute"]["op"]["per_row"] = serde_json::json!(false),
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
        "str(row)",
        "open('secret')",
    ] {
        assert!(
            compile_python_program(&es, &source(expression, &input, "text")).is_err(),
            "accepted {expression}"
        );
    }
    assert!(
        compile_python_program(&es, &source("f'{row.n}'", &input, "text.content")).is_err(),
        "plural render became scalar"
    );
    let single = format!("{entity}.query().aggregate(n=agg.count())");
    assert!(compile_python_program(&es, &source("f'{row.n}'", &single, "text.content")).is_ok());
    let projected = format!("{entity}.query().take(2).select(renamed=\"title\")");
    assert!(compile_python_program(&es, &source("row.renamed", &projected, "text")).is_ok());
    assert!(compile_python_program(&es, &source("row.title", &projected, "text")).is_err());
}

#[test]
fn python_plasm_dag_prompt_examples_compile_against_the_matrix() {
    let prompt = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../plasm-core/src/prompt_render/assets/python-plasm-dag.txt"
    ));
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let mut count = 0;
    for block in prompt.split("```python\n").skip(1) {
        let (source, _) = block.split_once("```").expect("closed Python example");
        let source = source.replace("e1", &entity);
        compile_python_program(&es, &source)
            .unwrap_or_else(|e| panic!("prompt example {count} fails admission: {e}\n{source}"));
        count += 1;
    }
    assert!(
        count >= 2,
        "prompt must teach the root and compute boundary with complete examples"
    );
}

#[path = "python_value_contract.rs"]
mod value_contract_matrix;

#[test]
fn python_teaching_card_signatures_compile_against_fixture() {
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
    assert_eq!(wave.capabilities.len(), 4);
    assert!(wave.capabilities.iter().all(|c| c.unavailable.is_none()));
    let symbols = exposure.to_symbol_map();
    let item = symbols.entity_sym_for("fixture", "Item");
    let tag = symbols.entity_sym_for("fixture", "Tag");
    let touch = symbols.method_sym_for("fixture", "Item", "item_touch");
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
        ("def get(cls, identity:", format!("{item}.get('i0')")),
        ("def query(cls) ->", format!("{item}.query()")),
        (
            "def query(cls, *, item_id:",
            format!("{tag}.query(item_id='i0')"),
        ),
        (&format!("def {touch}(cls)"), format!("{item}.{touch}()")),
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
            .unwrap_or_else(|e| panic!("advertised {expression}: {e}"));
    }
}

#[path = "python_union.rs"]
pub(super) mod union_matrix;

#[test]
fn python_signed_literals_preserve_numeric_domains() {
    let es = language_matrix::matrix_execute_session(language_matrix::load_language_matrix_cgs());
    let symbols = es.teaching_exposure.as_ref().unwrap().to_symbol_map();
    let entity = symbols.entity_sym_for(language_matrix::MATRIX_ENTRY_ID, "LangItem");
    let compile = |body: &str| {
        compile_python_program(
            &es,
            &program(
                &write_tokens(body, &symbols, language_matrix::MATRIX_ENTRY_ID),
                &entity,
            ),
        )
    };
    for value in ["-7", "+7", "-9223372036854775808", "+9223372036854775807"] {
        for body in [
            format!("return E.CREATE(title=\"signed\", owner=\"bot\", score={value})"),
            format!("return E.query().where(lambda row: row.score > {value})"),
        ] {
            compile(&body).unwrap_or_else(|e| panic!("rejected {body}: {e}"));
        }
    }
    for value in [
        "-9223372036854775809",
        "+9223372036854775808",
        "-1e999",
        "+1e999",
        "-True",
        "+False",
        "-1j",
    ] {
        let body = format!("return E.CREATE(title=\"signed\", owner=\"bot\", score={value})");
        assert!(compile(&body).is_err(), "accepted {body}");
    }
    // Unary plus is only spelling: the reviewed semantic DAG must be identical.
    let plain = compile("return E.CREATE(title=\"signed\", owner=\"bot\", score=7)").unwrap();
    let positive = compile("return E.CREATE(title=\"signed\", owner=\"bot\", score=+7)").unwrap();
    assert!(plasm_core::plasm_monad::comp_semantic_eq(
        &plain.artifact().comp,
        &positive.artifact().comp
    ));
}
