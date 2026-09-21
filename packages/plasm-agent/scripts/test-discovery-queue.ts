import assert from "node:assert/strict";
import { DiscoveryQueue } from "../src/runtime/discovery-queue.js";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

const queue = new DiscoveryQueue();
const entered = deferred<void>();
const release = deferred<void>();
const order: string[] = [];
const failure = new Error("discovery failed");
const first = queue.run("session-a", "first", async () => {
  order.push("first");
  entered.resolve();
  await release.promise;
  throw failure;
});
const failed = assert.rejects(first, error => error === failure);
await entered.promise;
const second = queue.run("session-a", "second", async () => {
  order.push("second");
  return "fresh snapshot";
});
const duplicate = queue.run("session-a", "second", async () => {
  assert.fail("identical pending work must not run twice");
});
assert.equal(second, duplicate);
assert.equal(await queue.run("session-b", "second", async () => "independent"), "independent");
assert.deepEqual(order, ["first"], "other sessions progress while the same session waits");
release.resolve();
await failed;
assert.equal(await second, "fresh snapshot");
assert.deepEqual(order, ["first", "second"]);
assert.equal(await queue.run("session-a", "second", async () => "new observation"), "new observation");

// Generated request sequences exercise repeated keys, multiple independent
// sessions, failures and every completion boundary, without a new dependency.
for (let seed = 1; seed <= 64; seed++) {
  let random = seed;
  const active = new Set<string>();
  const calls = new Map<string, number>();
  const requests = Array.from({ length: 80 }, () => {
    random = (Math.imul(random, 1664525) + 1013904223) >>> 0;
    return JSON.parse(JSON.stringify({ session: `s${random % 4}`, request: `r${(random >>> 4) % 9}` })) as { session: string; request: string };
  });
  const results = await Promise.allSettled(requests.map(({ session, request }) =>
    queue.run(session, request, async () => {
      assert.ok(!active.has(session), "a session cannot have overlapping discovery");
      active.add(session);
      const key = JSON.stringify([session, request]);
      calls.set(key, (calls.get(key) ?? 0) + 1);
      await Promise.resolve();
      active.delete(session);
      if (request === "r0") throw failure;
      return key;
    })));
  assert.equal(active.size, 0);
  assert.equal(calls.size, new Set(requests.map(r => JSON.stringify([r.session, r.request]))).size);
  assert.ok([...calls.values()].every(count => count === 1));
  results.forEach((result, index) => {
    const request = requests[index]!;
    if (request.request === "r0") {
      assert.equal(result.status, "rejected");
    } else {
      assert.equal(result.status, "fulfilled");
      if (result.status === "fulfilled") assert.equal(result.value, JSON.stringify([request.session, request.request]));
    }
  });
}
console.log("discovery-queue: independence, coalescing, failure release and 64 generated schedules passed");
