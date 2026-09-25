// NL to a complete statically compiled Python DAG.
class PlasmPlan {
  text string @description("Python source: one class derived from Program with build(self) and an explicit return. No fences or prose.")
  reasoning string @description("Brief rationale.")
}
class PlanChatTurn {
  role "user" | "assistant"
  content string
}
function TranslatePlan(messages: PlanChatTurn[]) -> PlasmPlan {
  client Sonnet
  prompt #"
    {{ _.role("system") }}
    Satisfy the CURRENT goal with one complete Python Program using the supplied Plasm library reference and incremental domain declarations. The final user message contains --- GOAL --- and optional --- CORRECTION FEEDBACK ---. Earlier turns provide context, not bindings available to the new program.
    Use only supplied session symbols, signatures, wire fields and typed operations. Build declares a deferred DAG; bounded @compute methods process materialized values. No imports, arbitrary outer Python execution, or invented methods. Return source only in text, preserving Python indentation and literals. Repair using the supplied diagnostics without dropping selection criteria or blindly repeating completed writes.
    {% for m in messages %}
    {{ _.role(m.role) }}
    {{ m.content }}
    {% endfor %}
    {{ ctx.output_format }}
  "#
}
