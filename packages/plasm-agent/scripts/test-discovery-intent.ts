import assert from "node:assert/strict";
import { DiscoveryIntent } from "../src/runtime/discovery-intent.js";

const intent = new DiscoveryIntent();
const original = 'Update records owned by my colleagues, before Friday.\nKeep "archived" records unchanged.';
const focus = "Find records and update them";
intent.addRequest(original, false);
const payload = () => intent.forCapabilityRequest(focus);
assert.deepEqual(payload(), { userRequests: [original], intent: focus });
// An extension retains the original constraint and the user's subsequent revision.
const correction = "Include archived records after all; only those owned by Alex.";
intent.addRequest(correction, false);
assert.deepEqual(payload().userRequests, [original, correction]);
// Repeated discoveries do not change or duplicate user-authored context.
assert.deepEqual(intent.forCapabilityRequest(focus), intent.forCapabilityRequest(focus));
const detached = intent.forCapabilityRequest(focus);
detached.userRequests.length = 0;
assert.equal(payload().userRequests.length, 2);
// A separate workflow cannot inherit the preceding workflow's constraints.
intent.addRequest("Read my current status", true);
assert.deepEqual(payload().userRequests, ["Read my current status"]);
console.log("Discovery intent preserves user requests, revisions, and workflow reset.");
