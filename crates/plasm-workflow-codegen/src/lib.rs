//! genco-backed TypeScript contract emission for workflow MCP Apps.

use genco::lang::js;
use genco::prelude::*;

const GENERATED_HEADER: &str = "// @generated - do not edit. Regenerate: cargo run -p plasm-workflow-codegen --bin plasm-gen-workflow-ts\n\n";

pub fn emit_contracts_ts() -> String {
    let tokens: js::Tokens = quote! {
        export type PlanUxLayout = "sequential" | "parallel_columns" | "branches";

        export type PlanUxWidgetKind =
            | "read_surface"
            | "relation_hop"
            | "render_template"
            | "compute"
            | "action_surface"
            | "data"
            | "derive"
            | "for_each"
            | "other";

        export interface PlanUxColumn {
            entry_id: string;
            label: string;
            step_ids: string[];
        }

        export interface PlanUxStep {
            id: string;
            ordinal: number;
            widget: PlanUxWidgetKind;
            entry_id?: string;
            entity?: string;
            qualified_entity?: string;
            operation: string;
            effect_class: string;
            approval_gate: boolean;
            layout_hint?: string;
            headline?: string;
        }

        export interface PlanUxEdge {
            from: string;
            to: string;
        }

        export interface PlanUxParamBinding {
            param_name: string;
            step_id: string;
            bind_kind?: string;
        }

        export interface PlanUxReview {
            verdict: string;
            warnings?: string;
            write_count: number;
            read_count: number;
        }

        export type PlanUxFlowVerdict = "clean" | "needs_review" | "denied";

        export type PlanUxFlowDisposition = "allow" | "approve" | "review" | "deny";

        export interface PlanUxFlowCounts {
            allow: number;
            approve: number;
            review: number;
            deny: number;
        }

        export interface PlanUxFlowSink {
            param: string;
            sink_class?: string;
        }

        export interface PlanUxFlowViolation {
            node_id: string;
            headline?: string;
            reason: string;
            labels: string[];
            sink?: PlanUxFlowSink;
        }

        export interface PlanUxFlowApproval {
            operation: string;
            policy_key: string;
            reason?: string;
        }

        export interface PlanUxFlowStep {
            id: string;
            ordinal: number;
            disposition: PlanUxFlowDisposition;
            labels_in: string[];
            labels_out: string[];
            sink?: PlanUxFlowSink;
            approval?: PlanUxFlowApproval;
        }

        export interface PlanUxDataClassInfo {
            severity: string;
            description?: string;
        }

        export interface PlanUxFlowReflection {
            schema_version: number;
            verdict: PlanUxFlowVerdict;
            policy_revision?: number;
            counts: PlanUxFlowCounts;
            catalog_has_labels: boolean;
            data_classes?: Record<string, PlanUxDataClassInfo>;
            violations: PlanUxFlowViolation[];
            trace: PlanUxFlowStep[];
        }

        export interface PlanUxReflection {
            schema_version: number;
            layout: PlanUxLayout;
            columns?: PlanUxColumn[];
            steps: PlanUxStep[];
            edges?: PlanUxEdge[];
            returns?: string[];
            writes?: string[];
            review: PlanUxReview;
            param_bindings?: PlanUxParamBinding[];
            live?: {
                running_step_id?: string;
                completed_step_ids?: string[];
            };
            flow: PlanUxFlowReflection;
        }

        export interface WorkflowSeed {
            entry_id: string;
            entity: string;
        }

        export interface WorkflowFieldView {
            name: string;
            description: string;
            required: boolean;
            wire_type: string;
            bind_kind: string;
            entry_id: string;
            entity: string;
        }

        export interface WorkflowViewModel {
            schema_version: number;
            id: string;
            title: string;
            description: string;
            seeds: WorkflowSeed[];
            fields: WorkflowFieldView[];
            warnings?: string[];
            ready: boolean;
            blocking_errors?: string[];
        }

        export interface InstantiateResponse {
            program: string;
        }
    };
    format!(
        "{GENERATED_HEADER}{}",
        tokens.to_file_string().expect("emit contracts.ts")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_ts_contains_plan_ux_reflection() {
        let out = emit_contracts_ts();
        assert!(out.contains("PlanUxReflection"));
        assert!(out.contains("@generated"));
        assert!(out.contains("ready: boolean"));
        assert!(out.contains("headline?: string"));
        assert!(out.contains("blocking_errors"));
    }

    #[test]
    fn contracts_ts_contains_plan_ux_flow_reflection() {
        let out = emit_contracts_ts();
        assert!(out.contains("PlanUxFlowReflection"));
        assert!(out.contains("PlanUxFlowVerdict"));
        assert!(out.contains("PlanUxFlowDisposition"));
        assert!(out.contains("flow: PlanUxFlowReflection;"));
        assert!(out.contains("catalog_has_labels: boolean"));
        assert!(out.contains("PlanUxDataClassInfo"));
    }
}
