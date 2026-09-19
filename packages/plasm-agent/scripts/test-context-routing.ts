#!/usr/bin/env node
import assert from "node:assert/strict";
import { routingPacketSchema, routingRecoveryMarkdown } from "../src/engine/routing.js";

const packet = routingPacketSchema.parse({
  routing: {
    intent: "Read the account balance.",
    pin_id: "11111111-1111-4111-8111-111111111111",
    retrieval: { generation: "fixture", candidates: [] },
    matching: {
      slots: [{ id: "s0", statement: "Read the account balance." }],
      matches: [],
      complete: false,
      unmatched_slot_ids: ["s0"],
      additional_capability_ids: [],
    },
    input_source_projection: [],
    input_source_matching: { matches: [], selected: [] },
    closure: null,
    recovery: {
      unmatched_slots: [{
        slot_id: "s0", statement: "Read the account balance.",
        admitted_candidate_ids: [],
      }],
      available_catalogs: [{ entry_id: "matrix", description: "Abstract fixture catalog" }],
      guidance: "Extend discovery when another capability is needed.",
    },
  },
  teaching: null,
});

const markdown = routingRecoveryMarkdown(packet.routing);
assert.ok(markdown?.includes("Unmatched affirmative effect slots"));
assert.ok(markdown?.includes("s0"));

const candidate = {
  id: "matrix:Contact:query",
  reference: { catalog: "matrix", capability: "Contact:query" },
  document: { entity: "Contact" },
};
const valid = {
  routing: {
    ...packet.routing,
    retrieval: { generation: "fixture", candidates: [candidate] },
    matching: {
      slots: packet.routing.matching.slots,
      matches: [{
        slot_id: "s0",
        capability_id: candidate.id,
        choice: "direct_match" as const,
        probabilities: { direct_match: 1, does_not_match: 0, uncertain: 0 },
        confidence: 1,
      }],
      complete: true,
      unmatched_slot_ids: [],
      additional_capability_ids: [candidate.id],
    },
    recovery: null,
  },
  teaching: null,
};
routingPacketSchema.parse(valid);

assert.throws(() =>
  routingPacketSchema.parse({
    routing: {
      ...valid.routing,
      matching: {
        ...valid.routing.matching,
        matches: valid.routing.matching.matches.map((entry) => ({
          ...entry,
          choice: "does_not_match" as const,
          probabilities: { direct_match: 0, does_not_match: 1, uncertain: 0 },
        })),
        complete: true,
        unmatched_slot_ids: [],
      },
    },
    teaching: null,
  }),
  /completion fields contradict match choices/,
);
console.log("context-routing: direct matching, typed input sources, and host recovery passed");
